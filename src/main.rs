use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension, Query,
    },
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use futures::{Future, SinkExt, StreamExt};
use rand::rngs::ThreadRng;
use tokio::sync::{mpsc::error::TryRecvError, RwLock};

use std::{
    borrow::BorrowMut,
    collections::HashMap,
    error::Error,
    fmt::Display,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// everything in here lifts heavily from this example:
// https://github.com/tokio-rs/axum/blob/main/examples/chat/src/main.rs

struct User {
    is_host: bool,
    name: String,
}

struct Room {
    code: u32,
}

struct AppState {
    web_page_visits: AtomicU32,
    magic_counter: AtomicU32,

    player_id_counter: AtomicU32,

    rooms: RwLock<
        HashMap<
            u32,
            (
                tokio::sync::mpsc::Sender<RoomMessage>,
                tokio::sync::broadcast::Receiver<RoomMessage>,
            ),
        >,
    >,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "buzzer=trace".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let shared_state = Arc::new(AppState {
        web_page_visits: AtomicU32::new(0),
        magic_counter: AtomicU32::new(0),
        player_id_counter: AtomicU32::new(1000),
        rooms: RwLock::new(HashMap::new()),
    });

    let task_state = shared_state.clone();
    tokio::spawn(async move {
        loop {
            task_state.magic_counter.fetch_add(1, Ordering::SeqCst);
            //tracing::debug!("Tick...");
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await
        }
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/buzzer", get(websocket_handler))
        .layer(Extension(shared_state));

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    tracing::debug!("listening on {}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn handler(Extension(state): Extension<Arc<AppState>>) -> String {
    let visits = state.web_page_visits.fetch_add(1, Ordering::SeqCst) + 1;

    return format!("This site has been visited {} times", visits);
}

async fn websocket_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
    Extension(app_state): Extension<Arc<AppState>>,
) -> impl IntoResponse {
    // lil debugger helper :)
    let stringified_params = params
        .iter()
        .map(|(a, b)| format!("{} -> {}", a, b))
        .collect::<Vec<_>>()
        .join(" -- ");
    tracing::debug!("params: {}", stringified_params);

    let room_hint: String = params.get("room").unwrap_or(&String::from("none")).clone();

    ws.on_upgrade(move |socket| handle_participant(socket, app_state, room_hint))
}

enum ParticipantEvent {
    FromBrowser(String),
    FromRoom(RoomMessage),
    NoAction,
}

async fn handle_participant(stream: WebSocket, app_state: Arc<AppState>, room_hint: String) {
    let (mut to_browser, mut from_browser) = stream.split();

    tracing::info!("Room hint received: {}", room_hint);

    let (to_room, mut from_room) = match room_hint.as_ref() {
        "new" => {
            // create a new room and join it
            use tokio::sync::broadcast;
            use tokio::sync::mpsc;
            // broadcast tx/rx pair, brx is to be cloned by all participants and will drive their receiving event loop
            // while btx is for this room to send messages out to everyone
            let (btx, mut brx) = broadcast::channel::<RoomMessage>(100);
            // stx is to be cloned for individual participants to send back into the room
            let (stx, mut srx) = mpsc::channel::<RoomMessage>(100);

            // spawn the room
            {
                let stx = stx.clone();
                let btx = btx.clone();
                tokio::spawn(async { start_room(btx, srx, stx, 666).await });
            }

            // store the tx/rx channels for future participants
            let mut app_state_rooms = app_state.rooms.write().await;
            app_state_rooms.insert(666, (stx.clone(), btx.subscribe()));

            (stx.clone(), btx.subscribe())
        }
        _ => {
            // parse the room hint as a u32 and try to find & join that room
            let code = room_hint.parse::<u32>().unwrap_or(0);
            let room_map = app_state.rooms.read().await;
            let (stx, btx) = room_map.get(&code).unwrap();

            (stx.clone(), btx.resubscribe())
        }
    };

    let my_id = app_state.player_id_counter.fetch_add(1, Ordering::SeqCst);

    to_room
        .send(RoomMessage::ParticipantJoined(my_id))
        .await
        .unwrap();

    loop {
        // wait on either a message from the browser or from the room broadcast
        let event: ParticipantEvent = tokio::select! {
            Some(Ok(ws_message)) = from_browser.next() => {
                if let Message::Text(message) = ws_message {
                    ParticipantEvent::FromBrowser(message)
                } else {
                    ParticipantEvent::NoAction
                }
            },
            Ok(room_message) = from_room.recv() => {
                ParticipantEvent::FromRoom(room_message)
            }
        };

        // handling the events inside of a macro is cumbersome for IDE reasons so wrapping them in varaints
        // and pulling the mout here is a big maintenance help
        match event {
            ParticipantEvent::FromBrowser(message) => {
                let op: ClientOperation = serde_json::from_str(message.as_ref()).unwrap();
                tracing::info!("Received op: {:?}", op);

                match op.op.as_ref() {
                    "set_name" => {
                        to_room
                            .send(RoomMessage::ParticipantSetName(my_id, op.value))
                            .await
                            .unwrap();
                    }
                    _ => panic!("unknown operation {}", op.op),
                };
            }
            ParticipantEvent::FromRoom(room_message) => {
                if let Err(_) = match room_message {
                    RoomMessage::Ping => to_browser.send(Message::Pong("!".into())).await,
                    RoomMessage::NewRoomState(new_state) => {
                        to_browser
                            .send(Message::Text(String::from(new_state.as_ref())))
                            .await
                    }
                    _ => Ok(()),
                } {
                    break; // quit the main loop so we can clean up and leave as eleganty as possible
                }
            }
            ParticipantEvent::NoAction => {}
        }
    }

    tracing::info!("Cleaning up player {}", my_id);
    // clean up on our way out
    to_room
        .send(RoomMessage::ParticipantQuit(my_id))
        .await
        .unwrap();
}

#[derive(Clone, Debug)]
enum RoomMessage {
    Ping, // not for use by room participant tasks but also totally w/e if it does happen
    ParticipantJoined(u32),
    ParticipantQuit(u32),
    ParticipantSetName(u32, String),
    NewRoomState(Arc<String>),
}

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct ClientOperation {
    op: String,
    value: String,
}

#[derive(Serialize)]
struct Participant {
    name: String,
}

#[derive(Serialize)]
struct RoomState {
    room_id: u32,
    participants: HashMap<u32, Participant>,
}

async fn start_room(
    btx: tokio::sync::broadcast::Sender<RoomMessage>,
    mut srx: tokio::sync::mpsc::Receiver<RoomMessage>,
    stx: tokio::sync::mpsc::Sender<RoomMessage>,
    room_id: u32,
) {
    let ping_tx = stx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            ping_tx.send(RoomMessage::Ping).await.unwrap();
        }
    });

    let mut room_state = RoomState {
        participants: HashMap::new(),
        room_id: room_id,
    };

    loop {
        // drain all messages clients may have sent into the pending vector
        // wait on either our heartbeat timer or a message to come in

        let should_emit_state = match srx.recv().await {
            Some(RoomMessage::Ping) => {
                //tracing::info!("Sending pings");
                btx.send(RoomMessage::Ping).unwrap();
                false
            }
            Some(RoomMessage::ParticipantJoined(id)) => {
                tracing::info!("Person joined! Id: {}", id);
                room_state.participants.insert(
                    id,
                    Participant {
                        name: String::from("New Player"),
                    },
                );
                true
            }
            Some(RoomMessage::ParticipantQuit(id)) => {
                tracing::info!("Person quit! Id: {}", id);
                room_state.participants.remove(&id);
                true
            }
            Some(RoomMessage::ParticipantSetName(id, new_name)) => {
                tracing::info!("Set name: {} -> {}", id, new_name);
                if let Some(player) = room_state.participants.get_mut(&id) {
                    player.name = new_name;
                }
                true
            }
            Some(RoomMessage::NewRoomState(_)) => false, // these are meant for clients
            None => {
                panic!("Not handling this yet");
            }
        };

        if should_emit_state {
            let state_emission = serde_json::to_string(&room_state).unwrap();
            btx.send(RoomMessage::NewRoomState(Arc::new(state_emission)))
                .unwrap();
        }
    }
}

// Include utf-8 file at **compile** time.
async fn index() -> Html<&'static str> {
    Html(std::include_str!("../buzz.html"))
}
