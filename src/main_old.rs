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
use tokio::sync::RwLock;

use std::{
    borrow::BorrowMut,
    collections::HashMap,
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
    rooms: RwLock<HashMap<u32, Arc<RwLock<Room>>>>,
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

    let app_state_clone = app_state.clone();
    let room_hint: String = params.get("room").unwrap_or(&String::from("none")).clone();

    ws.on_upgrade(move |socket| handle_join(socket, app_state, room_hint))
}

async fn create_room(app_state: &Arc<AppState>) -> Arc<RwLock<Room>> {
    use rand::Rng;

    let mut room_map = app_state.rooms.write().await;

    let mut attempts: u8 = 0;

    loop {
        let room_code: u32 = {
            let mut trng = ThreadRng::default();
            trng.gen_range(1000..9999)
        };

        if !room_map.contains_key(&room_code) {
            let new_room = Arc::new(RwLock::new(Room { code: room_code }));
            room_map.insert(room_code, new_room.clone());

            let room_state = new_room.clone();
            tokio::spawn(async move { room_task(room_state).await });

            return new_room;
        }

        attempts += 1;
        if attempts > 100 {
            panic!("Couldn't create a new room! This is unexpected and we should increase our key space or use a smarter room generator.")
        }
        // keep seeking an unused room code
    }
}

async fn room_task(state: Arc<RwLock<Room>>) {
    let code = { state.read().await.code };

    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(5000)).await;
        tracing::info!("Room {} is thinking...", code)
    }
}

async fn handle_join(stream: WebSocket, app_state: Arc<AppState>, room_hint: String) {
    let (mut sender, mut receiver) = stream.split();

    let room = {
        if room_hint.eq("new") {
            create_room(&app_state.clone()).await
        } else {
            let code = room_hint.parse::<u32>().unwrap_or(0);
            let room_map = app_state.rooms.read().await;
            room_map.get(&code).unwrap().clone()
        }
    };

    let actual_room_code = room.read().await.code;
    tracing::info!("Joined room {}", actual_room_code);

    loop {
        sender.send(Message::Pong("hi".into())).await.unwrap();
        // sender
        //     .send(Message::Text(String::from(r#"{ "msg": "hey" }"#)))
        //     .await
        //     .unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await
    }
}

// Include utf-8 file at **compile** time.
async fn index() -> Html<&'static str> {
    Html(std::include_str!("../buzz.html"))
}
