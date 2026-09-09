//! Wire protocol between client and relay server.
//!
//! Everything here is *metadata only*. Message content travels exclusively in
//! `Relay.p` as an opaque, client-side-encrypted base64 string that the server
//! never inspects, parses or logs.

use serde::{Deserialize, Serialize};

/// Messages the client sends to the server (JSON text frames).
#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Must be the first frame after the WebSocket upgrade.
    /// `host_token` is the *authorization* secret (the server MUST see and
    /// verify it). It is distinct from the room encryption key, which never
    /// reaches the server.
    Hello {
        #[serde(default)]
        host_token: Option<String>,
        #[serde(default)]
        guest_token: Option<String>,
    },
    /// Opaque encrypted payload, relayed verbatim to all other participants.
    Relay { p: String },
    /// Request the generation lock ("Jetzt antworten").
    GenStart,
    /// Release the generation lock (host only; generation finished/aborted).
    GenEnd,
    /// Change who may trigger generation (host only).
    Policy { everyone: bool },
}

/// Messages the server sends to clients.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome {
        you: u64,
        role: &'static str, // "host" | "guest"
        count: usize,
        locked_by: Option<u64>,
        everyone_can_generate: bool,
    },
    Joined {
        id: u64,
        count: usize,
    },
    Left {
        id: u64,
        count: usize,
    },
    Relay {
        from: u64,
        p: String,
    },
    Locked {
        by: u64,
    },
    Unlocked,
    Policy {
        everyone: bool,
    },
    /// Terminal event: the room is gone. Sent right before the server closes
    /// every remaining connection, so UIs can show a real reason instead of
    /// just hanging.
    RoomClosed {
        reason: &'static str, // "host_left" | "expired"
    },
    Error {
        code: &'static str,
    },
}

impl ServerMsg {
    pub fn to_json(&self) -> String {
        // Serialization of these enums cannot fail.
        serde_json::to_string(self).expect("serialize ServerMsg")
    }
}

/// WebSocket close codes (application range 4000+).
pub mod close {
    pub const HOST_LEFT: u16 = 4001;
    pub const IDLE_TIMEOUT: u16 = 4008;
    pub const BAD_HOST_TOKEN: u16 = 4403;
    pub const ROOM_NOT_FOUND: u16 = 4404;
    pub const HOST_ALREADY_CONNECTED: u16 = 4409;
    pub const PROTOCOL_ERROR: u16 = 4400;
    pub const SLOW_CONSUMER: u16 = 4413;
    pub const ROOM_FULL: u16 = 4429;
}
