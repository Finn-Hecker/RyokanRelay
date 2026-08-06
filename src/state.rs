//! In-memory room state. Nothing in this module is ever written to disk,
//! a database or an external cache; dropping the process drops everything.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use subtle::ConstantTimeEq;
use tokio::sync::mpsc;

use crate::protocol::ServerMsg;

/// Per-connection outbound queue size. If a client can't keep up and the
/// queue fills, only *that* client is disconnected (backpressure isolation).
pub const CLIENT_QUEUE: usize = 256;

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
    pub fn new(host_token_hash: [u8; 32]) -> Self {
        Self {
            host_token_hash,
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
        }
    }
}

pub struct AppState {
    pub rooms: Mutex<HashMap<String, Room>>,
    pub cfg: Config,
}

impl AppState {
    pub fn new(cfg: Config) -> Self {
        Self {
            rooms: Mutex::new(HashMap::new()),
            cfg,
        }
    }
}
