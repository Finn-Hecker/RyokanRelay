//! WebSocket connection lifecycle.
//!
//! Flow per connection:
//!   1. HTTP upgrade on `GET /ws/:room_id`
//!   2. First text frame must be `hello` (optionally carrying the host token,
//!      which the server hashes and verifies — this is the *authorization*
//!      secret and deliberately visible to the server, unlike the room
//!      encryption key which never leaves the clients).
//!   3. Reader task dispatches frames; writer task drains a bounded queue and
//!      enforces ping/idle timeouts.
//!   4. Any exit path funnels into `disconnect()`, which either announces a
//!      `left` event (guest) or tears down the whole room (host).

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::protocol::{close, ClientMsg, ServerMsg};
use crate::state::{Outbound, Participant, Room, CLIENT_QUEUE};
use crate::SharedState;

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const PING_INTERVAL: Duration = Duration::from_secs(10);
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(room_id): Path<String>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    State(state): State<SharedState>,
) -> Response {
    if !valid_room_id(&room_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let client_ip = effective_client_ip(peer.ip(), &headers, state.cfg.trust_proxy);
    let Some(permit) = state.try_open_connection(client_ip) else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let max_message_size = state.cfg.max_message_size;
    ws.max_message_size(max_message_size)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            handle_socket(socket, state, room_id).await;
        })
        .into_response()
}

fn effective_client_ip(
    peer_ip: std::net::IpAddr,
    headers: &HeaderMap,
    trust_proxy: bool,
) -> std::net::IpAddr {
    if !trust_proxy {
        return peer_ip;
    }
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(peer_ip)
}

fn valid_room_id(room_id: &str) -> bool {
    room_id.len() == 6
        && room_id
            .bytes()
            .all(|b| b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(&b))
}

async fn handle_socket(mut socket: WebSocket, state: SharedState, room_id: String) {
    // ---- Phase 1: hello ---------------------------------------------------
    let hello = match read_hello(&mut socket).await {
        Ok(h) => h,
        Err(()) => {
            close_with(&mut socket, close::PROTOCOL_ERROR).await;
            return;
        }
    };

    // ---- Phase 2: registration -------------------------------------------
    let (host_token, guest_token) = hello;
    let (my_id, is_host, rx, last_seen) = match register(
        &state,
        &room_id,
        host_token.as_deref(),
        guest_token.as_deref(),
    ) {
        Ok(v) => v,
        Err(code) => {
            close_with(&mut socket, code).await;
            return;
        }
    };

    info!(room = %room_id, id = my_id, host = is_host, "participant joined");

    let (sink, stream) = socket.split();

    let idle_limit = if is_host {
        state.cfg.host_idle
    } else {
        state.cfg.guest_idle
    };

    let writer = tokio::spawn(writer_task(sink, rx, Arc::clone(&last_seen), idle_limit));

    // ---- Phase 3: read loop, raced against the writer ---------------------
    let reader = read_loop(stream, &state, &room_id, my_id, is_host, &last_seen);
    tokio::pin!(reader);
    let mut writer = writer;
    tokio::select! {
        _ = &mut reader => {}
        _ = &mut writer => {}
    }
    writer.abort();

    // ---- Phase 4: cleanup (exactly once, all exit paths) ------------------
    disconnect(&state, &room_id, my_id);
    info!(room = %room_id, id = my_id, host = is_host, "participant left");
}

/// Reads the mandatory first `hello` frame. Returns the presented host token.
async fn read_hello(socket: &mut WebSocket) -> Result<(Option<String>, Option<String>), ()> {
    let deadline = tokio::time::timeout(HELLO_TIMEOUT, async {
        while let Some(Ok(msg)) = socket.recv().await {
            match msg {
                Message::Text(txt) => return Some(txt),
                Message::Ping(_) | Message::Pong(_) => continue,
                _ => return None,
            }
        }
        None
    });
    let Ok(Some(txt)) = deadline.await else {
        return Err(());
    };
    match serde_json::from_str::<ClientMsg>(&txt) {
        Ok(ClientMsg::Hello {
            host_token,
            guest_token,
        }) => Ok((host_token, guest_token)),
        _ => Err(()),
    }
}

