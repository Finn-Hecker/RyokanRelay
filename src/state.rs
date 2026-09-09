//! In-memory room state. Nothing in this module is ever written to disk,
//! a database or an external cache; dropping the process drops everything.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use subtle::ConstantTimeEq;
use tokio::sync::mpsc;

use crate::protocol::ServerMsg;

/// Per-connection outbound queue size. If a client can't keep up and the
/// queue fills, only *that* client is disconnected (backpressure isolation).
pub const CLIENT_QUEUE: usize = 16;

/// A frame queued for delivery to one participant.
#[derive(Debug)]
pub enum Outbound {
    Msg(String),
    /// Close the connection with this code after flushing.
    Close(u16),
}

pub struct Participant {
    pub tx: mpsc::Sender<Outbound>,
    pub is_host: bool,
}

pub struct Room {
    /// SHA-256 hash of the host token. The plaintext token is returned exactly
    /// once at creation time and never stored server-side.
    pub host_token_hash: [u8; 32],
    /// Hash of the bearer token required for guest admission. This token is
    /// not an encryption key and reveals no session content to the relay.
    pub guest_token_hash: [u8; 32],
    pub created_at: Instant,
    pub next_id: u64,
    pub participants: HashMap<u64, Participant>,
    pub host_id: Option<u64>,
    pub locked_by: Option<u64>,
    /// Monotonic counter, bumped on every lock; lets the lock watchdog detect
    /// that "its" lock is still the active one.
    pub lock_seq: u64,
    pub everyone_can_generate: bool,
    /// Set while the room is being torn down, so racing handlers back off.
    pub closing: bool,
}

impl Room {
    pub fn new(host_token_hash: [u8; 32], guest_token_hash: [u8; 32]) -> Self {
        Self {
            host_token_hash,
            guest_token_hash,
            created_at: Instant::now(),
            next_id: 1,
            participants: HashMap::new(),
            host_id: None,
            locked_by: None,
            lock_seq: 0,
            everyone_can_generate: false,
            closing: false,
        }
    }

    pub fn verify_host_token(&self, presented_hash: &[u8; 32]) -> bool {
        self.host_token_hash.ct_eq(presented_hash).into()
    }

    pub fn verify_guest_token(&self, presented_hash: &[u8; 32]) -> bool {
        self.guest_token_hash.ct_eq(presented_hash).into()
    }

    pub fn count(&self) -> usize {
        self.participants.len()
    }

    /// Queue a message for every participant except `except`. Participants
    /// whose queue is full are collected and removed by the caller.
    pub fn broadcast(&mut self, msg: &ServerMsg, except: Option<u64>) -> Vec<u64> {
        let json = msg.to_json();
        let mut dead = Vec::new();
        for (id, p) in &self.participants {
            if Some(*id) == except {
                continue;
            }
            if p.tx.try_send(Outbound::Msg(json.clone())).is_err() {
                dead.push(*id);
            }
        }
        dead
    }

    /// Queue a message to a single participant.
    pub fn send_to(&self, id: u64, msg: &ServerMsg) {
        if let Some(p) = self.participants.get(&id) {
            let _ = p.tx.try_send(Outbound::Msg(msg.to_json()));
        }
    }
}

/// Runtime configuration, read once from the environment at startup.
#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    /// Idle limit for the host connection. Deliberately stricter than for
    /// guests: losing the host kills the room, so we want to detect a dead
    /// host connection quickly instead of leaving everyone in limbo.
    pub host_idle: Duration,
    pub guest_idle: Duration,
    /// Auto-unlock deadline for a stuck generation (host connected, but the
    /// LLM call never completes).
    pub lock_timeout: Duration,
    /// Rooms whose host never (re)connected are garbage-collected after this.
    pub room_ttl: Duration,
    pub max_rooms: usize,
    pub max_connections: usize,
    pub max_connections_per_ip: usize,
    pub max_participants_per_room: usize,
    pub max_message_size: usize,
    pub max_frames_per_window: u32,
    pub max_bytes_per_window: usize,
    pub rate_window: Duration,
    pub room_creates_per_window: u32,
    /// Honor X-Forwarded-For only when the relay is reachable exclusively
    /// through a trusted reverse proxy.
    pub trust_proxy: bool,
}

