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

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header::CACHE_CONTROL, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
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

    let static_dir = std::env::var("WEB_DIR").unwrap_or_else(|_| "web".into());
    let index = std::path::Path::new(&static_dir).join("index.html");

    let app = Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/rooms", post(create_room))
        .route("/ws/:room_id", get(ws::ws_handler))
        // Statische Dateien; Fallback auf index.html, damit "/?mp=CODE#k=..." funktioniert.
        .fallback_service(ServeDir::new(&static_dir).fallback(ServeFile::new(index)))
        .layer(CorsLayer::permissive())
        .layer(middleware::from_fn(cache_headers))
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    info!(%addr, "relay server listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
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
    /// Relay-visible admission token. It is deliberately distinct from the
    /// end-to-end encryption key, which is generated only by the host client.
    guest_token: String,
}

async fn create_room(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    State(state): State<SharedState>,
) -> Result<Json<CreateRoomResponse>, StatusCode> {
    let client_ip = if state.cfg.trust_proxy {
        headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(peer.ip())
    } else {
        peer.ip()
    };
    if !state.allow_room_create(client_ip) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    let mut rng = rand::thread_rng();

    let token_bytes: [u8; 32] = rng.gen();
    let host_token = URL_SAFE_NO_PAD.encode(token_bytes);
    let host_token_hash: [u8; 32] = Sha256::digest(host_token.as_bytes()).into();
    let guest_token_bytes: [u8; 32] = rng.gen();
    let guest_token = URL_SAFE_NO_PAD.encode(guest_token_bytes);
    let guest_token_hash: [u8; 32] = Sha256::digest(guest_token.as_bytes()).into();

    let mut rooms = state.rooms.lock().unwrap();
    if rooms.len() >= state.cfg.max_rooms {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let room_id = loop {
        let candidate: String = (0..CODE_LEN)
            .map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char)
            .collect();
        if !rooms.contains_key(&candidate) {
            break candidate;
        }
    };
    rooms.insert(
        room_id.clone(),
        Room::new(host_token_hash, guest_token_hash),
    );
    drop(rooms);

    info!(room = %room_id, "room created");
    Ok(Json(CreateRoomResponse {
        room_id,
        host_token,
        guest_token,
    }))
}

async fn cache_headers(request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    let value = if path.starts_with("/assets/") {
        HeaderValue::from_static("public, max-age=31536000, immutable")
    } else if path.starts_with("/api/") {
        HeaderValue::from_static("no-store")
    } else {
        // Revalidate HTML and other entry files so a fingerprinted new build
        // is discovered without users clearing their browser cache.
        HeaderValue::from_static("no-cache")
    };
    response.headers_mut().insert(CACHE_CONTROL, value);
    response
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
