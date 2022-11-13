use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension, Query,
    },
    http::header,
    response::AppendHeaders,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use futures::{SinkExt, StreamExt};
use tokio::sync::RwLock;

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// everything in here lifts heavily from this example:
// https://github.com/tokio-rs/axum/blob/main/examples/chat/src/main.rs

struct AppState {
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
        player_id_counter: AtomicU32::new(1000),
        rooms: RwLock::new(HashMap::new()),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/buzzer", get(websocket_handler))
        .route("/media/bewoop.mp3", get(doot_sound))
        .route("/buzzer.ico", get(doot_sound))
        .layer(Extension(shared_state));

    //let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    let addr = SocketAddr::from(([0, 0, 0, 0], 7777));
    tracing::debug!("listening on {}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
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
            let (btx, _brx) = broadcast::channel::<RoomMessage>(100);
            // stx is to be cloned for individual participants to send back into the room
            let (stx, srx) = mpsc::channel::<RoomMessage>(100);

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

    // inform the client of their ID in the room, this is one of the few messages we send directly
    let authority_msg = serde_json::to_string(&InformAuthority { your_id: my_id }).unwrap();
    to_browser.send(Message::Text(authority_msg)).await.unwrap();

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
        // and pulling them out here is a big maintenance help
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
                    "buzzer_hit" => {
                        to_room
                            .send(RoomMessage::ParticipantBuzzed(my_id))
                            .await
                            .unwrap();
                    }
                    "reset_buzzers" => {
                        to_room
                            .send(RoomMessage::ResetBuzzers(my_id))
                            .await
                            .unwrap();
                    }
                    "set_buzzer_lock" => {
                        let lock_state = match op.value.as_ref() {
                            "true" => true,
                            "false" => false,
                            _ => false,
                        };

                        to_room
                            .send(RoomMessage::SetLockState(my_id, lock_state))
                            .await
                            .unwrap();
                    }
                    "set_mode" => {
                        let new_mode_opt = match op.value.as_ref() {
                            "first_to_buzz" => Some(RoomMode::FirstToBuzz),
                            "buzz_race" => Some(RoomMode::BuzzerRace),
                            _ => None,
                        };

                        if let Some(new_mode) = new_mode_opt {
                            to_room
                                .send(RoomMessage::SetRoomMode(my_id, new_mode))
                                .await
                                .unwrap();
                        }
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
    ParticipantBuzzed(u32),
    NewRoomState(Arc<String>),
    ResetBuzzers(u32),
    SetLockState(u32, bool),
    SetRoomMode(u32, RoomMode),
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

#[derive(Serialize, Deserialize, Clone, Debug)]
enum RoomMode {
    FirstToBuzz, // first one to successfully buzz transitions us to locked
    BuzzerRace,  // everyone can buzz, and the order will be presented
}

#[derive(Serialize)]
struct RoomState {
    room_id: u32,
    participants: HashMap<u32, Participant>,
    room_captain: Option<u32>,
    room_mode: RoomMode,
    room_locked: bool,
    activated_buzzers: Vec<u32>,
}

#[derive(Serialize)]
struct InformAuthority {
    your_id: u32,
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
        room_captain: None,
        room_mode: RoomMode::FirstToBuzz,
        room_locked: false,
        activated_buzzers: vec![],
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

                // check if we should assign this person the leader as they're
                // the only one here
                if room_state.participants.len() == 1 {
                    room_state.room_captain = Some(id);
                }

                true
            }
            Some(RoomMessage::ParticipantQuit(id)) => {
                tracing::info!("Person quit! Id: {}", id);
                room_state.participants.remove(&id);

                // check if someone new should become captain
                if let Some(captain_id) = room_state.room_captain {
                    if id == captain_id {
                        // new leader must be chosen!.. if there's anyone left
                        if room_state.participants.len() > 0 {
                            room_state.room_captain =
                                Some(room_state.participants.iter().next().unwrap().0.clone());
                        } else {
                            room_state.room_captain = None;
                        }
                    }
                    // no action to take
                } else {
                    // no leaders!
                    room_state.room_captain = None;
                }

                // remove them from the buzz list if they're in it
                if let Some(pos) = room_state.activated_buzzers.iter().position(|&r| r == id) {
                    room_state.activated_buzzers.remove(pos);
                }

                true
            }
            Some(RoomMessage::ParticipantSetName(id, new_name)) => {
                tracing::info!("Set name: {} -> {}", id, new_name);
                if let Some(player) = room_state.participants.get_mut(&id) {
                    player.name = new_name;
                }
                true
            }
            Some(RoomMessage::ParticipantBuzzed(id)) => {
                let buzzers = &mut room_state.activated_buzzers;

                // we take no action if the room is locked
                // we ignore buzzers that are already active
                let needs_update: bool = if !buzzers.contains(&id) && !room_state.room_locked {
                    match room_state.room_mode {
                        RoomMode::FirstToBuzz => {
                            // only the first person to buzz gets to buzz, and the room locks
                            // immediately upon doing so
                            if buzzers.is_empty() {
                                buzzers.push(id);
                                room_state.room_locked = true;
                                true
                            } else {
                                false
                            }
                        }
                        RoomMode::BuzzerRace => {
                            // everyone can buzz!

                            // conveniently, hashset insert returns true if the value was actually added and wasnt a dupe
                            if buzzers.contains(&id) {
                                false
                            } else {
                                buzzers.push(id);
                                true
                            }
                        }
                    }
                } else {
                    false // no state change needed
                };

                needs_update
            }
            Some(RoomMessage::ResetBuzzers(id)) => {
                // only captains can reset buzzers
                if let Some(captain_id) = room_state.room_captain {
                    if captain_id == id {
                        room_state.activated_buzzers.clear();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            Some(RoomMessage::SetLockState(id, state)) => {
                if let Some(captain_id) = room_state.room_captain {
                    if captain_id == id {
                        room_state.room_locked = state;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            Some(RoomMessage::SetRoomMode(id, new_mode)) => {
                if let Some(captain_id) = room_state.room_captain {
                    if captain_id == id {
                        room_state.room_mode = new_mode;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            Some(RoomMessage::NewRoomState(_)) => false, // these are meant for clients
            None => {
                panic!("Not handling this yet");
            }
        };

        if should_emit_state {
            let state_emission = serde_json::to_string(&room_state).unwrap();
            tracing::debug!("New room state: {}", &state_emission);
            btx.send(RoomMessage::NewRoomState(Arc::new(state_emission)))
                .unwrap();
        }
    }
}

// Include utf-8 file at **compile** time.
async fn index() -> Html<&'static str> {
    Html(std::include_str!("../buzz.html"))
}

async fn doot_sound() -> &'static [u8] {
    std::include_bytes!("../deployed_media/bewoop.mp3")
}

async fn favico() -> &'static [u8] {
    std::include_bytes!("../deployed_media/ffxiv-logo.ico")
}
