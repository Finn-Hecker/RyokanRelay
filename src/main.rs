//! Zero-knowledge relay server for multiplayer character chat.
//!
//! Design in one paragraph: axum + tokio, one process, all state in RAM
//! behind a single `Mutex<HashMap<RoomId, Room>>`. Every participant owns a
//! bounded mpsc queue; broadcasting is a non-blocking `try_send` fan-out, so
//! the room lock is never held across an `.await` and one slow client can
//! only ever get *itself* disconnected. Message content is client-side
//! encrypted and opaque to this process — it relays bytes, verifies the host
//! token, tracks presence and the generation lock, and nothing else. There is
//! no persistence of any kind and no logging of payloads.

mod protocol;
mod state;
mod ws;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tower_http::cors::CorsLayer;
use tracing::info;

use state::{AppState, Config, Room};

pub type SharedState = Arc<AppState>;

/// Room codes: short, human-typable, no ambiguous characters (0/O, 1/I).
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 6;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "relay_server=info".into()),
        )
        .init();

    let cfg = Config::from_env();
    let port = cfg.port;
    let state: SharedState = Arc::new(AppState::new(cfg));

    tokio::spawn(gc_task(Arc::clone(&state)));

    let app = Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/rooms", post(create_room))
        .route("/ws/:room_id", get(ws::ws_handler))
        // Dev-friendly CORS; lock this down to your frontend origin in prod.
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    info!(%addr, "relay server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server error");
}

#[derive(Serialize)]
struct CreateRoomResponse {
    room_id: String,
    /// Returned exactly once. The server keeps only a SHA-256 hash of it.
    host_token: String,
}

async fn create_room(State(state): State<SharedState>) -> Json<CreateRoomResponse> {
    let mut rng = rand::thread_rng();

    let token_bytes: [u8; 32] = rng.gen();
    let host_token = URL_SAFE_NO_PAD.encode(token_bytes);
    let host_token_hash: [u8; 32] = Sha256::digest(host_token.as_bytes()).into();

    let mut rooms = state.rooms.lock().unwrap();
    let room_id = loop {
        let candidate: String = (0..CODE_LEN)
            .map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char)
            .collect();
        if !rooms.contains_key(&candidate) {
            break candidate;
        }
    };
    rooms.insert(room_id.clone(), Room::new(host_token_hash));
    drop(rooms);

    info!(room = %room_id, "room created");
    Json(CreateRoomResponse {
        room_id,
        host_token,
    })
}

/// Garbage collection: rooms whose host is not connected (never joined, e.g.
/// because the creator closed the tab before entering, or guests waiting on a
/// host link that will never arrive) are expired after `room_ttl`. Rooms with
/// a connected host live until the host leaves — host loss itself tears the
/// room down immediately via the connection lifecycle, not via GC.
async fn gc_task(state: SharedState) {
    let ttl = state.cfg.room_ttl;
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tick.tick().await;
        let mut rooms = state.rooms.lock().unwrap();
        let expired: Vec<String> = rooms
            .iter()
            .filter(|(_, r)| r.host_id.is_none() && r.created_at.elapsed() > ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            ws::close_room(&mut rooms, &id, "expired", protocol::close::IDLE_TIMEOUT);
        }
    }
}