type Registered = (u64, bool, mpsc::Receiver<Outbound>, Arc<StdMutex<Instant>>);

fn register(
    state: &SharedState,
    room_id: &str,
    host_token: Option<&str>,
    guest_token: Option<&str>,
) -> Result<Registered, u16> {
    let mut rooms = state.rooms.lock().unwrap();
    let room = rooms.get_mut(room_id).ok_or(close::ROOM_NOT_FOUND)?;
    if room.closing {
        return Err(close::ROOM_NOT_FOUND);
    }

    let is_host = match host_token {
        Some(token) => {
            if token.len() != 43 {
                return Err(close::BAD_HOST_TOKEN);
            }
            let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            if !room.verify_host_token(&hash) {
                return Err(close::BAD_HOST_TOKEN);
            }
            if room.host_id.is_some() {
                return Err(close::HOST_ALREADY_CONNECTED);
            }
            true
        }
        None => {
            let Some(token) = guest_token.filter(|token| token.len() == 43) else {
                return Err(close::BAD_HOST_TOKEN);
            };
            let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            if !room.verify_guest_token(&hash) {
                return Err(close::BAD_HOST_TOKEN);
            }
            false
        }
    };

    if room.count() >= state.cfg.max_participants_per_room {
        return Err(close::ROOM_FULL);
    }

    let id = room.next_id;
    room.next_id += 1;
    let (tx, rx) = mpsc::channel(CLIENT_QUEUE);
    room.participants.insert(id, Participant { tx, is_host });
    if is_host {
        room.host_id = Some(id);
    }

    let count = room.count();
    room.send_to(
        id,
        &ServerMsg::Welcome {
            you: id,
            role: if is_host { "host" } else { "guest" },
            count,
            locked_by: room.locked_by,
            everyone_can_generate: room.everyone_can_generate,
        },
    );
    let dead = room.broadcast(&ServerMsg::Joined { id, count }, Some(id));
    if purge_dead(room, dead) {
        close_room(&mut rooms, room_id, "host_left", close::HOST_LEFT);
        // Room is gone, but the new participant's rx already carries the
        // room_closed frame, so let the normal lifecycle deliver it.
    }

    let last_seen = Arc::new(StdMutex::new(Instant::now()));
    Ok((id, is_host, rx, last_seen))
}

async fn read_loop(
    mut stream: SplitStream<WebSocket>,
    state: &SharedState,
    room_id: &str,
    my_id: u64,
    is_host: bool,
    last_seen: &Arc<StdMutex<Instant>>,
) {
    let mut rate_started = Instant::now();
    let mut frames_in_window = 0_u32;
    let mut bytes_in_window = 0_usize;
    while let Some(Ok(msg)) = stream.next().await {
        if rate_started.elapsed() >= state.cfg.rate_window {
            rate_started = Instant::now();
            frames_in_window = 0;
            bytes_in_window = 0;
        }
        frames_in_window = frames_in_window.saturating_add(1);
        bytes_in_window = bytes_in_window.saturating_add(message_len(&msg));
        if frames_in_window > state.cfg.max_frames_per_window
            || bytes_in_window > state.cfg.max_bytes_per_window
        {
            info!(room = %room_id, id = my_id, "connection rate limited");
            break;
        }
        *last_seen.lock().unwrap() = Instant::now();
        let txt = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue, // pings/pongs/binary noise
        };
        let Ok(parsed) = serde_json::from_str::<ClientMsg>(&txt) else {
            debug!(room = %room_id, id = my_id, "unparseable frame dropped");
            continue;
        };
        match parsed {
            ClientMsg::Hello { .. } => break,
            ClientMsg::Relay { p } => handle_relay(state, room_id, my_id, is_host, p),
            ClientMsg::GenStart => handle_gen_start(state, room_id, my_id, is_host),
            ClientMsg::GenEnd => handle_gen_end(state, room_id, my_id, is_host),
            ClientMsg::Policy { everyone } => {
                handle_policy(state, room_id, my_id, is_host, everyone)
            }
        }
    }
}

