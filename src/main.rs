use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension,
    },
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use futures::{SinkExt, StreamExt};

use std::{
    borrow::BorrowMut,
    collections::HashMap,
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

struct AppState {
    web_page_visits: AtomicU32,
    magic_counter: AtomicU32,
    //rooms: Mutex<HashMap<u32, User>>,
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
        //rooms: Mutex::new(HashMap::new()),
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
    Extension(state): Extension<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| websocket(socket, state))
}

async fn websocket(stream: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = stream.split();

    loop {
        sender
            .send(Message::Text(String::from("Tock...")))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await
    }
}

// Include utf-8 file at **compile** time.
async fn index() -> Html<&'static str> {
    Html(std::include_str!("../buzz.html"))
}
