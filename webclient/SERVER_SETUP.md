# Web-Client vom Rust-Relay ausliefern

## 1. Cargo.toml

`tower-http` braucht zusätzlich das `fs`-Feature:

```toml
tower-http = { version = "0.5", features = ["cors", "fs"] }
```

## 2. main.rs

```rust
use tower_http::services::{ServeDir, ServeFile};

let static_dir = std::env::var("WEB_DIR").unwrap_or_else(|_| "web".into());
let index = format!("{static_dir}/index.html");

let app = Router::new()
    .route("/api/health", get(|| async { "ok" }))
    .route("/api/rooms", post(create_room))
    .route("/ws/:room_id", get(ws::ws_handler))
    // Statische Dateien; Fallback auf index.html, damit "/?mp=CODE#k=..." funktioniert.
    .fallback_service(ServeDir::new(&static_dir).fallback(ServeFile::new(index)))
    .layer(CorsLayer::permissive())
    .with_state(state);
```

Die expliziten `/api`- und `/ws`-Routen haben Vorrang; alles andere kommt aus dem
Ordner. Der Schlüssel (`#k=...`) steht im URL-Fragment und wird vom Browser nie an
den Server geschickt — er taucht also auch nicht in HTTP-Logs auf.

## 3. Frontend bauen und daneben legen

```
cd webclient
npm install
npm run build
```

Dann `dist/` neben das Server-Binary kopieren (oder `WEB_DIR` setzen):

```
cp -r dist /pfad/zum/server/web
```

Fertig: Gäste öffnen einfach `https://dein-server/…`, fügen den Einladungslink
ein (oder klicken ihn direkt an), geben ihren Namen ein und sind drin.

## Hinweise

- Der Web-Client ist bewusst **nur Gast**: Hosting (Charakterkarte, Verlauf,
  LLM-Aufruf) bleibt in der Desktop-App. Ein `&h=`-Host-Link funktioniert im
  Browser deshalb nicht als Host — die Seite tritt als Gast bei.
- In Produktion `CorsLayer::permissive()` entschärfen bzw. entfernen — wenn die
  Seite vom selben Origin kommt, ist CORS gar nicht mehr nötig.
- WebCrypto (`crypto.subtle`) funktioniert nur in "secure contexts": also über
  HTTPS oder `localhost`. Für Freunde übers Internet brauchst du TLS (z. B.
  Caddy/nginx davor oder direkt axum-server mit rustls).