fn message_len(message: &Message) -> usize {
    match message {
        Message::Text(text) => text.len(),
        Message::Binary(bytes) | Message::Ping(bytes) | Message::Pong(bytes) => bytes.len(),
        Message::Close(_) => 0,
    }
}

fn handle_relay(state: &SharedState, room_id: &str, my_id: u64, is_host: bool, p: String) {
    let mut rooms = state.rooms.lock().unwrap();
    let Some(room) = rooms.get_mut(room_id) else {
        return;
    };
    // While the generation lock is held, only the host may inject frames
    // (it is streaming the encrypted LLM answer / snapshots); everyone else
    // is muted, including whoever triggered the generation.
    if room.locked_by.is_some() && !is_host {
        room.send_to(my_id, &ServerMsg::Error { code: "locked" });
        return;
    }
    let dead = room.broadcast(&ServerMsg::Relay { from: my_id, p }, Some(my_id));
    if purge_dead(room, dead) {
        close_room(&mut rooms, room_id, "host_left", close::HOST_LEFT);
    }
}

fn handle_gen_start(state: &SharedState, room_id: &str, my_id: u64, is_host: bool) {
    let lock_timeout = state.cfg.lock_timeout;
    let mut rooms = state.rooms.lock().unwrap();
    let Some(room) = rooms.get_mut(room_id) else {
        return;
    };
    if room.locked_by.is_some() {
        room.send_to(
            my_id,
            &ServerMsg::Error {
                code: "already_locked",
            },
        );
        return;
    }
    if !(is_host || room.everyone_can_generate) {
        room.send_to(
            my_id,
            &ServerMsg::Error {
                code: "not_allowed",
            },
        );
        return;
    }
    room.locked_by = Some(my_id);
    room.lock_seq += 1;
    let seq = room.lock_seq;
    let dead = room.broadcast(&ServerMsg::Locked { by: my_id }, None);
    if purge_dead(room, dead) {
        close_room(&mut rooms, room_id, "host_left", close::HOST_LEFT);
        return;
    }
    drop(rooms);

    // Watchdog for the smaller failure mode "host is connected but the LLM
    // call never finishes": auto-unlock after a deadline so the room does not
    // stay muted forever. Deliberately separate from host-loss handling.
    let state = Arc::clone(state);
    let room_id = room_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(lock_timeout).await;
        let mut rooms = state.rooms.lock().unwrap();
        if let Some(room) = rooms.get_mut(&room_id) {
            if room.locked_by.is_some() && room.lock_seq == seq {
                room.locked_by = None;
                let dead = room.broadcast(&ServerMsg::Unlocked, None);
                if purge_dead(room, dead) {
                    close_room(&mut rooms, &room_id, "host_left", close::HOST_LEFT);
                }
            }
        }
    });
}

fn handle_gen_end(state: &SharedState, room_id: &str, my_id: u64, is_host: bool) {
    let mut rooms = state.rooms.lock().unwrap();
    let Some(room) = rooms.get_mut(room_id) else {
        return;
    };
    if !is_host {
        room.send_to(
            my_id,
            &ServerMsg::Error {
                code: "not_allowed",
            },
        );
        return;
    }
    if room.locked_by.is_none() {
        return;
    }
    room.locked_by = None;
    room.lock_seq += 1; // invalidate the watchdog
    let dead = room.broadcast(&ServerMsg::Unlocked, None);
    if purge_dead(room, dead) {
        close_room(&mut rooms, room_id, "host_left", close::HOST_LEFT);
    }
}