impl Config {
    pub fn from_env() -> Self {
        fn secs(key: &str, default: u64) -> Duration {
            Duration::from_secs(
                std::env::var(key)
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(default),
            )
        }
        Self {
            port: std::env::var("PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8787),
            host_idle: secs("HOST_IDLE_SECS", 30),
            guest_idle: secs("GUEST_IDLE_SECS", 90),
            lock_timeout: secs("LOCK_TIMEOUT_SECS", 180),
            room_ttl: secs("ROOM_TTL_SECS", 900),
            max_rooms: number("MAX_ROOMS", 256),
            max_connections: number("MAX_CONNECTIONS", 512),
            max_connections_per_ip: number("MAX_CONNECTIONS_PER_IP", 24),
            max_participants_per_room: number("MAX_PARTICIPANTS_PER_ROOM", 12),
            max_message_size: number("MAX_MESSAGE_BYTES", 1024 * 1024),
            max_frames_per_window: number("MAX_FRAMES_PER_WINDOW", 80),
            max_bytes_per_window: number("MAX_BYTES_PER_WINDOW", 2 * 1024 * 1024),
            rate_window: secs("RATE_WINDOW_SECS", 10),
            room_creates_per_window: number("ROOM_CREATES_PER_WINDOW", 10),
            trust_proxy: std::env::var("TRUST_PROXY")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        }
    }
}

fn number<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[derive(Default)]
struct IpAbuseState {
    connections: usize,
    room_window_started: Option<Instant>,
    room_creates: u32,
}

pub struct AppState {
    pub rooms: Mutex<HashMap<String, Room>>,
    pub cfg: Config,
    active_connections: AtomicUsize,
    abuse_by_ip: Mutex<HashMap<IpAddr, IpAbuseState>>,
}

impl AppState {
    pub fn new(cfg: Config) -> Self {
        Self {
            rooms: Mutex::new(HashMap::new()),
            cfg,
            active_connections: AtomicUsize::new(0),
            abuse_by_ip: Mutex::new(HashMap::new()),
        }
    }

    pub fn try_open_connection(self: &Arc<Self>, ip: IpAddr) -> Option<ConnectionPermit> {
        let previous = self.active_connections.fetch_add(1, Ordering::AcqRel);
        if previous >= self.cfg.max_connections {
            self.active_connections.fetch_sub(1, Ordering::AcqRel);
            return None;
        }

        let mut abuse = self.abuse_by_ip.lock().unwrap();
        let entry = abuse.entry(ip).or_default();
        if entry.connections >= self.cfg.max_connections_per_ip {
            self.active_connections.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        entry.connections += 1;
        Some(ConnectionPermit {
            state: Arc::clone(self),
            ip,
        })
    }

    pub fn allow_room_create(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut abuse = self.abuse_by_ip.lock().unwrap();
        let entry = abuse.entry(ip).or_default();
        let expired = entry
            .room_window_started
            .map(|start| now.duration_since(start) >= self.cfg.rate_window)
            .unwrap_or(true);
        if expired {
            entry.room_window_started = Some(now);
            entry.room_creates = 0;
        }
        if entry.room_creates >= self.cfg.room_creates_per_window {
            return false;
        }
        entry.room_creates += 1;
        true
    }
}

pub struct ConnectionPermit {
    state: Arc<AppState>,
    ip: IpAddr,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.state.active_connections.fetch_sub(1, Ordering::AcqRel);
        let mut abuse = self.state.abuse_by_ip.lock().unwrap();
        if let Some(entry) = abuse.get_mut(&self.ip) {
            entry.connections = entry.connections.saturating_sub(1);
            if entry.connections == 0
                && entry
                    .room_window_started
                    .map(|start| start.elapsed() >= self.state.cfg.rate_window)
                    .unwrap_or(true)
            {
                abuse.remove(&self.ip);
            }
        }
    }
}
