# Zero Knowledge Relay Server

A lightweight WebSocket relay for multiplayer character chat. The server only forwards encrypted, opaque frames between clients. It has no disk, no database and no cache, and it never logs message content. On SIGTERM, all state is wiped.

## How it works

Built with axum and tokio. Axum handles the WebSocket upgrade and routing cleanly, and tokio's async runtime fits a workload with many connections and light CPU use per frame.

All rooms live in a single Mutex<HashMap<RoomId, Room>>. The lock is only held for a quick, allocation free broadcast step with no awaiting, so a shared map works fine at this scale (hundreds of rooms, dozens of participants each). Sharding would be premature.

Each connection has a bounded queue of 16 frames. If a client falls behind, only that client is disconnected (close code 4413). Everyone else is unaffected apart from a "left" event.

There are two separate secrets:

- The room key (AES-GCM, kept in the URL fragment) never reaches the server. It exists purely for confidentiality between clients.
- The host token is an authorization secret the server does need to check. The server only stores a SHA-256 hash of it and compares it in constant time. The plain token only ever exists on the server for the moment of verification.
- The guest token is a separate relay-visible bearer token required to enter a room. It prevents room-code guessing from consuming participant slots, but is not an encryption key and cannot decrypt content.

If the host disconnects, the room ends immediately. All remaining clients get a "room_closed" message with reason "host_left" and close code 4001. Hosts have a shorter idle timeout (30s) than guests (90s), since losing the host has bigger consequences.

## Generation lock

The host can always start a generation. Guests can only do so if the room policy is set to "everyone", which only the host can change. While locked, everyone except the host is muted, and the host streams the encrypted response.

A stalled generation and a host disconnect are handled separately. If a generation stalls, a watchdog releases the lock automatically after LOCK_TIMEOUT_SECS (default 180s), without closing the room. If the guest who triggered it leaves, the lock is released too, and the host stops streaming.

## Protocol

Client to server: hello (with optional host_token, must be the first message), relay, gen_start, gen_end, policy.

Server to client: welcome, joined, left, relay, locked, unlocked, policy, room_closed, error.

The payload field p is always base64url(iv + AES-GCM ciphertext). The server never parses or logs it.

Close codes: 4001 host left, 4008 idle, 4400 protocol error, 4403 invalid host token, 4404 room not found, 4409 host already connected, 4413 slow consumer.

## Running it

```bash
cargo run --release
```

Listens on 0.0.0.0:8787 by default.

Configuration via environment variables:

- PORT (default 8787)
- HOST_IDLE_SECS (default 30)
- GUEST_IDLE_SECS (default 90)
- LOCK_TIMEOUT_SECS (default 180)
- ROOM_TTL_SECS (default 900, cleans up rooms that never got a host)
- MAX_ROOMS (default 256)
- MAX_CONNECTIONS (default 512)
- MAX_CONNECTIONS_PER_IP (default 24)
- MAX_PARTICIPANTS_PER_ROOM (default 12)
- MAX_MESSAGE_BYTES (default 1048576)
- RATE_WINDOW_SECS (default 10)
- MAX_FRAMES_PER_WINDOW (default 80)
- MAX_BYTES_PER_WINDOW (default 2097152)
- ROOM_CREATES_PER_WINDOW (default 10, per source IP)
- TRUST_PROXY (default false; enable only when direct access is blocked and a trusted reverse proxy sets X-Forwarded-For)

Vite's fingerprinted `/assets/` files are served as long-lived immutable content.
HTML and other entry files use `Cache-Control: no-cache`, so browsers revalidate
and discover each new build without a manual cache clear. API responses use `no-store`.

Logging is controlled with RUST_LOG (for example RUST_LOG=relay_server=debug). Only room IDs, participant IDs and events are logged, never payloads.

CORS is permissive by default for development. For production, restrict it to your frontend's origin and run behind TLS (wss://), otherwise the host token and metadata are readable on the wire.

## Testing

```bash
curl -X POST http://127.0.0.1:8787/api/rooms
# returns {"room_id":"A3K7PQ","host_token":"..."}
```

smoke_test.py (Python, requires pip install websockets) runs through the full protocol: host auth, wrong or duplicate tokens, presence, relay fan out, lock policy, muting during generation, host streaming, unlocking, host disconnect leading to room_closed and code 4001, and room cleanup. It should print ALL PASS.

You can also test manually: start the frontend, create a room, and open the invite link in a second private browser window to see presence, chat, the snapshot on join, and the lock triggering when you click reply.