fn handle_policy(state: &SharedState, room_id: &str, my_id: u64, is_host: bool, everyone: bool) {
    let mut rooms = state.rooms.lock().unwrap();
    let Some(room) = rooms.get_mut(room_id) else {
        return;
    };
    if !is_host {
        room.send_to(
            my_id,
            &ServerMsg::Error {
                code: "not_allowed",
            },
        );
        return;
    }
    room.everyone_can_generate = everyone;
    let dead = room.broadcast(&ServerMsg::Policy { everyone }, None);
    if purge_dead(room, dead) {
        close_room(&mut rooms, room_id, "host_left", close::HOST_LEFT);
    }
}

/// Removes a participant after its connection ended. Guests produce a `left`
/// event; losing the host tears down the entire room — a deliberate design
/// decision, since history and character card only exist on the host.
fn disconnect(state: &SharedState, room_id: &str, id: u64) {
    let mut rooms = state.rooms.lock().unwrap();
    let Some(room) = rooms.get_mut(room_id) else {
        return;
    };
    let Some(p) = room.participants.remove(&id) else {
        return;
    };
    if p.is_host {
        close_room(&mut rooms, room_id, "host_left", close::HOST_LEFT);
        return;
    }
    // If the leaving guest held the generation lock, release it. The host
    // (executor) will notice the unlock and abort its stream client-side.
    if room.locked_by == Some(id) {
        room.locked_by = None;
        room.lock_seq += 1;
        let dead = room.broadcast(&ServerMsg::Unlocked, None);
        if purge_dead(room, dead) {
            close_room(&mut rooms, room_id, "host_left", close::HOST_LEFT);
            return;
        }
    }
    let count = room.count();
    let dead = room.broadcast(&ServerMsg::Left { id, count }, None);
    if purge_dead(room, dead) {
        close_room(&mut rooms, room_id, "host_left", close::HOST_LEFT);
    }
}

/// Drops participants whose outbound queue overflowed (slow consumers),
/// announcing each departure. Returns true if the host was among them.
fn purge_dead(room: &mut Room, mut dead: Vec<u64>) -> bool {
    let mut host_died = false;
    while let Some(id) = dead.pop() {
        if let Some(p) = room.participants.remove(&id) {
            if p.is_host {
                host_died = true;
            }
            let _ = p.tx.try_send(Outbound::Close(close::SLOW_CONSUMER));
            let count = room.count();
            let more = room.broadcast(&ServerMsg::Left { id, count }, None);
            dead.extend(more);
        }
    }
    host_died
}

/// Tears down a room: informs every remaining participant why, queues a close
/// frame, and forgets the room entirely. Nothing survives.
pub fn close_room(
    rooms: &mut HashMap<String, Room>,
    room_id: &str,
    reason: &'static str,
    code: u16,
) {
    let Some(mut room) = rooms.remove(room_id) else {
        return;
    };
    room.closing = true;
    let msg = ServerMsg::RoomClosed { reason }.to_json();
    for (_, p) in room.participants.drain() {
        let _ = p.tx.try_send(Outbound::Msg(msg.clone()));
        let _ = p.tx.try_send(Outbound::Close(code));
    }
    info!(room = %room_id, reason, "room closed");
}

async fn writer_task(
    mut sink: SplitSink<WebSocket, Message>,
    mut rx: mpsc::Receiver<Outbound>,
    last_seen: Arc<StdMutex<Instant>>,
    idle_limit: Duration,
) {
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            item = rx.recv() => match item {
                Some(Outbound::Msg(json)) => {
                    if sink.send(Message::Text(json)).await.is_err() {
                        break;
                    }
                }
                Some(Outbound::Close(code)) => {
                    let _ = sink
                        .send(Message::Close(Some(CloseFrame { code, reason: "".into() })))
                        .await;
                    break;
                }
                None => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            },
            _ = ping.tick() => {
                let idle = last_seen.lock().unwrap().elapsed();
                if idle > idle_limit {
                    let _ = sink
                        .send(Message::Close(Some(CloseFrame {
                            code: close::IDLE_TIMEOUT,
                            reason: "".into(),
                        })))
                        .await;
                    break;
                }
                if sink.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn close_with(socket: &mut WebSocket, code: u16) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: "".into(),
        })))
        .await;
}
