//! InitConnection — WebSocket signaling for the subraum mesh.
//!
//! Dumb relay: routes offer/answer/ice by `to`, keeps the per-room roster,
//! enforces room-auth + cap, mints ephemeral TURN creds. No media here.
//!
//! Env (full table in README.md):
//!   PORT             listen port (default 8080)
//!   ROOM_AUTH_SECRET HMAC secret for room tokens — required; the server
//!                    refuses to start without it unless ALLOW_OPEN_AUTH=1 (dev)
//!   TLS_DISABLE      1/true = plain ws behind a TLS proxy (binds 127.0.0.1
//!                    unless ALLOW_PLAIN_PUBLIC_BIND=1)
//!   TLS_CERT/TLS_KEY self-signed cert + key paths (generated on first run)
//!   TLS_SAN          extra SANs for that cert, comma-separated
//!   TURN_SECRET      coturn shared secret (optional)
//!   TURN_URLS        comma-separated turn: urls (optional)
//!   DOWNLOADS_DIR    installer mirror read by /get (default /srv/downloads/subraum)
//!
//! Subcommand: `init-connection mint <room>` prints that room's join token.

mod auth;
mod i18n;
mod sessions;
mod tls;
mod turn;

use i18n::{Lang, Ui};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use protocol::{ClientMsg, PeerInfo, ServerMsg, ERR_ID_IN_USE, ERR_PROTOCOL_MISMATCH, PROTOCOL_VERSION};
use serde_json::json;
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;

use auth::AuthConfig;
use sessions::{JoinError, Sessions};
use turn::TurnConfig;

/// Public base URL (for share links) + ws URL handed back on join.
fn public_base() -> String {
    std::env::var("PUBLIC_BASE").unwrap_or_else(|_| "https://subraum.cc".into())
}
fn public_ws() -> String {
    std::env::var("PUBLIC_WS").unwrap_or_else(|_| "wss://subraum.cc/ws".into())
}

/// Soft cap → quality warning. Hard cap → join refused. (ARCHITECTURE §10.)
const WARN_CAP: usize = 12;
const HARD_CAP: usize = 16;

// ── Input limits (defense against oversized/abusive frames) ──────────────────
const MAX_WS_MSG: usize = 64 * 1024; // whole WS text frame
const MAX_REST_BODY: usize = 4 * 1024; // REST JSON body
const MAX_ID: usize = 64; // room / user_id
const MAX_NAME: usize = 64;
const MAX_TOKEN: usize = 128;
const MAX_SECRET: usize = 64;
const MAX_SDP: usize = 16 * 1024;
const MAX_ICE: usize = 4 * 1024;
const MAX_CODE: usize = 32;
const MAX_PIN: usize = 12;
/// Per-peer outbound queue depth before backpressure drops signaling messages.
const PEER_CHAN: usize = 256;
// Per-connection abuse limits. A member is authenticated but not trusted.
const MSG_RATE_WINDOW: Duration = Duration::from_secs(1);
const MSG_RATE_MAX: u32 = 50; // WS frames per window before excess is dropped
const REKEY_COOLDOWN: Duration = Duration::from_secs(5); // min gap between rekeys
/// A socket that sends nothing for this long is dropped. Clients ping every
/// 25 s, so a live client never trips it; a half-open one stops counting toward
/// the cap and stops keeping its session alive.
const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

fn len_ok(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max
}

/// True when a raw frame carries `"t":"join"`, regardless of whether it
/// deserializes as the current `ClientMsg::Join`. Used to classify a join
/// from a different protocol generation before giving up on the socket.
fn is_join_frame(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("t").and_then(|t| t.as_str()).map(|t| t == "join"))
        .unwrap_or(false)
}

/// The terminal error for a client on another protocol generation. `proto`
/// is what the client sent, `None` when its join did not even deserialize
/// (pre-0.4.0 builds carry no `proto` at all).
fn protocol_mismatch_error(proto: Option<u32>) -> ServerMsg {
    let got = proto.map(|p| p.to_string()).unwrap_or_else(|| "none".into());
    ServerMsg::Error {
        code: ERR_PROTOCOL_MISMATCH.into(),
        message: format!("protocol {got} not supported, server speaks {PROTOCOL_VERSION}: update subraum"),
    }
}

/// Minimal HTML-escape for any value reflected into server-rendered HTML.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// CSP meta for the server-rendered pages (no scripts; inline styles + the logo).
const HTML_CSP: &str = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; font-src 'self'; base-uri 'none'; form-action 'none'\">";

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Per-IP rate limiting (fixed window) against session/PIN bruteforce ───────
const RL_WINDOW: u64 = 300; // 5 min
const RL_JOIN_MAX: u32 = 30; // PIN tries per IP per window
const RL_CREATE_MAX: u32 = 20; // session creations per IP per window

#[derive(Default)]
struct RateLimiter {
    inner: Mutex<HashMap<String, (u64, u32)>>, // ip -> (window_start, count)
}
impl RateLimiter {
    /// Returns true if allowed, false if the IP exceeded `max` in `window`.
    fn allow(&self, ip: &str, max: u32, window: u64) -> bool {
        let now = now_secs();
        let mut m = self.inner.lock().unwrap();
        let e = m.entry(ip.to_string()).or_insert((now, 0));
        if now.saturating_sub(e.0) >= window {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= max
    }
    fn prune(&self, window: u64) {
        let now = now_secs();
        self.inner.lock().unwrap().retain(|_, (start, _)| now.saturating_sub(*start) < window);
    }
}

/// Real client IP for rate-limiting. X-Forwarded-For is only trusted when the
/// direct peer is our loopback reverse proxy; otherwise the socket peer is used
/// (so a directly-reachable client can't spoof XFF to dodge/forge limits).
/// The LAST hop is used: it is whatever our reverse proxy saw as its peer,
/// whether the proxy replaces the header or appends to it. The first hop is
/// client-controlled when the proxy appends.
fn client_ip(headers: &HeaderMap, peer: SocketAddr) -> String {
    if peer.ip().is_loopback() {
        if let Some(last) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').map(str::trim).rfind(|s| !s.is_empty()))
        {
            return last.to_string();
        }
    }
    peer.ip().to_string()
}

/// CORS limited to known origins (own domain + Tauri webview + dev), extendable
/// via EXTRA_CORS_ORIGINS for a future browser participant.
fn build_cors() -> CorsLayer {
    let mut origins: Vec<HeaderValue> = [
        "https://subraum.cc",
        "http://tauri.localhost",
        "https://tauri.localhost",
        "tauri://localhost",
        "http://localhost:1420",
    ]
    .into_iter()
    .filter_map(|o| o.parse().ok())
    .collect();
    if let Ok(extra) = std::env::var("EXTRA_CORS_ORIGINS") {
        for o in extra.split(',') {
            if let Ok(v) = o.trim().parse() {
                origins.push(v);
            }
        }
    }
    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([axum::http::header::CONTENT_TYPE])
}

/// Boolean env switch: only `1` / `true` (any case) turn it on. `is_ok()` would
/// read `TLS_DISABLE=0` as "disable TLS".
fn env_flag(name: &str) -> bool {
    std::env::var(name).map(|v| flag_on(&v)).unwrap_or(false)
}
fn flag_on(v: &str) -> bool {
    let v = v.trim();
    v == "1" || v.eq_ignore_ascii_case("true")
}

struct PeerHandle {
    name: String,
    tx: mpsc::Sender<ServerMsg>,
    /// Join secret of this connection: only a later join presenting the same
    /// value may supersede it.
    secret: String,
}

type Room = HashMap<String, PeerHandle>; // user_id -> handle

struct AppState {
    rooms: Mutex<HashMap<String, Room>>,
    auth: AuthConfig,
    turn: Option<TurnConfig>,
    sessions: Sessions,
    rate: RateLimiter,
    /// See [`WS_IDLE_TIMEOUT`]; a field so tests can shorten it.
    ws_idle: Duration,
    /// Last accepted Rekey per room. Per room, not per socket: the room token
    /// is shared, so one member could otherwise dodge the cooldown by opening
    /// a second connection and tear down every link again and again.
    rekeys: Mutex<HashMap<String, Instant>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "init_connection=info,tower_http=info".into()),
        )
        .init();

    let auth = AuthConfig::from_env().map_err(|e| anyhow::anyhow!(e))?;

    // `mint <room>` helper: print the join token for a room and exit.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("mint") {
        let room = args.get(2).cloned().unwrap_or_default();
        match auth.token_for(&room) {
            Some(t) => println!("{t}"),
            None => eprintln!("ALLOW_OPEN_AUTH set: open mode, no token needed"),
        }
        return Ok(());
    }

    if matches!(auth, AuthConfig::Open) {
        tracing::warn!("ALLOW_OPEN_AUTH set: OPEN mode, any client may join any room (dev only)");
    }
    let turn = TurnConfig::from_env();
    tracing::info!("TURN minting: {}", if turn.is_some() { "enabled" } else { "disabled" });

    let state = Arc::new(AppState {
        rooms: Mutex::new(HashMap::new()),
        auth,
        turn,
        sessions: Sessions::default(),
        rate: RateLimiter::default(),
        ws_idle: WS_IDLE_TIMEOUT,
        rekeys: Mutex::new(HashMap::new()),
    });

    // Session lifecycle: keep a session alive while its room has members,
    // grace after empty, 24h hard cap. Swept once a minute.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                iv.tick().await;
                state.sessions.reap(|room| {
                    state.rooms.lock().unwrap().get(room).map(|r| !r.is_empty()).unwrap_or(false)
                });
                state.rate.prune(RL_WINDOW);
            }
        });
    }

    let app = router(state);

    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);

    // TLS by default (wss). TLS_DISABLE=1 → plain ws (must sit behind a TLS proxy);
    // bind loopback only unless ALLOW_PLAIN_PUBLIC_BIND=1, so a misconfig can't
    // expose plain ws to the network.
    if env_flag("TLS_DISABLE") {
        let bind_ip = if env_flag("ALLOW_PLAIN_PUBLIC_BIND") {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        };
        tracing::warn!("TLS_DISABLE set: serving PLAIN ws on {bind_ip}:{port} (proxy expected)");
        let listener = tokio::net::TcpListener::bind((bind_ip, port)).await?;
        tracing::info!("InitConnection listening (ws) on {bind_ip}:{port} (warn@{WARN_CAP} hard@{HARD_CAP})");
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
        return Ok(());
    }

    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_path = std::env::var("TLS_CERT").unwrap_or_else(|_| "init-cert.pem".into());
    let key_path = std::env::var("TLS_KEY").unwrap_or_else(|_| "init-key.pem".into());
    let cert = tls::ensure(&cert_path, &key_path)?;
    tracing::info!("InitConnection listening (wss) on :{port} (warn@{WARN_CAP} hard@{HARD_CAP})");
    tracing::info!("TLS cert SHA-256 (pin this on the client): {}", cert.fingerprint);
    println!("CERT_SHA256={}", cert.fingerprint);
    let rustls_config = axum_server::tls_rustls::RustlsConfig::from_pem(
        cert.cert_pem.into_bytes(),
        cert.key_pem.into_bytes(),
    )
    .await?;
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    axum_server::bind_rustls(addr, rustls_config)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}

/// Every route the server exposes; shared by `main` and the WS tests.
fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/get", get(downloads_page))
        .route("/privacy", get(privacy))
        .route("/legal", get(legal))
        .route("/license", get(license_page))
        .route("/changelog", get(changelog_page))
        .route("/ws", get(ws_handler))
        .route("/healthz", get(|| async { "ok" }))
        // Brand assets are compiled into the binary, so a fresh deploy serves the
        // right logo with no files to copy onto the host.
        .route("/assets/logo.svg", get(logo_svg))
        .route("/assets/rdoc.svg", get(rdoc_svg))
        .route("/assets/rdoc-light.svg", get(rdoc_light_svg))
        .route("/assets/fonts/:name", get(font_file))
        .route("/assets/og-image.png", get(og_image))
        .route("/assets/shot/:n", get(shot_png))
        // PIN-protected session brokering (REST, called by the app webview → CORS).
        .route("/session", post(create_session))
        .route("/session/:code/join", post(join_session))
        .route("/j/:code", get(landing))
        .layer(DefaultBodyLimit::max(MAX_REST_BODY))
        .layer(build_cors())
        .with_state(state)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.max_message_size(MAX_WS_MSG)
        .max_frame_size(MAX_WS_MSG)
        .on_upgrade(move |socket| handle_socket(socket, state))
}

/// Host creates a session → random room + token + 6-digit PIN + share code.
async fn create_session(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !state.rate.allow(&client_ip(&headers, peer), RL_CREATE_MAX, RL_WINDOW) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": "rate_limited" }))).into_response();
    }
    let (code, pin, room, token) = state.sessions.create(|r| state.auth.token_for(r));
    let base = public_base();
    tracing::info!(%code, %room, "session created");
    Json(json!({
        "code": code,
        "pin": pin,
        "room": room,
        "token": token,
        "ws": public_ws(),
        "link": format!("{base}/j/{code}"),
    }))
    .into_response()
}

/// Mate resolves a code with the PIN (rate-limited) → room + token.
async fn join_session(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(code): Path<String>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Response {
    if !state.rate.allow(&client_ip(&headers, peer), RL_JOIN_MAX, RL_WINDOW) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": "rate_limited" }))).into_response();
    }
    let pin = body.get("pin").and_then(|v| v.as_str()).unwrap_or("");
    // Bound code/PIN lengths before touching the session store.
    if !len_ok(&code, MAX_CODE) || !len_ok(pin, MAX_PIN) {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response();
    }
    match state.sessions.join(&code, pin) {
        Ok((room, token)) => {
            Json(json!({ "room": room, "token": token, "ws": public_ws() })).into_response()
        }
        Err(JoinError::NotFound) => {
            (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response()
        }
        Err(JoinError::Locked) => {
            (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": "locked" }))).into_response()
        }
        Err(JoinError::BadPin) => {
            (StatusCode::FORBIDDEN, Json(json!({ "error": "bad_pin" }))).into_response()
        }
    }
}

/// Best language for a request: ?lang= → Accept-Language → English.
fn lang_of(q: &Option<String>, headers: &HeaderMap) -> Lang {
    let qlang = q
        .as_deref()
        .and_then(|s| s.split('&').find_map(|kv| kv.strip_prefix("lang=")));
    let accept = headers.get("accept-language").and_then(|v| v.to_str().ok());
    Lang::detect(qlang, accept)
}

/// Cookie the display size is remembered in. It holds one of `md|lg|xl` and
/// nothing else: no id, no timestamp, nothing that could identify a visitor.
/// The privacy page names it — the site promises no tracking, and a cookie it
/// does set has to be declared for that promise to stay honest.
const UI_COOKIE: &str = "subraum_ui";

/// Display size: `?ui=` wins, else the cookie, else normal. The bool says the
/// size came from the query, which is when the answer gets a `Set-Cookie` —
/// following a size link is the explicit act that stores the preference.
fn ui_of(q: &Option<String>, headers: &HeaderMap) -> (Ui, bool) {
    let qui = q
        .as_deref()
        .and_then(|s| s.split('&').find_map(|kv| kv.strip_prefix("ui=")));
    let prefix = format!("{UI_COOKIE}=");
    let cookie = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| c.split(';').find_map(|kv| kv.trim().strip_prefix(&prefix)))
        .map(|s| s.to_string());
    (Ui::detect(qui, cookie.as_deref()), qui.is_some())
}

/// A year, path-wide, unreadable by script (there is none) and not sent on
/// cross-site requests. `Secure` follows `PUBLIC_BASE`, not the request scheme:
/// production is HTTPS and gets the flag, while a dev server that also sets
/// `PUBLIC_BASE=http://…` does not — a `Secure` cookie over plain http is
/// dropped by the browser without a word.
fn ui_cookie(ui: Ui) -> String {
    let secure = if public_base().starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!(
        "{UI_COOKIE}={}; Path=/; Max-Age=31536000; SameSite=Lax; HttpOnly{secure}",
        ui.code()
    )
}

/// Wraps a rendered page so an explicit size choice is remembered.
fn page(html: Html<String>, set_cookie: bool, ui: Ui) -> Response {
    if set_cookie {
        ([(axum::http::header::SET_COOKIE, ui_cookie(ui))], html).into_response()
    } else {
        html.into_response()
    }
}

/// Human landing page for a share link (localized; code is HTML-escaped).
async fn landing(Path(code): Path<String>, RawQuery(q): RawQuery, headers: HeaderMap) -> Response {
    let lang = lang_of(&q, &headers);
    let (ui, chose) = ui_of(&q, &headers);
    let code = esc(&code.chars().take(32).collect::<String>());
    let body = i18n::landing(lang, &public_base(), &code);
    page(shell(lang, ui, &format!("/j/{code}"), "subraum", &body), chose, ui)
}

/// Page styling. The look is deliberately a datasheet, not a landing page: the
/// product's whole claim is topological (nothing sits in the middle of a call),
/// so the page is built from schematics, hairlines and mono labels rather than
/// marketing furniture. No webfont — the CSP allows no font-src, and the system
/// mono stack is what a spec sheet would use anyway.
const PAGE_CSS: &str = r#"<style>
/* RDOC brand system, Markenhandbuch v2.2.
   Rules this file is bound by, quoted where they bite:
   - Michroma is the ONLY display face and has exactly one cut. Emphasis in a
     heading comes from size and colour, never font-weight (Kap. 10).
   - Display tracking is 0. The negative values of v2.0 corrected Space
     Grotesk's narrow rhythm; Michroma brings the width itself (Kap. 10).
   - Copper marks exactly one element per view: the primary action. Never body
     text, never a background except a surface that IS the action (Kap. 8).
   - No gradient, shadow, glow, bevel (Kap. 8).
   - Light mode is measured, not inverted (Kap. 8). Copper drops to Copper Deep
     there because #C48A4A reaches only 2.65:1 on Off White.
   - State is never colour alone (Kap. 16).
   - Patina is the second accent added in v2.2 (Kap. 8). It carries STRUCTURE:
     section markers, table heads, step numbers, one data series. Three rules
     bound it and all three are load-bearing here:
       1. Patina is never the signet ring. The ring stays Copper.
       2. Patina is not a second primary action. Copper is the button.
       3. Patina is not a state colour. A state carries the functional colour
          and a word, never Patina and never grey.
     Like Copper it fails on a light ground (#4FB5B5 is 2.12:1 on Off White),
     so light mode swaps in Patina Deep #175F63 at 6.57:1.
     Before v2.2 everything that was not the one primary action fell back to
     Steel, which is why the page read as one flat grey. */
:root{
color-scheme:dark light;
/* Dark is the default ground. */
--bg:#121416;--surface:#2B3135;--line:#2B3135;
--ink:#F2F2F0;--dim:#76828D;--accent:#C48A4A;--accent-2:#4FB5B5;--focus:#E0A868;
/* Copper on Space is 6.22:1, legible as text. On Off White it is not, so the
   light block below swaps in Copper Deep. */
--accent-ink:#121416;
--mono:"IBM Plex Mono",ui-monospace,Consolas,monospace;
--sans:"IBM Plex Sans",system-ui,"Segoe UI",Arial,sans-serif;
--disp:"Michroma",var(--sans);
--wrap:64rem;--prose:41rem;
}
@media (prefers-color-scheme:light){
:root{
--bg:#F2F2F0;--surface:#E4E4E1;--line:#C7C9C6;
--ink:#121416;--dim:#4E5862;--accent:#8A5A22;--accent-2:#175F63;--focus:#6E4517;
--accent-ink:#F2F2F0;
}
}
@font-face{font-family:"Michroma";font-weight:400;font-style:normal;font-display:swap;src:url(/assets/fonts/mi-400.woff2) format("woff2")}
@font-face{font-family:"IBM Plex Sans";font-weight:400;font-style:normal;font-display:swap;src:url(/assets/fonts/ps-400.woff2) format("woff2")}
@font-face{font-family:"IBM Plex Sans";font-weight:600;font-style:normal;font-display:swap;src:url(/assets/fonts/ps-600.woff2) format("woff2")}
@font-face{font-family:"IBM Plex Mono";font-weight:400;font-style:normal;font-display:swap;src:url(/assets/fonts/pm-400.woff2) format("woff2")}
*{box-sizing:border-box}
/* Display size. Every length in this sheet is rem or em, so moving the root
   font size scales type, spacing, the content column and the schematic
   together — which is what "make it bigger" means. Media-query breakpoints are
   unaffected: rem in a media query resolves against the browser default, not
   against this. */
:root[data-ui="lg"]{font-size:118.75%}   /* 19px */
:root[data-ui="xl"]{font-size:137.5%}    /* 22px */
body{font-family:var(--sans);font-weight:400;background:var(--bg);color:var(--ink);
margin:0;line-height:1.6;font-size:1rem;-webkit-text-size-adjust:100%}
/* Links are Patina, not Copper: Copper is spent on the one primary action and
   a page of Copper links would drown it. */
a{color:var(--accent-2);text-decoration:none;border-bottom:1px solid var(--accent-2)}
a:hover{color:var(--ink);border-bottom-color:var(--ink)}
a:focus-visible,button:focus-visible{outline:2px solid var(--focus);outline-offset:2px}
img{max-width:100%;height:auto}

/* Theme-swapped brand assets. Two files instead of a CSS filter: the guide
   rejects filters on brand files, and the mono cuts exist for both grounds. */
.on-dark{display:block}
.on-light{display:none}
@media (prefers-color-scheme:light){
.on-dark{display:none}
.on-light{display:block}
}

/* ── Frame ─────────────────────────────────────────────────────────────── */
/* Wraps on purpose: at the largest display size on a phone the mark, the name
   and both switchers do not fit one line, and an unwrapped row pushes the size
   control off-screen — unreachable for exactly the people who set it. */
.top{display:flex;align-items:center;flex-wrap:wrap;gap:.65rem;padding:.85rem 1.4rem;
border-bottom:1px solid var(--line)}
/* rem, not px: the mark has to grow with the display-size setting, or the
   header stays small while everything under it scales. */
.top svg{width:1.625rem;height:1.625rem;display:block;flex:none}
/* The wordmark is set in Michroma at label size, the product name is a
   heading, not UI chrome. Lowercase is deliberate and part of the name. */
.top .brand{color:var(--ink);font-family:var(--disp);font-weight:400;
font-size:.9rem;letter-spacing:0;border:0}
/* Chrome, not content: the language nav and the size control keep fixed px
   sizes so the header stays compact at the largest setting. Scaling the
   controls along with the page is what pushed the row past a phone's width and
   forced the whole layout to overflow sideways. It wraps for the same reason. */
.lang{margin-left:auto;display:flex;flex-wrap:wrap;gap:.15rem;font-family:var(--mono);font-size:12px}
/* Text-size control: three A's that show their own effect. It sits after the
   language nav and keeps a hairline between them so the two groups do not read
   as one list. The sizes here are fixed in px on purpose — the control must not
   grow with the setting it changes, or the largest step pushes the header
   around. */
.uisize{display:flex;align-items:baseline;gap:.35rem;margin-left:.9rem;padding-left:.9rem;
border-left:1px solid var(--line)}
/* Once the row wraps, the size control starts the new line: drop the divider
   that would then hang at the left edge, and let it keep the auto margin. */
@media (max-width:34rem){
.uisize{margin-left:auto;padding-left:0;border-left:0}
}
.uisize a{border:0;color:var(--dim);line-height:1;padding:0 .1rem}
.uisize a:hover{color:var(--ink)}
.uisize a.on{color:var(--accent-2)}
.uisize a.md{font-size:12px}
.uisize a.lg{font-size:15px}
.uisize a.xl{font-size:18px}
.lang a{color:var(--dim);padding:.15rem .4rem;border:0;letter-spacing:.07em}
.lang a:hover{color:var(--ink)}
.lang a.on{color:var(--accent-2);border-bottom:1px solid var(--accent-2)}
main{max-width:var(--wrap);margin:0 auto;padding:0 1.4rem 4rem}
footer{max-width:var(--wrap);margin:0 auto;padding:1.4rem;border-top:1px solid var(--line);
color:var(--dim);font-size:.875rem;display:flex;flex-wrap:wrap;align-items:center;gap:.35rem 1.3rem}
footer a{color:var(--dim);border:0}
footer a:hover{color:var(--accent-2)}
/* Clear space is half the cap height, applied as padding, nothing enters it. */
footer .rdoc{margin-left:auto;border:0;display:block;padding:.5rem 0}
/* 150px lockup -> the 220-unit signet lands at 32px, the floor in Kap. 6.
   Below it the 4-degree radial cuts fall under a pixel and the ring reads as a
   plain circle. */
/* 9.375rem == 150px at the normal size, where the 220-unit signet lands on the
   32px floor of Kap. 6. It scales up with the setting and never below. */
footer .rdoc img{display:block;width:9.375rem;height:auto}

/* ── Sections. A hairline plus a mono eyebrow: the page reads as a datasheet,
      which is what the product is. ───────────────────────────────────────── */
.sec{border-top:1px solid var(--line);padding:2.6rem 0 .4rem;margin-top:2.6rem}
.sec:first-of-type{border-top:0;margin-top:0}
.eyebrow{font-family:var(--mono);font-size:.8125rem;line-height:1.3;letter-spacing:.07em;
text-transform:uppercase;color:var(--dim);margin:0 0 1rem}
/* The eyebrow names the layer being described — a section marker, which is
   exactly what Patina is for. */
.eyebrow b{color:var(--accent-2);font-weight:400}
.prose{max-width:var(--prose)}
/* Michroma: one cut, tracking 0, no synthetic weight anywhere below. */
h1,h2,h3{font-family:var(--disp);font-weight:400;letter-spacing:0;
overflow-wrap:break-word}
h1{font-size:clamp(1.5rem,4vw,2.125rem);line-height:1.15;margin:0 0 .6rem}
h2{font-size:clamp(1.35rem,3vw,1.75rem);line-height:1.2;margin:0 0 .6rem}
h3{font-size:1.3125rem;line-height:1.3;margin:1.5rem 0 .4rem}
p{margin:.7rem 0}
ul{padding-left:1.1rem;margin:.6rem 0}
li{margin:.3rem 0}
strong,b{font-weight:600}
.muted{color:var(--dim);font-size:.875rem}
.tagline{font-family:var(--mono);font-size:.8125rem;letter-spacing:.07em;
text-transform:uppercase;color:var(--dim);margin:0 0 1.8rem}
code{font-family:var(--mono);font-size:.875rem;background:var(--surface);
padding:.05rem .32rem;border-radius:2px}

/* ── Hero. Michroma runs ~1.35x wider than a grotesk, so the display size is
      the guide's 48px ceiling and clamps down hard on narrow screens. ────── */
.hero{padding:3.2rem 0 0}
.hero h1{font-size:clamp(1.75rem,6.5vw,3rem);line-height:1.1;margin:0 0 .7rem}
.lede{font-size:1.125rem;line-height:1.6;max-width:var(--prose)}

/* ── Schematic: the signature element, and the one thing on the page that is
      an argument rather than a claim. Colours come from tokens, so it follows
      the scheme. ─────────────────────────────────────────────────────────── */
.diagram{margin:1.8rem 0 .6rem;border:1px solid var(--line);padding:1.4rem;overflow-x:auto}
/* px, not rem: the floor is about the schematic staying readable, and in rem it
   would grow with the display size and force its own scrollbar at the largest
   step. The container scrolls it when it does not fit. */
.diagram svg{display:block;width:100%;height:auto;min-width:384px}
.diagram figcaption{font-family:var(--mono);font-size:.8125rem;line-height:1.3;
color:var(--dim);margin-top:1rem;letter-spacing:.07em}

/* ── Plane cards. Hairline cells, not filled tiles: Graphite is structure, and
      on a light ground a filled tile would fight the page. ───────────────── */
.planes{display:grid;grid-template-columns:repeat(auto-fit,minmax(17rem,1fr));
gap:1px;background:var(--line);border:1px solid var(--line);margin:1.5rem 0}
.plane{background:var(--bg);padding:1.2rem 1.3rem}
.plane h3{margin:.2rem 0 .5rem;font-size:1.3125rem}
.plane .tag{font-family:var(--mono);font-size:.8125rem;letter-spacing:.07em;
text-transform:uppercase;color:var(--dim)}
/* One data series carries Patina — here the plane the product is about. The
   control plane stays Steel; that contrast is the whole point of the pair. */
.plane.p2p .tag{color:var(--accent-2)}
.plane p{margin:.45rem 0;font-size:.9375rem;color:var(--dim)}
.plane p strong{color:var(--ink)}

/* ── Spec rows. The label column is mono because a machine produced those
      names; the value is prose because a human reads the answer. ────────── */
.spec{border-top:1px solid var(--line);margin:1.4rem 0 0;max-width:52rem}
.spec div{display:flex;align-items:baseline;gap:.8rem;padding:.6rem 0;
border-bottom:1px solid var(--line);font-size:.9375rem}
.spec dt,.spec .k{font-family:var(--mono);font-size:.8125rem;letter-spacing:.07em;
color:var(--accent-2);flex:none;min-width:14rem}
.spec dd,.spec .v{margin:0;color:var(--ink)}
/* State carries the word, never colour alone. */
.spec .no,.spec .yes{color:var(--ink)}

/* ── Steps: a real sequence, so it is numbered. ────────────────────────── */
.steps{counter-reset:s;list-style:none;padding:0;margin:1.2rem 0;max-width:var(--prose)}
.steps li{counter-increment:s;position:relative;padding-left:2.8rem;margin:1rem 0}
.steps li::before{content:counter(s,decimal-leading-zero);position:absolute;left:0;top:.1rem;
font-family:var(--mono);font-size:.8125rem;color:var(--accent-2);letter-spacing:.07em}

/* ── Actions. Copper is the primary action and appears once per view. ───── */
.dl{display:inline-block;margin:.4rem .5rem .4rem 0;padding:.65rem 1.05rem;
border:1px solid var(--dim);color:var(--ink);font-size:.9375rem;background:none}
.dl:hover{border-color:var(--ink)}
.dl.store{display:inline-flex;align-items:center;gap:.6rem;border:1px solid var(--accent);
background:var(--accent);color:var(--accent-ink);font-weight:600;
padding:.75rem 1.2rem;font-size:1rem}
.dl.store:hover{background:var(--accent);color:var(--accent-ink);border-color:var(--ink)}
.dl.store svg{display:block;flex:none}
.announce{border:1px solid var(--line);border-left:2px solid var(--accent-2);
padding:.9rem 1.1rem;color:var(--dim);font-size:.9375rem}
.announce strong{color:var(--ink)}

/* ── Downloads ─────────────────────────────────────────────────────────── */
.arts{list-style:none;padding:0;margin:.9rem 0;border-top:1px solid var(--line)}
.arts li{border-bottom:1px solid var(--line);padding:.85rem 0}
.arts .file{font-family:var(--mono);font-size:.9375rem;color:var(--ink);border:0;
word-break:break-all}
.arts .file:hover{border-bottom:1px solid var(--ink)}
.arts .meta{font-family:var(--mono);font-size:.8125rem;color:var(--dim);letter-spacing:.07em;
margin-top:.3rem;display:block}
.arts .sha{font-family:var(--mono);font-size:.75rem;color:var(--dim);word-break:break-all;
display:block;margin-top:.25rem;letter-spacing:.02em}

/* ── Screenshots. Real captures of what runs, the guide rules out symbol
      imagery, so these carry the "what it looks like" job alone. ────────── */
.shots{display:grid;grid-template-columns:repeat(auto-fill,minmax(16rem,1fr));gap:1.2rem;
margin:1.2rem 0;align-items:start}
.shot{margin:0}
.shot a{display:block;border:0}
.shot img{display:block;width:100%;height:auto;max-height:24rem;object-fit:contain;
object-position:top;border:1px solid var(--line)}
.shot img:hover{border-color:var(--dim)}
.shot figcaption{color:var(--dim);font-size:.75rem;letter-spacing:.02em;
margin-top:.5rem;line-height:1.4}

/* ── Invite landing ────────────────────────────────────────────────────── */
.code{font-family:var(--mono);font-size:clamp(1.6rem,6vw,2.2rem);font-weight:400;
letter-spacing:.22em;border:1px solid var(--line);padding:.75rem 1.15rem;
display:inline-block;color:var(--ink)}
.links a{display:block;margin:.4rem 0;width:fit-content}

/* ── Changelog. Versions are machine-made identifiers → mono, not Michroma. */
h2.ver{font-family:var(--mono);font-size:1rem;letter-spacing:.07em;color:var(--accent-2);
font-weight:400;border-top:1px solid var(--line);padding-top:1.2rem;margin-top:2.2rem}

@media (max-width:34rem){
.top{padding:.75rem 1rem}
main{padding:0 1rem 3rem}
.diagram{padding:.9rem}
.spec div{flex-direction:column;gap:.2rem}
.spec dt,.spec .k{min-width:0}
footer .rdoc{margin-left:0}
}
@media (prefers-reduced-motion:reduce){
*,*::before,*::after{animation-duration:.01ms!important;transition-duration:.01ms!important}
}
</style>"#;

/// The subraum mark: a surface line with the peer mesh hanging below it, one node
/// breaking through. Four peers, all six links, nothing in the middle — the
/// topology the product actually uses.
const LOGO_SVG: &str = include_str!("../assets/logo.svg");
/// RDOC lockup for the footer (mono off-white, per the brand kit's dark-ground
/// rule; >=96 px wide, clear space via padding).
const RDOC_SVG: &str = include_str!("../assets/rdoc.svg");
/// Same lockup for light grounds (mono-space cut) — chosen by ground, per the
/// brand kit's variant table, not by inverting one file.
const RDOC_LIGHT_SVG: &str = include_str!("../assets/rdoc-light.svg");
/// The subraum mark, inlined into the header so its strokes can inherit
/// `currentColor` and follow the colour scheme. As an <img> it could not.
const MARK_INLINE: &str = include_str!("../assets/mark-inline.svg");
/// Brand fonts, subset to latin(+ext) and woff2-packed (~49 KB total) so the
/// site needs no external font host — the CSP allows font-src 'self' only.
const FONTS: [(&str, &[u8]); 4] = [
    ("mi-400.woff2", include_bytes!("../assets/fonts/mi-400.woff2")),
    ("ps-400.woff2", include_bytes!("../assets/fonts/ps-400.woff2")),
    ("ps-600.woff2", include_bytes!("../assets/fonts/ps-600.woff2")),
    ("pm-400.woff2", include_bytes!("../assets/fonts/pm-400.woff2")),
];

async fn rdoc_light_svg() -> Response {
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/svg+xml"),
            (axum::http::header::CACHE_CONTROL, ASSET_CACHE),
        ],
        RDOC_LIGHT_SVG,
    )
        .into_response()
}

async fn rdoc_svg() -> Response {
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/svg+xml"),
            (axum::http::header::CACHE_CONTROL, ASSET_CACHE),
        ],
        RDOC_SVG,
    )
        .into_response()
}

async fn font_file(axum::extract::Path(name): axum::extract::Path<String>) -> Response {
    match FONTS.iter().find(|(n, _)| *n == name) {
        Some((_, bytes)) => (
            [
                (axum::http::header::CONTENT_TYPE, "font/woff2"),
                (axum::http::header::CACHE_CONTROL, ASSET_CACHE),
            ],
            *bytes,
        )
            .into_response(),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}
/// Social preview card, pre-rendered from `assets/og-image.svg` (scrapers do not
/// render SVG, so this one ships as a PNG).
const OG_IMAGE_PNG: &[u8] = include_bytes!("../assets/og-image.png");

/// App screenshots for the home page gallery, in display order. They live in the
/// repo and ship inside the binary on purpose: the earlier arrangement kept them
/// in the download mirror, which `pull-installer.sh` empties on every release —
/// screenshots put there survived at most an hour. Order must match the captions
/// in `i18n::screenshots`.
const SHOTS: [&[u8]; 5] = [
    include_bytes!("../assets/shot-1.png"),
    include_bytes!("../assets/shot-2.png"),
    include_bytes!("../assets/shot-3.png"),
    include_bytes!("../assets/shot-4.png"),
    include_bytes!("../assets/shot-5.png"),
];

async fn shot_png(Path(n): Path<usize>) -> Response {
    match n.checked_sub(1).and_then(|i| SHOTS.get(i)) {
        Some(bytes) => (
            [
                (axum::http::header::CONTENT_TYPE, "image/png"),
                (axum::http::header::CACHE_CONTROL, ASSET_CACHE),
            ],
            *bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Cache for a day: the assets only change when a new binary is deployed.
const ASSET_CACHE: &str = "public, max-age=86400";

async fn logo_svg() -> Response {
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/svg+xml"),
            (axum::http::header::CACHE_CONTROL, ASSET_CACHE),
        ],
        LOGO_SVG,
    )
        .into_response()
}

async fn og_image() -> Response {
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/png"),
            (axum::http::header::CACHE_CONTROL, ASSET_CACHE),
        ],
        OG_IMAGE_PNG,
    )
        .into_response()
}

fn footer(base: &str, lang: Lang) -> String {
    let n = i18n::nav(lang);
    let lc = lang.code();
    format!(
        r#"<a href="{base}/get?lang={lc}">{}</a><a href="/privacy?lang={lc}">{}</a><a href="/legal?lang={lc}">{}</a><a href="/license?lang={lc}">{}</a><a href="/changelog?lang={lc}">Changelog</a><a href="{gh}">GitHub</a><a class="rdoc" href="{rd}" aria-label="RDOC"><img class="on-dark" src="/assets/rdoc.svg" alt="RDOC" width="150" height="32"><img class="on-light" src="/assets/rdoc-light.svg" alt="RDOC" width="150" height="32"></a>"#,
        n[0], n[1], n[2], n[3], gh = i18n::GITHUB_URL, rd = i18n::RAUMDOCK_URL
    )
}

fn shell(lang: Lang, ui: Ui, path: &str, title: &str, body: &str) -> Html<String> {
    let base = public_base();
    let lc = lang.code();
    let desc = i18n::meta_desc(lang);
    let og_title = format!("{title} | subraum");
    let og_image = format!("{base}/assets/og-image.png");
    let og_url = format!("{base}{path}");
    Html(format!(
        "<!doctype html><html lang=\"{lc}\" data-ui=\"{uic}\"><head><meta charset=\"utf-8\">{HTML_CSP}\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{title} | subraum</title>\
<meta name=\"description\" content=\"{desc}\">\
<meta property=\"og:type\" content=\"website\">\
<meta property=\"og:site_name\" content=\"subraum\">\
<meta property=\"og:title\" content=\"{og_title}\">\
<meta property=\"og:description\" content=\"{desc}\">\
<meta property=\"og:url\" content=\"{og_url}\">\
<meta property=\"og:image\" content=\"{og_image}\">\
<meta property=\"og:image:width\" content=\"1200\"><meta property=\"og:image:height\" content=\"630\">\
<meta property=\"og:image:alt\" content=\"subraum\">\
<meta name=\"twitter:card\" content=\"summary_large_image\">\
<meta name=\"twitter:title\" content=\"{og_title}\">\
<meta name=\"twitter:description\" content=\"{desc}\">\
<meta name=\"twitter:image\" content=\"{og_image}\">\
<meta name=\"theme-color\" media=\"(prefers-color-scheme: dark)\" content=\"#121416\">\
<meta name=\"theme-color\" media=\"(prefers-color-scheme: light)\" content=\"#F2F2F0\">\
<link rel=\"icon\" href=\"/assets/logo.svg\">{css}</head><body>\
<header class=\"top\">{mark}\
<a class=\"brand\" href=\"/?lang={lc}\">subraum</a>{sw}</header>\
<main>{body}</main><footer>{footer}</footer></body></html>",
        css = PAGE_CSS,
        uic = ui.code(),
        mark = MARK_INLINE,
        sw = i18n::switcher(path, lang, ui),
        footer = footer(&base, lang),
    ))
}

async fn home(RawQuery(q): RawQuery, headers: HeaderMap) -> Response {
    let lang = lang_of(&q, &headers);
    let (ui, chose) = ui_of(&q, &headers);
    let (title, body) = i18n::home(lang, &public_base(), SHOTS.len());
    page(shell(lang, ui, "/", title, &body), chose, ui)
}

async fn privacy(RawQuery(q): RawQuery, headers: HeaderMap) -> Response {
    let lang = lang_of(&q, &headers);
    let (ui, chose) = ui_of(&q, &headers);
    let (title, body) = i18n::privacy(lang);
    page(shell(lang, ui, "/privacy", title, &body), chose, ui)
}

async fn legal(RawQuery(q): RawQuery, headers: HeaderMap) -> Response {
    let lang = lang_of(&q, &headers);
    let (ui, chose) = ui_of(&q, &headers);
    let (title, body) = i18n::legal(lang);
    page(shell(lang, ui, "/legal", title, &body), chose, ui)
}

async fn license_page(RawQuery(q): RawQuery, headers: HeaderMap) -> Response {
    let lang = lang_of(&q, &headers);
    let (ui, chose) = ui_of(&q, &headers);
    let (title, body) = i18n::license(lang);
    page(shell(lang, ui, "/license", title, &body), chose, ui)
}

/// The public changelog, rendered from the repo `CHANGELOG.md` (embedded at
/// build time) into HTML, one section per version. Not localized — the source
/// is a single mixed EN/DE document.
const CHANGELOG_MD: &str = include_str!("../../../CHANGELOG.md");

async fn changelog_page(RawQuery(q): RawQuery, headers: HeaderMap) -> Response {
    let lang = lang_of(&q, &headers);
    let (ui, chose) = ui_of(&q, &headers);
    let body = i18n::doc(&render_changelog(CHANGELOG_MD));
    page(shell(lang, ui, "/changelog", "Changelog", &body), chose, ui)
}

/// Minimal, trusted-input Markdown → HTML for the changelog. Handles the subset
/// the file uses: `## ` version headers, `### ` sections, `- ` bullets (with
/// wrapped continuation lines), `**bold**` and `` `code` ``.
fn render_changelog(md: &str) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }
    // Escape first, then apply `code` and **bold** on the escaped text (the
    // inserted tags contain no backticks/asterisks, so each loop terminates).
    fn inline(s: &str) -> String {
        let mut out = esc(s);
        while let Some(a) = out.find('`') {
            let Some(rel) = out[a + 1..].find('`') else { break };
            let b = a + 1 + rel;
            let code = out[a + 1..b].to_string();
            out.replace_range(a..=b, &format!("<code>{code}</code>"));
        }
        while let Some(a) = out.find("**") {
            let Some(rel) = out[a + 2..].find("**") else { break };
            let b = a + 2 + rel;
            let strong = out[a + 2..b].to_string();
            out.replace_range(a..b + 2, &format!("<strong>{strong}</strong>"));
        }
        out
    }

    let mut html = String::from("<h1>Changelog</h1>");
    let mut in_ul = false;
    for raw in md.lines() {
        let line = raw.trim_end();
        if let Some(rest) = line.strip_prefix("## ") {
            if in_ul { html.push_str("</ul>"); in_ul = false; }
            html.push_str(&format!("<h2 class=\"ver\">{}</h2>", inline(rest)));
        } else if let Some(rest) = line.strip_prefix("### ") {
            if in_ul { html.push_str("</ul>"); in_ul = false; }
            html.push_str(&format!("<h3>{}</h3>", inline(rest)));
        } else if line.strip_prefix("# ").is_some() {
            // top-level title — skip (we emit our own <h1>)
        } else if let Some(rest) = line.strip_prefix("- ") {
            if !in_ul { html.push_str("<ul>"); in_ul = true; }
            html.push_str(&format!("<li>{}</li>", inline(rest)));
        } else if line.trim().is_empty() {
            if in_ul { html.push_str("</ul>"); in_ul = false; }
        } else if in_ul {
            // wrapped continuation of the current bullet → append to last <li>
            if let Some(pos) = html.rfind("</li>") {
                html.insert_str(pos, &format!(" {}", inline(line.trim())));
            }
        } else {
            html.push_str(&format!("<p class=\"muted\">{}</p>", inline(line.trim())));
        }
    }
    if in_ul { html.push_str("</ul>"); }
    html
}

/// Localized download page: lists every mirrored installer + its SHA-256, read
/// from $DOWNLOADS_DIR/manifest.json (written by deploy/pull-installer.sh).
async fn downloads_page(RawQuery(q): RawQuery, headers: HeaderMap) -> Response {
    let lang = lang_of(&q, &headers);
    let (ui, chose) = ui_of(&q, &headers);
    let (version, arts) = load_manifest();
    let (title, body) = i18n::downloads(lang, &public_base(), version.as_deref(), &arts);
    page(shell(lang, ui, "/get", title, &body), chose, ui)
}

fn downloads_dir() -> String {
    std::env::var("DOWNLOADS_DIR").unwrap_or_else(|_| "/srv/downloads/subraum".into())
}

/// Parse the mirror manifest into (version, artifacts). Missing or invalid →
/// empty, so the page degrades to a "no builds yet" notice instead of erroring.
fn load_manifest() -> (Option<String>, Vec<i18n::Artifact>) {
    let dir = downloads_dir();
    parse_manifest(&std::path::Path::new(&dir).join("manifest.json"))
}

/// `load_manifest` for a given file (no environment access, so it is testable).
fn parse_manifest(path: &std::path::Path) -> (Option<String>, Vec<i18n::Artifact>) {
    let Ok(txt) = std::fs::read_to_string(path) else {
        return (None, Vec::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return (None, Vec::new());
    };
    let version = v
        .get("version")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let arts = v
        .get("artifacts")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    Some(i18n::Artifact {
                        platform: o.get("platform")?.as_str()?.to_string(),
                        arch: o.get("arch").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        file: o.get("file")?.as_str()?.to_string(),
                        size: o.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
                        sha256: o.get("sha256").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (version, arts)
}


async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sink, mut stream) = socket.split();
    // Bounded: a slow/stuck peer applies backpressure instead of growing memory.
    let (tx, mut rx) = mpsc::channel::<ServerMsg>(PEER_CHAN);

    // Writer task: serialize ServerMsg → WS text frames.
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let Ok(txt) = serde_json::to_string(&msg) else { continue };
            if sink.send(Message::Text(txt)).await.is_err() {
                break;
            }
        }
    });

    // (room, user_id) once this socket has joined.
    let mut me: Option<(String, String)> = None;
    // Sliding-window message rate limit, per connection.
    let mut win_start = Instant::now();
    let mut win_count: u32 = 0;

    // Idle timeout: any frame (text, ping, pong) counts as activity. A socket
    // that goes quiet longer than `ws_idle` is treated as dead and cleaned up
    // like a Leave, instead of lingering in the roster until TCP gives up.
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(state.ws_idle, stream.next()).await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        // Rate-limit: drop frames past the cap instead of growing relay queues.
        let now = Instant::now();
        if now.duration_since(win_start) > MSG_RATE_WINDOW {
            win_start = now;
            win_count = 0;
        }
        win_count += 1;
        if win_count > MSG_RATE_MAX {
            continue;
        }
        let cmsg: ClientMsg = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                // A join that does not even deserialize is almost always an
                // older build (no `proto`/`secret`). Name the real problem so
                // the user sees "update" instead of a serde field error.
                if me.is_none() && is_join_frame(&text) {
                    let _ = tx.try_send(protocol_mismatch_error(None));
                    break;
                }
                let _ = tx.try_send(ServerMsg::Error { code: "bad_json".into(), message: e.to_string() });
                continue;
            }
        };

        match cmsg {
            ClientMsg::Join { proto, room, user_id, name, token, secret } => {
                if me.is_some() {
                    let _ = tx.try_send(ServerMsg::Error {
                        code: "already_joined".into(),
                        message: "this socket already joined a room".into(),
                    });
                    continue;
                }
                if proto != PROTOCOL_VERSION {
                    let _ = tx.try_send(protocol_mismatch_error(Some(proto)));
                    break;
                }
                // Bound all client-supplied fields before doing anything with them.
                if !len_ok(&room, MAX_ID)
                    || !len_ok(&user_id, MAX_ID)
                    || !len_ok(&name, MAX_NAME)
                    || token.as_deref().map(|t| t.len() > MAX_TOKEN).unwrap_or(false)
                    || !len_ok(&secret, MAX_SECRET)
                {
                    let _ = tx.try_send(ServerMsg::Error {
                        code: "bad_input".into(),
                        message: "field empty or too long".into(),
                    });
                    break;
                }
                if !state.auth.check(&room, token.as_deref()) {
                    let _ = tx.try_send(ServerMsg::Error {
                        code: "bad_token".into(),
                        message: "invalid room token".into(),
                    });
                    break;
                }

                let size = {
                    let mut rooms = state.rooms.lock().unwrap();
                    let r = rooms.entry(room.clone()).or_default();
                    // Cap check (a rejoining same user_id doesn't count as growth).
                    if r.len() >= HARD_CAP && !r.contains_key(&user_id) {
                        let _ = tx.try_send(ServerMsg::RoomFull { cap: HARD_CAP });
                        drop(rooms);
                        break;
                    }
                    // Supersede only a connection that is ours (same secret);
                    // anyone else holding the id is refused.
                    let takeover_ok = r.get(&user_id).map(|old| old.secret == secret).unwrap_or(true);
                    if !takeover_ok {
                        let _ = tx.try_send(ServerMsg::Error {
                            code: ERR_ID_IN_USE.into(),
                            message: "user_id already taken in this room".into(),
                        });
                        drop(rooms);
                        break;
                    }
                    // Supersede a stale connection with the same user_id.
                    if let Some(old) = r.remove(&user_id) {
                        let _ = old.tx.try_send(ServerMsg::Error {
                            code: "superseded".into(),
                            message: "joined from another connection".into(),
                        });
                    }
                    let roster: Vec<PeerInfo> = r
                        .iter()
                        .map(|(id, h)| PeerInfo { user_id: id.clone(), name: h.name.clone() })
                        .collect();
                    r.insert(user_id.clone(), PeerHandle { name: name.clone(), tx: tx.clone(), secret: secret.clone() });
                    // Tell existing peers about the newcomer.
                    for (id, h) in r.iter() {
                        if id != &user_id {
                            let _ = h.tx.try_send(ServerMsg::PeerJoined {
                                user_id: user_id.clone(),
                                name: name.clone(),
                            });
                        }
                    }
                    let size = r.len();
                    // The joiner's first frame is always its Roster — before any
                    // room-wide Warn, so a client can rely on the order.
                    let _ = tx.try_send(ServerMsg::Roster { peers: roster });
                    // Soft-cap warning to everyone in the room.
                    if size >= WARN_CAP {
                        for h in r.values() {
                            let _ = h.tx.try_send(ServerMsg::Warn { size, cap: WARN_CAP });
                        }
                    }
                    size
                };

                if let Some(turn) = &state.turn {
                    let _ = tx.try_send(ServerMsg::Turn(turn.mint(&user_id)));
                }
                tracing::info!(%room, %user_id, size, "join");
                me = Some((room, user_id));
            }

            ClientMsg::Offer { to, sdp } => {
                if !len_ok(&to, MAX_ID) || sdp.len() > MAX_SDP {
                    continue;
                }
                relay_to(&state, &me, &to, |from| ServerMsg::Offer { from, sdp });
            }
            ClientMsg::Answer { to, sdp } => {
                if !len_ok(&to, MAX_ID) || sdp.len() > MAX_SDP {
                    continue;
                }
                relay_to(&state, &me, &to, |from| ServerMsg::Answer { from, sdp });
            }
            ClientMsg::Ice { to, candidate } => {
                if !len_ok(&to, MAX_ID) || candidate.len() > MAX_ICE {
                    continue;
                }
                relay_to(&state, &me, &to, |from| ServerMsg::Ice { from, candidate });
            }
            ClientMsg::Ptt { active } => {
                if let Some((room, from)) = &me {
                    let rooms = state.rooms.lock().unwrap();
                    if let Some(r) = rooms.get(room) {
                        for (id, h) in r.iter() {
                            if id != from {
                                let _ = h.tx.try_send(ServerMsg::Ptt {
                                    user_id: from.clone(),
                                    active,
                                });
                            }
                        }
                    }
                }
            }
            ClientMsg::Rekey => {
                // Throttle: a full-room rekey tears down every link, so one
                // member must not be able to spam it and interrupt audio.
                // Broadcast a key-rotation request to the whole room (incl. the
                // initiator) so every client re-handshakes together.
                if let Some((room, from)) = &me {
                    {
                        let mut rekeys = state.rekeys.lock().unwrap();
                        if rekeys.get(room).is_some_and(|t| now.duration_since(*t) < REKEY_COOLDOWN) {
                            continue;
                        }
                        rekeys.insert(room.clone(), now);
                    }
                    let rooms = state.rooms.lock().unwrap();
                    if let Some(r) = rooms.get(room) {
                        let by = r.get(from).map(|h| h.name.clone()).unwrap_or_default();
                        for h in r.values() {
                            let _ = h.tx.try_send(ServerMsg::Rekey { by: by.clone() });
                        }
                    }
                    tracing::info!(%room, %from, "rekey");
                }
            }
            ClientMsg::Leave => break,
        }
    }

    // Cleanup: drop from room, notify peers. Only if the room still holds THIS
    // socket's handle: a superseded socket must not evict the live connection
    // that replaced it under the same user_id.
    if let Some((room, user_id)) = me {
        let mut rooms = state.rooms.lock().unwrap();
        if let Some(r) = rooms.get_mut(&room) {
            let mine = r.get(&user_id).is_some_and(|h| h.tx.same_channel(&tx));
            if mine {
                r.remove(&user_id);
                for h in r.values() {
                    let _ = h.tx.try_send(ServerMsg::PeerLeft { user_id: user_id.clone() });
                }
                if r.is_empty() {
                    rooms.remove(&room);
                    state.rekeys.lock().unwrap().remove(&room);
                }
                tracing::info!(%room, %user_id, "leave");
            } else {
                tracing::info!(%room, %user_id, "stale socket closed (already superseded)");
            }
        }
    }
    // Let the writer flush a final error frame (e.g. id-in-use) before the
    // socket goes away: with our sender dropped it ends once the queue is
    // empty. Bounded, so a stuck peer cannot hold the task open.
    drop(tx);
    let mut writer = writer;
    if tokio::time::timeout(Duration::from_millis(500), &mut writer).await.is_err() {
        writer.abort();
    }
}

/// Relay a built message to a single peer (by user_id) in the sender's room,
/// stamping the sender's id as `from`.
fn relay_to(
    state: &AppState,
    me: &Option<(String, String)>,
    to: &str,
    build: impl FnOnce(String) -> ServerMsg,
) {
    let Some((room, from)) = me else { return };
    let rooms = state.rooms.lock().unwrap();
    if let Some(r) = rooms.get(room) {
        if let Some(h) = r.get(to) {
            let _ = h.tx.try_send(build(from.clone()));
        }
    }
}

#[cfg(test)]
mod ws_tests {
    //! Real sockets against the real router: these cover the join / supersede /
    //! cleanup path in `handle_socket`, which has no seams for unit tests.
    use super::*;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::tungstenite::Message as WsMsg;
    use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

    type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

    async fn spawn_server(ws_idle: Duration) -> (String, Arc<AppState>) {
        let state = Arc::new(AppState {
            rooms: Mutex::new(HashMap::new()),
            auth: AuthConfig::Open,
            turn: None,
            sessions: Sessions::default(),
            rate: RateLimiter::default(),
            ws_idle,
            rekeys: Mutex::new(HashMap::new()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        (format!("ws://{addr}/ws"), state)
    }

    /// Next ServerMsg, or None once the server has closed the socket.
    async fn next_msg(c: &mut Client) -> Option<ServerMsg> {
        loop {
            match tokio::time::timeout(Duration::from_secs(3), c.next()).await {
                Ok(Some(Ok(WsMsg::Text(t)))) => return Some(serde_json::from_str(&t).unwrap()),
                Ok(Some(Ok(WsMsg::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => return None,
                Ok(Some(Ok(_))) => continue,
                Err(_) => panic!("no frame within 3 s"),
            }
        }
    }

    /// Connect and join; returns once the Roster has arrived. The secret is
    /// derived from the user_id, so a second `join` for the same id is "the
    /// same client reconnecting".
    async fn join(url: &str, room: &str, user_id: &str) -> Client {
        join_with(url, room, user_id, &format!("secret-{user_id}")).await
    }

    fn join_msg(room: &str, user_id: &str, secret: &str) -> ClientMsg {
        ClientMsg::Join {
            proto: PROTOCOL_VERSION,
            room: room.into(),
            user_id: user_id.into(),
            name: user_id.into(),
            token: None,
            secret: secret.into(),
        }
    }

    async fn join_with(url: &str, room: &str, user_id: &str, secret: &str) -> Client {
        let (mut c, _) = connect_async(url).await.unwrap();
        let j = serde_json::to_string(&join_msg(room, user_id, secret)).unwrap();
        c.send(WsMsg::Text(j)).await.unwrap();
        match next_msg(&mut c).await {
            Some(ServerMsg::Roster { .. }) => c,
            other => panic!("expected Roster, got {other:?}"),
        }
    }

    /// Send a raw text frame on a fresh socket and return the first reply.
    async fn send_raw(url: &str, frame: &str) -> (Client, Option<ServerMsg>) {
        let (mut c, _) = connect_async(url).await.unwrap();
        c.send(WsMsg::Text(frame.into())).await.unwrap();
        let first = next_msg(&mut c).await;
        (c, first)
    }

    fn member_present(state: &AppState, room: &str, user_id: &str) -> bool {
        state.rooms.lock().unwrap().get(room).map(|r| r.contains_key(user_id)).unwrap_or(false)
    }

    #[tokio::test]
    async fn second_join_with_wrong_secret_is_rejected_with_id_in_use() {
        let (url, state) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join_with(&url, "r", "u", "s1").await;
        let frame = serde_json::to_string(&join_msg("r", "u", "s2")).unwrap();
        let (mut b, first) = send_raw(&url, &frame).await;
        match first {
            Some(ServerMsg::Error { code, .. }) => assert_eq!(code, ERR_ID_IN_USE),
            other => panic!("expected id-in-use, got {other:?}"),
        }
        assert!(next_msg(&mut b).await.is_none(), "socket must be closed after id-in-use");
        assert!(member_present(&state, "r", "u"));
        // a never heard a thing
        assert!(tokio::time::timeout(Duration::from_millis(300), a.next()).await.is_err());
    }

    #[tokio::test]
    async fn second_join_with_matching_secret_supersedes() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join_with(&url, "r", "u", "s1").await;
        let _b = join_with(&url, "r", "u", "s1").await;
        match next_msg(&mut a).await {
            Some(ServerMsg::Error { code, .. }) => assert_eq!(code, "superseded"),
            other => panic!("{other:?}"),
        }
    }

    /// A 0.3.x build sends a join with no `proto` and no `secret`. It must be
    /// told to update (not "bad_json") and the socket must close.
    #[tokio::test]
    async fn pre_v2_join_is_refused_with_protocol_mismatch() {
        let (url, state) = spawn_server(Duration::from_secs(30)).await;
        let old = r#"{"t":"join","room":"r","user_id":"u","name":"u"}"#;
        let (mut c, first) = send_raw(&url, old).await;
        match first {
            Some(ServerMsg::Error { code, message }) => {
                assert_eq!(code, ERR_PROTOCOL_MISMATCH);
                assert!(message.contains("update"), "{message}");
                assert!(message.contains("none"), "{message}");
            }
            other => panic!("expected protocol-mismatch, got {other:?}"),
        }
        assert!(next_msg(&mut c).await.is_none(), "socket must be closed");
        assert!(!member_present(&state, "r", "u"));
    }

    /// A join with an explicit but foreign `proto` (older or newer) is refused
    /// the same way, and the room never learns of it.
    #[tokio::test]
    async fn wrong_proto_is_refused_and_not_announced() {
        let (url, state) = spawn_server(Duration::from_secs(30)).await;
        let mut observer = join(&url, "r", "w").await;
        for proto in [PROTOCOL_VERSION - 1, PROTOCOL_VERSION + 1] {
            let frame = format!(
                r#"{{"t":"join","proto":{proto},"room":"r","user_id":"u","name":"u","secret":"s"}}"#
            );
            let (mut c, first) = send_raw(&url, &frame).await;
            match first {
                Some(ServerMsg::Error { code, message }) => {
                    assert_eq!(code, ERR_PROTOCOL_MISMATCH);
                    assert!(message.contains(&proto.to_string()), "{message}");
                }
                other => panic!("expected protocol-mismatch, got {other:?}"),
            }
            assert!(next_msg(&mut c).await.is_none(), "socket must be closed");
        }
        assert!(!member_present(&state, "r", "u"));
        assert!(tokio::time::timeout(Duration::from_millis(300), observer.next()).await.is_err());
    }

    /// Garbage that is not a join still gets the generic bad_json and keeps the
    /// socket open — only a join-shaped frame is classified as "too old".
    #[tokio::test]
    async fn non_join_garbage_is_bad_json_not_protocol_mismatch() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let (mut c, first) = send_raw(&url, r#"{"t":"offer"}"#).await;
        match first {
            Some(ServerMsg::Error { code, .. }) => assert_eq!(code, "bad_json"),
            other => panic!("expected bad_json, got {other:?}"),
        }
        // Socket still usable: a proper join now succeeds.
        let j = serde_json::to_string(&join_msg("r", "u", "s")).unwrap();
        c.send(WsMsg::Text(j)).await.unwrap();
        assert!(matches!(next_msg(&mut c).await, Some(ServerMsg::Roster { .. })));
    }

    #[tokio::test]
    async fn empty_secret_is_bad_input() {
        let (url, state) = spawn_server(Duration::from_secs(30)).await;
        let frame = serde_json::to_string(&join_msg("r", "u", "")).unwrap();
        let (_c, first) = send_raw(&url, &frame).await;
        assert!(matches!(first, Some(ServerMsg::Error { code, .. }) if code == "bad_input"));
        assert!(!member_present(&state, "r", "u"));
    }

    #[test]
    fn is_join_frame_classifies_by_tag_only() {
        assert!(is_join_frame(r#"{"t":"join"}"#));
        assert!(is_join_frame(r#"{"t":"join","room":"r","user_id":"u","name":"n"}"#));
        assert!(!is_join_frame(r#"{"t":"offer","to":"x","sdp":""}"#));
        assert!(!is_join_frame("not json"));
        assert!(!is_join_frame(r#"["t","join"]"#));
    }

    #[test]
    fn protocol_mismatch_error_names_both_versions() {
        match protocol_mismatch_error(Some(1)) {
            ServerMsg::Error { code, message } => {
                assert_eq!(code, ERR_PROTOCOL_MISMATCH);
                assert!(message.starts_with("protocol 1 "), "{message}");
                assert!(message.contains(&format!("speaks {PROTOCOL_VERSION}")), "{message}");
            }
            other => panic!("{other:?}"),
        }
        match protocol_mismatch_error(None) {
            ServerMsg::Error { message, .. } => assert!(message.starts_with("protocol none "), "{message}"),
            other => panic!("{other:?}"),
        }
    }

    /// A client reconnects while its old socket is still half-open. The old
    /// socket is superseded; when it finally dies, its cleanup must not evict
    /// the live replacement or announce a PeerLeft for it.
    #[tokio::test]
    async fn stale_superseded_socket_does_not_evict_the_live_member() {
        let (url, state) = spawn_server(Duration::from_secs(60)).await;
        let mut observer = join(&url, "r", "w").await;
        let mut old = join(&url, "r", "u").await;
        let mut fresh = join(&url, "r", "u").await;
        match next_msg(&mut old).await {
            Some(ServerMsg::Error { code, .. }) => assert_eq!(code, "superseded"),
            other => panic!("expected superseded error, got {other:?}"),
        }
        // observer saw u join twice
        for _ in 0..2 {
            match next_msg(&mut observer).await {
                Some(ServerMsg::PeerJoined { user_id, .. }) => assert_eq!(user_id, "u"),
                other => panic!("expected PeerJoined, got {other:?}"),
            }
        }

        old.close(None).await.unwrap();
        drop(old);
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(member_present(&state, "r", "u"), "live member evicted by the stale socket's cleanup");
        let leaked = tokio::time::timeout(Duration::from_millis(300), observer.next()).await;
        assert!(leaked.is_err(), "observer got a frame for the stale close: {leaked:?}");
        let leaked = tokio::time::timeout(Duration::from_millis(100), fresh.next()).await;
        assert!(leaked.is_err(), "live member got a frame for the stale close: {leaked:?}");
    }

    /// A socket that falls silent (half-open TCP, crashed client) must be
    /// dropped by the server and leave the room, so it stops counting toward
    /// the cap and stops holding the session open.
    #[tokio::test]
    async fn silent_socket_is_dropped_after_the_idle_timeout_and_leaves_the_room() {
        let (url, state) = spawn_server(Duration::from_millis(300)).await;
        let mut c = join(&url, "r", "u").await;
        assert!(member_present(&state, "r", "u"));
        let ended = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match c.next().await {
                    None | Some(Err(_)) | Some(Ok(WsMsg::Close(_))) => break,
                    _ => {}
                }
            }
        })
        .await
        .is_ok();
        assert!(ended, "server kept the idle socket open");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!member_present(&state, "r", "u"), "idle member still in the roster");
    }

    /// The engine sends `Leave` on shutdown; the server must drop the member at
    /// once instead of waiting for the idle timeout.
    #[tokio::test]
    async fn leave_removes_the_member_at_once() {
        let (url, state) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join(&url, "r", "u").await;
        a.send(WsMsg::Text(serde_json::to_string(&ClientMsg::Leave).unwrap())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!member_present(&state, "r", "u"));
    }

    /// One member must not be able to tear down every link in the room
    /// repeatedly by opening a second socket: the cooldown belongs to the room.
    #[tokio::test]
    async fn rekey_cooldown_is_per_room_not_per_socket() {
        let (url, _state) = spawn_server(Duration::from_secs(60)).await;
        let mut observer = join(&url, "r", "w").await;
        let mut a = join(&url, "r", "a").await;
        let mut b = join(&url, "r", "b").await;
        for _ in 0..2 {
            assert!(matches!(next_msg(&mut observer).await, Some(ServerMsg::PeerJoined { .. })));
        }
        assert!(matches!(next_msg(&mut a).await, Some(ServerMsg::PeerJoined { .. })));

        let rekey = serde_json::to_string(&ClientMsg::Rekey).unwrap();
        a.send(WsMsg::Text(rekey.clone())).await.unwrap();
        b.send(WsMsg::Text(rekey)).await.unwrap();

        let mut rekeys = 0;
        loop {
            match tokio::time::timeout(Duration::from_millis(500), observer.next()).await {
                Ok(Some(Ok(WsMsg::Text(t)))) => {
                    if matches!(serde_json::from_str::<ServerMsg>(&t).unwrap(), ServerMsg::Rekey { .. }) {
                        rekeys += 1;
                    }
                }
                Ok(Some(Ok(_))) => continue,
                _ => break,
            }
        }
        assert_eq!(rekeys, 1, "second socket bypassed the room's rekey cooldown");
    }

    /// The client heartbeat is a WS ping; it must count as activity.
    fn text(msg: &ClientMsg) -> WsMsg {
        WsMsg::Text(serde_json::to_string(msg).unwrap())
    }

    async fn expect_quiet(c: &mut Client) {
        assert!(tokio::time::timeout(Duration::from_millis(300), c.next()).await.is_err(), "unexpected frame");
    }

    #[tokio::test]
    async fn offer_answer_ice_are_relayed_to_the_target_with_the_sender_stamped() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join(&url, "r", "a").await;
        let mut b = join(&url, "r", "b").await;
        let mut c = join(&url, "r", "c").await;
        // a saw b and c join, b saw c join — drain those.
        for _ in 0..2 {
            assert!(matches!(next_msg(&mut a).await, Some(ServerMsg::PeerJoined { .. })));
        }
        assert!(matches!(next_msg(&mut b).await, Some(ServerMsg::PeerJoined { .. })));

        a.send(text(&ClientMsg::Offer { to: "b".into(), sdp: "v=0 offer".into() })).await.unwrap();
        match next_msg(&mut b).await {
            Some(ServerMsg::Offer { from, sdp }) => assert_eq!((from.as_str(), sdp.as_str()), ("a", "v=0 offer")),
            other => panic!("{other:?}"),
        }
        b.send(text(&ClientMsg::Answer { to: "a".into(), sdp: "v=0 answer".into() })).await.unwrap();
        match next_msg(&mut a).await {
            Some(ServerMsg::Answer { from, sdp }) => assert_eq!((from.as_str(), sdp.as_str()), ("b", "v=0 answer")),
            other => panic!("{other:?}"),
        }
        a.send(text(&ClientMsg::Ice { to: "b".into(), candidate: "cand".into() })).await.unwrap();
        match next_msg(&mut b).await {
            Some(ServerMsg::Ice { from, candidate }) => assert_eq!((from.as_str(), candidate.as_str()), ("a", "cand")),
            other => panic!("{other:?}"),
        }
        // The third member never hears a word of the pair's exchange.
        expect_quiet(&mut c).await;
    }

    #[tokio::test]
    async fn relay_to_an_unknown_peer_or_with_an_oversized_payload_is_dropped_silently() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join(&url, "r", "a").await;
        let mut b = join(&url, "r", "b").await;
        assert!(matches!(next_msg(&mut a).await, Some(ServerMsg::PeerJoined { .. })));

        a.send(text(&ClientMsg::Offer { to: "nobody".into(), sdp: "v=0".into() })).await.unwrap();
        a.send(text(&ClientMsg::Offer { to: "b".into(), sdp: "x".repeat(MAX_SDP + 1) })).await.unwrap();
        a.send(text(&ClientMsg::Ice { to: "b".into(), candidate: "x".repeat(MAX_ICE + 1) })).await.unwrap();
        a.send(text(&ClientMsg::Offer { to: "".into(), sdp: "v=0".into() })).await.unwrap();
        expect_quiet(&mut b).await;
        expect_quiet(&mut a).await;
        // The socket is still alive: a well-formed relay goes through afterwards.
        a.send(text(&ClientMsg::Ice { to: "b".into(), candidate: "ok".into() })).await.unwrap();
        assert!(matches!(next_msg(&mut b).await, Some(ServerMsg::Ice { candidate, .. }) if candidate == "ok"));
    }

    #[tokio::test]
    async fn signaling_before_join_is_ignored_and_the_socket_stays_open() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let _b = join(&url, "r", "b").await;
        let (mut c, _) = connect_async(&url).await.unwrap();
        c.send(text(&ClientMsg::Offer { to: "b".into(), sdp: "v=0".into() })).await.unwrap();
        c.send(text(&ClientMsg::Ptt { active: true })).await.unwrap();
        c.send(text(&ClientMsg::Rekey)).await.unwrap();
        expect_quiet(&mut c).await;
        c.send(text(&join_msg("r", "c", "s"))).await.unwrap();
        assert!(matches!(next_msg(&mut c).await, Some(ServerMsg::Roster { peers }) if peers.len() == 1));
    }

    #[tokio::test]
    async fn a_second_join_on_the_same_socket_is_refused() {
        let (url, state) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join(&url, "r", "a").await;
        a.send(text(&join_msg("r", "a2", "s"))).await.unwrap();
        match next_msg(&mut a).await {
            Some(ServerMsg::Error { code, .. }) => assert_eq!(code, "already_joined"),
            other => panic!("{other:?}"),
        }
        assert!(member_present(&state, "r", "a"));
        assert!(!member_present(&state, "r", "a2"));
    }

    #[tokio::test]
    async fn ptt_is_broadcast_to_everyone_but_the_speaker() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join(&url, "r", "a").await;
        let mut b = join(&url, "r", "b").await;
        assert!(matches!(next_msg(&mut a).await, Some(ServerMsg::PeerJoined { .. })));
        a.send(text(&ClientMsg::Ptt { active: true })).await.unwrap();
        match next_msg(&mut b).await {
            Some(ServerMsg::Ptt { user_id, active }) => assert_eq!((user_id.as_str(), active), ("a", true)),
            other => panic!("{other:?}"),
        }
        expect_quiet(&mut a).await;
    }

    #[tokio::test]
    async fn the_roster_lists_everyone_already_present_but_not_the_joiner() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let _a = join(&url, "r", "a").await;
        let _b = join(&url, "r", "b").await;
        let (mut c, _) = connect_async(&url).await.unwrap();
        c.send(text(&join_msg("r", "c", "s"))).await.unwrap();
        match next_msg(&mut c).await {
            Some(ServerMsg::Roster { peers }) => {
                let mut ids: Vec<&str> = peers.iter().map(|p| p.user_id.as_str()).collect();
                ids.sort();
                assert_eq!(ids, ["a", "b"]);
                assert!(peers.iter().all(|p| p.name == p.user_id), "fixture uses id as name");
            }
            other => panic!("{other:?}"),
        }
        // Rooms are isolated: a joiner elsewhere sees an empty roster.
        let (mut d, _) = connect_async(&url).await.unwrap();
        d.send(text(&join_msg("elsewhere", "d", "s"))).await.unwrap();
        assert!(matches!(next_msg(&mut d).await, Some(ServerMsg::Roster { peers }) if peers.is_empty()));
    }

    #[tokio::test]
    async fn the_warn_cap_is_announced_to_the_whole_room() {
        let (url, _) = spawn_server(Duration::from_secs(60)).await;
        let mut first = join(&url, "r", "m0").await;
        let mut others = Vec::new();
        for i in 1..WARN_CAP {
            others.push(join(&url, "r", &format!("m{i}")).await);
        }
        // `first` saw WARN_CAP-1 joins and then exactly one Warn for the 12th.
        let mut warns = 0;
        for _ in 1..WARN_CAP {
            assert!(matches!(next_msg(&mut first).await, Some(ServerMsg::PeerJoined { .. })));
        }
        match next_msg(&mut first).await {
            Some(ServerMsg::Warn { size, cap }) => {
                assert_eq!((size, cap), (WARN_CAP, WARN_CAP));
                warns += 1;
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(warns, 1);
        // The 12th joiner itself gets Roster then Warn.
        let last = others.last_mut().unwrap();
        assert!(matches!(next_msg(last).await, Some(ServerMsg::Warn { size, .. }) if size == WARN_CAP));
    }

    #[tokio::test]
    async fn the_hard_cap_refuses_the_next_joiner_and_closes_the_socket() {
        let (url, state) = spawn_server(Duration::from_secs(60)).await;
        let mut members = Vec::new();
        for i in 0..HARD_CAP {
            members.push(join(&url, "r", &format!("m{i}")).await);
        }
        let frame = serde_json::to_string(&join_msg("r", "late", "s")).unwrap();
        let (mut late, first) = send_raw(&url, &frame).await;
        assert!(matches!(first, Some(ServerMsg::RoomFull { cap }) if cap == HARD_CAP), "{first:?}");
        assert!(next_msg(&mut late).await.is_none(), "socket must be closed after RoomFull");
        assert!(!member_present(&state, "r", "late"));
        assert_eq!(state.rooms.lock().unwrap()["r"].len(), HARD_CAP);
        // A member reconnecting with its own id is not growth and still works.
        let _again = join_with(&url, "r", "m0", "secret-m0").await;
    }

    #[tokio::test]
    async fn rekey_is_broadcast_to_the_whole_room_including_the_initiator() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join(&url, "r", "a").await;
        let mut b = join(&url, "r", "b").await;
        assert!(matches!(next_msg(&mut a).await, Some(ServerMsg::PeerJoined { .. })));
        b.send(text(&ClientMsg::Rekey)).await.unwrap();
        for c in [&mut a, &mut b] {
            match next_msg(c).await {
                Some(ServerMsg::Rekey { by }) => assert_eq!(by, "b", "by carries the display name"),
                other => panic!("{other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn leaving_empties_the_room_and_clears_its_rekey_state() {
        let (url, state) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join(&url, "r", "a").await;
        a.send(text(&ClientMsg::Rekey)).await.unwrap();
        assert!(matches!(next_msg(&mut a).await, Some(ServerMsg::Rekey { .. })));
        assert!(state.rekeys.lock().unwrap().contains_key("r"));
        a.send(text(&ClientMsg::Leave)).await.unwrap();
        assert!(next_msg(&mut a).await.is_none(), "server closes after Leave");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!state.rooms.lock().unwrap().contains_key("r"), "empty room is dropped");
        assert!(!state.rekeys.lock().unwrap().contains_key("r"));
    }

    #[tokio::test]
    async fn peers_are_told_when_a_member_leaves() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join(&url, "r", "a").await;
        let mut b = join(&url, "r", "b").await;
        assert!(matches!(next_msg(&mut a).await, Some(ServerMsg::PeerJoined { .. })));
        b.send(text(&ClientMsg::Leave)).await.unwrap();
        assert!(matches!(next_msg(&mut a).await, Some(ServerMsg::PeerLeft { user_id }) if user_id == "b"));
        // Dropping the socket without Leave has the same effect.
        let mut c = join(&url, "r", "c").await;
        assert!(matches!(next_msg(&mut a).await, Some(ServerMsg::PeerJoined { .. })));
        c.close(None).await.unwrap();
        assert!(matches!(next_msg(&mut a).await, Some(ServerMsg::PeerLeft { user_id }) if user_id == "c"));
    }

    /// A member is authenticated, not trusted: a flood of frames must be
    /// dropped past the per-second budget instead of fanning out to the room.
    #[tokio::test]
    async fn frames_past_the_rate_budget_are_dropped() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join(&url, "r", "a").await;
        let mut b = join(&url, "r", "b").await;
        assert!(matches!(next_msg(&mut a).await, Some(ServerMsg::PeerJoined { .. })));
        let burst = (MSG_RATE_MAX * 2) as usize;
        for _ in 0..burst {
            a.send(text(&ClientMsg::Ptt { active: true })).await.unwrap();
        }
        let mut got = 0;
        while tokio::time::timeout(Duration::from_millis(300), b.next()).await.map(|m| m.is_some()).unwrap_or(false) {
            got += 1;
        }
        // The join frame already used one slot. Even if the burst straddles a
        // window boundary it cannot all get through.
        assert!(got < burst, "nothing was dropped: {got}");
        assert!(got >= (MSG_RATE_MAX - 1) as usize, "too much dropped: {got}");
    }

    #[tokio::test]
    async fn oversized_ws_frames_end_the_connection() {
        let (url, state) = spawn_server(Duration::from_secs(30)).await;
        let mut a = join(&url, "r", "a").await;
        let huge = format!(r#"{{"t":"offer","to":"b","sdp":"{}"}}"#, "x".repeat(MAX_WS_MSG + 1));
        let _ = a.send(WsMsg::Text(huge)).await;
        assert!(next_msg(&mut a).await.is_none(), "the server drops the socket");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!member_present(&state, "r", "a"));
    }

    // ── REST + pages over a real TCP socket ─────────────────────────────────

    /// Minimal HTTP/1.1 client: one request, `Connection: close`, returns
    /// (status, lowercase headers, body). Enough for JSON and HTML responses.
    async fn http(ws_url: &str, method: &str, path: &str, extra_headers: &str, body: &str) -> (u16, String, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let addr = ws_url.trim_start_matches("ws://").trim_end_matches("/ws");
        let mut s = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n{extra_headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let raw = String::from_utf8_lossy(&buf).to_string();
        let (head, mut body) = raw.split_once("\r\n\r\n").map(|(h, b)| (h.to_string(), b.to_string())).unwrap();
        let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        let headers = head.to_lowercase();
        if headers.contains("transfer-encoding: chunked") {
            let mut out = String::new();
            let mut rest = body.as_str();
            loop {
                let (len, after) = rest.split_once("\r\n").unwrap();
                let n = usize::from_str_radix(len.trim(), 16).unwrap();
                if n == 0 {
                    break;
                }
                out.push_str(&after[..n]);
                rest = &after[n + 2..];
            }
            body = out;
        }
        (status, headers, body)
    }

    async fn post_json(ws_url: &str, path: &str, json: &str) -> (u16, serde_json::Value) {
        let (st, _, body) = http(ws_url, "POST", path, "Content-Type: application/json\r\n", json).await;
        (st, serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
    }

    #[tokio::test]
    async fn healthz_answers_ok() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let (st, _, body) = http(&url, "GET", "/healthz", "", "").await;
        assert_eq!((st, body.as_str()), (200, "ok"));
    }

    #[tokio::test]
    async fn create_session_then_join_with_the_pin_resolves_the_room() {
        let (url, state) = spawn_server(Duration::from_secs(30)).await;
        let (st, v) = post_json(&url, "/session", "{}").await;
        assert_eq!(st, 200, "{v}");
        let code = v["code"].as_str().unwrap();
        let pin = v["pin"].as_str().unwrap();
        let room = v["room"].as_str().unwrap();
        assert_eq!(code.len(), 8);
        assert_eq!(pin.len(), 6);
        assert!(room.starts_with("squad-"));
        assert!(v["token"].is_null(), "open-auth server mints no token");
        assert_eq!(v["ws"], public_ws());
        assert_eq!(v["link"], format!("{}/j/{code}", public_base()));

        let (st, j) = post_json(&url, &format!("/session/{code}/join"), &format!(r#"{{"pin":"{pin}"}}"#)).await;
        assert_eq!(st, 200, "{j}");
        assert_eq!(j["room"], room);
        assert_eq!(j["ws"], public_ws());
        assert!(j["token"].is_null());
        // The session store agrees.
        assert!(state.sessions.join(code, pin).is_ok());
    }

    #[tokio::test]
    async fn join_session_errors_map_to_status_codes() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let (_, v) = post_json(&url, "/session", "{}").await;
        let code = v["code"].as_str().unwrap().to_string();
        let pin = v["pin"].as_str().unwrap().to_string();
        let wrong = if pin == "000000" { "111111" } else { "000000" };

        let (st, j) = post_json(&url, "/session/nosuch12/join", r#"{"pin":"123456"}"#).await;
        assert_eq!((st, j["error"].as_str()), (404, Some("not_found")));
        let (st, j) = post_json(&url, &format!("/session/{code}/join"), &format!(r#"{{"pin":"{wrong}"}}"#)).await;
        assert_eq!((st, j["error"].as_str()), (403, Some("bad_pin")));
        let (st, j) = post_json(&url, &format!("/session/{code}/join"), r#"{"nopin":true}"#).await;
        assert_eq!((st, j["error"].as_str()), (404, Some("not_found")), "missing pin is bounded out before the store");
        let (st, j) = post_json(&url, &format!("/session/{code}/join"), r#"{"pin":"1234567890123"}"#).await;
        assert_eq!((st, j["error"].as_str()), (404, Some("not_found")), "over-long pin");
        // Burn the remaining guesses → locked, and the right PIN no longer helps.
        for _ in 0..5 {
            let _ = post_json(&url, &format!("/session/{code}/join"), &format!(r#"{{"pin":"{wrong}"}}"#)).await;
        }
        let (st, j) = post_json(&url, &format!("/session/{code}/join"), &format!(r#"{{"pin":"{pin}"}}"#)).await;
        assert_eq!((st, j["error"].as_str()), (429, Some("locked")));
    }

    #[tokio::test]
    async fn session_creation_is_rate_limited_per_ip() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        for i in 0..RL_CREATE_MAX {
            let (st, _) = post_json(&url, "/session", "{}").await;
            assert_eq!(st, 200, "creation {i}");
        }
        let (st, v) = post_json(&url, "/session", "{}").await;
        assert_eq!((st, v["error"].as_str()), (429, Some("rate_limited")));
    }

    #[tokio::test]
    async fn rest_bodies_over_the_limit_are_refused() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let big = format!(r#"{{"pin":"1","pad":"{}"}}"#, "x".repeat(MAX_REST_BODY + 1));
        let (st, _, _) = http(&url, "POST", "/session/abcd2345/join", "Content-Type: application/json\r\n", &big).await;
        assert_eq!(st, 413);
    }

    #[tokio::test]
    async fn landing_page_shows_the_code_escaped_and_truncated() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let (st, _, body) = http(&url, "GET", "/j/abcd2345?lang=de", "", "").await;
        assert_eq!(st, 200);
        assert!(body.contains("<html lang=\"de\""));
        assert!(body.contains("<p class=\"code\">abcd2345</p>"));
        let (st, _, body) = http(&url, "GET", "/j/%3Cb%3Ex", "", "").await;
        assert_eq!(st, 200);
        assert!(body.contains("<p class=\"code\">&lt;b&gt;x</p>"), "{body}");
        assert!(!body.contains("<b>x"));
        let long = "a".repeat(40);
        let (_, _, body) = http(&url, "GET", &format!("/j/{long}"), "", "").await;
        assert!(body.contains(&format!("<p class=\"code\">{}</p>", "a".repeat(32))));
    }

    #[tokio::test]
    async fn pages_render_in_the_requested_language_and_remember_an_explicit_size() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        let (st, headers, body) = http(&url, "GET", "/?lang=fr&ui=xl", "", "").await;
        assert_eq!(st, 200);
        assert!(body.starts_with("<!doctype html><html lang=\"fr\" data-ui=\"xl\">"));
        assert!(headers.contains("set-cookie: subraum_ui=xl;"), "{headers}");
        // Accept-Language alone picks the language but sets no cookie.
        let (_, headers, body) = http(&url, "GET", "/", "Accept-Language: it-IT,it;q=0.8\r\nCookie: subraum_ui=lg\r\n", "").await;
        assert!(body.contains("<html lang=\"it\" data-ui=\"lg\">"));
        assert!(!headers.contains("set-cookie"), "{headers}");
        for (path, needle) in [
            ("/privacy", "<h1>Privacy</h1>"),
            ("/legal", "<h1>Legal notice</h1>"),
            ("/license", "PolyForm Noncommercial License 1.0.0"),
            ("/changelog", "<h1>Changelog</h1>"),
            ("/get", "<h1>Downloads</h1>"),
        ] {
            let (st, headers, body) = http(&url, "GET", path, "", "").await;
            assert_eq!(st, 200, "{path}");
            assert!(headers.contains("content-type: text/html"), "{path}: {headers}");
            assert!(body.contains(needle), "{path} lacks {needle}");
        }
    }

    #[tokio::test]
    async fn brand_assets_are_served_with_types_and_caching_and_unknown_ones_are_404() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        for (path, ctype) in [
            ("/assets/logo.svg", "image/svg+xml"),
            ("/assets/rdoc.svg", "image/svg+xml"),
            ("/assets/rdoc-light.svg", "image/svg+xml"),
            ("/assets/og-image.png", "image/png"),
            ("/assets/shot/1", "image/png"),
            ("/assets/shot/5", "image/png"),
            ("/assets/fonts/mi-400.woff2", "font/woff2"),
        ] {
            let (st, headers, body) = http(&url, "GET", path, "", "").await;
            assert_eq!(st, 200, "{path}");
            assert!(headers.contains(&format!("content-type: {ctype}")), "{path}: {headers}");
            assert!(headers.contains(&format!("cache-control: {ASSET_CACHE}")), "{path}: {headers}");
            assert!(!body.is_empty(), "{path}");
        }
        for path in ["/assets/shot/0", "/assets/shot/6", "/assets/fonts/nope.woff2", "/nope"] {
            let (st, _, _) = http(&url, "GET", path, "", "").await;
            assert_eq!(st, 404, "{path}");
        }
        // A non-numeric shot index is rejected by the path extractor itself.
        let (st, _, _) = http(&url, "GET", "/assets/shot/x", "", "").await;
        assert_eq!(st, 400);
    }

    #[tokio::test]
    async fn pngs_really_are_pngs() {
        let (url, _) = spawn_server(Duration::from_secs(30)).await;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let addr = url.trim_start_matches("ws://").trim_end_matches("/ws");
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(b"GET /assets/og-image.png HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let sep = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert_eq!(&buf[sep..sep + 8], b"\x89PNG\r\n\x1a\n");
    }

    #[tokio::test]
    async fn pings_keep_a_quiet_socket_alive() {
        let (url, state) = spawn_server(Duration::from_millis(300)).await;
        let mut c = join(&url, "r", "u").await;
        for _ in 0..8 {
            c.send(WsMsg::Ping(vec![])).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            // drain pongs so the read side keeps moving
            while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(10), c.next()).await {}
        }
        assert!(member_present(&state, "r", "u"), "pinging member was dropped as idle");
    }
}

#[cfg(test)]
mod env_flag_tests {
    use super::flag_on;

    /// `TLS_DISABLE=0` used to disable TLS: the check was `is_ok()`, so any
    /// value — including "0" or "" — counted as set.
    #[test]
    fn only_one_or_true_switch_a_flag_on() {
        for on in ["1", "true", "TRUE", " true "] {
            assert!(flag_on(on), "{on:?} should be on");
        }
        for off in ["0", "", "false", "no", "yes", "2"] {
            assert!(!flag_on(off), "{off:?} should be off");
        }
    }
}

#[cfg(test)]
mod client_ip_tests {
    use super::*;

    fn hdr(v: &str) -> HeaderMap { let mut h = HeaderMap::new(); h.insert("x-forwarded-for", v.parse().unwrap()); h }
    fn lo() -> SocketAddr { "127.0.0.1:9".parse().unwrap() }

    #[test]
    fn xff_uses_the_last_hop() {
        assert_eq!(client_ip(&hdr("6.6.6.6, 1.2.3.4"), lo()), "1.2.3.4");
    }
    #[test]
    fn xff_ignores_trailing_empty_hop() {
        assert_eq!(client_ip(&hdr("1.2.3.4, "), lo()), "1.2.3.4");
        assert_eq!(client_ip(&hdr("  "), lo()), "127.0.0.1");
    }
    #[test]
    fn xff_from_a_non_loopback_peer_is_ignored() {
        let peer: SocketAddr = "9.9.9.9:1".parse().unwrap();
        assert_eq!(client_ip(&hdr("1.2.3.4"), peer), "9.9.9.9");
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    // ── Input bounds and escaping ────────────────────────────────────────────

    #[test]
    fn len_ok_rejects_empty_and_over_long_by_bytes() {
        assert!(!len_ok("", 10));
        assert!(len_ok("a", 1));
        assert!(len_ok(&"x".repeat(64), 64));
        assert!(!len_ok(&"x".repeat(65), 64));
        // Byte length, not char count: two umlauts are four bytes.
        assert!(len_ok("ää", 4));
        assert!(!len_ok("ää", 3));
    }

    #[test]
    fn esc_covers_every_reflected_character() {
        assert_eq!(esc("<>&\"'"), "&lt;&gt;&amp;&quot;&#x27;");
        assert_eq!(esc("abcd2345"), "abcd2345");
        // Ampersand first, so already-escaped sequences are not double-decoded.
        assert_eq!(esc("&lt;"), "&amp;lt;");
    }

    // ── Per-IP rate limiter ──────────────────────────────────────────────────

    #[test]
    fn rate_limiter_allows_max_then_denies_within_the_window() {
        let rl = RateLimiter::default();
        for i in 0..3 {
            assert!(rl.allow("1.2.3.4", 3, 300), "call {i} is within budget");
        }
        assert!(!rl.allow("1.2.3.4", 3, 300));
        assert!(!rl.allow("1.2.3.4", 3, 300), "stays denied, the counter keeps growing");
    }

    #[test]
    fn rate_limiter_counts_per_ip() {
        let rl = RateLimiter::default();
        assert!(rl.allow("a", 1, 300));
        assert!(!rl.allow("a", 1, 300));
        assert!(rl.allow("b", 1, 300), "another IP has its own budget");
    }

    #[test]
    fn rate_limiter_zero_window_resets_every_call() {
        let rl = RateLimiter::default();
        for _ in 0..10 {
            assert!(rl.allow("a", 1, 0));
        }
    }

    #[test]
    fn prune_drops_expired_windows_and_keeps_live_ones() {
        let rl = RateLimiter::default();
        assert!(rl.allow("a", 5, 300));
        rl.prune(300);
        assert_eq!(rl.inner.lock().unwrap().len(), 1, "a fresh window survives");
        rl.prune(0);
        assert!(rl.inner.lock().unwrap().is_empty(), "window 0 means everything is expired");
        // After a prune the IP starts over.
        assert!(rl.allow("a", 1, 300));
    }

    // ── Language / size selection from a request ─────────────────────────────

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    #[test]
    fn lang_of_reads_the_lang_query_param_anywhere_in_the_string() {
        assert_eq!(lang_of(&Some("ui=lg&lang=de".into()), &HeaderMap::new()), Lang::De);
        assert_eq!(lang_of(&Some("lang=fr".into()), &headers(&[("accept-language", "de")])), Lang::Fr);
        assert_eq!(lang_of(&Some("language=fr".into()), &HeaderMap::new()), Lang::En, "only the exact key counts");
        assert_eq!(lang_of(&None, &headers(&[("accept-language", "it-IT,it;q=0.9")])), Lang::It);
        assert_eq!(lang_of(&None, &HeaderMap::new()), Lang::En);
    }

    #[test]
    fn ui_of_prefers_query_then_cookie_and_reports_an_explicit_choice() {
        assert_eq!(ui_of(&Some("ui=xl".into()), &HeaderMap::new()), (Ui::Larger, true));
        assert_eq!(ui_of(&Some("lang=de&ui=lg".into()), &headers(&[("cookie", "subraum_ui=xl")])), (Ui::Large, true));
        assert_eq!(ui_of(&None, &headers(&[("cookie", "a=b; subraum_ui=xl; c=d")])), (Ui::Larger, false));
        assert_eq!(ui_of(&None, &headers(&[("cookie", "subraum_ui=junk")])), (Ui::Normal, false));
        assert_eq!(ui_of(&None, &headers(&[("cookie", "other_ui=xl")])), (Ui::Normal, false), "prefix match must be exact");
        assert_eq!(ui_of(&None, &HeaderMap::new()), (Ui::Normal, false));
        // A bad query value still counts as "chosen" (the cookie gets rewritten to normal).
        assert_eq!(ui_of(&Some("ui=huge".into()), &HeaderMap::new()), (Ui::Normal, true));
    }

    #[test]
    fn ui_cookie_is_a_year_long_lax_http_only_cookie_holding_only_the_size() {
        let c = ui_cookie(Ui::Large);
        assert!(c.starts_with("subraum_ui=lg; "), "{c}");
        for attr in ["Path=/", "Max-Age=31536000", "SameSite=Lax", "HttpOnly"] {
            assert!(c.contains(attr), "{c} lacks {attr}");
        }
        assert_eq!(c.matches(';').count(), if c.contains("Secure") { 5 } else { 4 }, "no extra attributes: {c}");
    }

    // ── Changelog rendering ──────────────────────────────────────────────────

    #[test]
    fn render_changelog_maps_headings_bullets_and_inline_marks() {
        let md = "# Changelog\n\n## v1.0 — 2026-01-01\n\n### Behoben\n- **Fett** und `code` <b>\n  Fortsetzung der Zeile.\n- Zweiter Punkt\n\nEin Absatz.\n";
        let html = render_changelog(md);
        assert!(html.starts_with("<h1>Changelog</h1>"));
        assert_eq!(html.matches("<h1>").count(), 1, "the markdown title is skipped");
        assert!(html.contains("<h2 class=\"ver\">v1.0 — 2026-01-01</h2>"));
        assert!(html.contains("<h3>Behoben</h3>"));
        assert!(html.contains("<li><strong>Fett</strong> und <code>code</code> &lt;b&gt; Fortsetzung der Zeile.</li>"));
        assert!(html.contains("<li>Zweiter Punkt</li>"));
        assert!(html.contains("<p class=\"muted\">Ein Absatz.</p>"));
        assert_eq!(html.matches("<ul>").count(), 1);
        assert_eq!(html.matches("</ul>").count(), 1);
    }

    #[test]
    fn render_changelog_closes_lists_at_headings_and_blank_lines() {
        let html = render_changelog("- a\n## v2\n- b\n\n- c\n### s\n");
        assert_eq!(html.matches("<ul>").count(), 3);
        assert_eq!(html.matches("</ul>").count(), 3);
        assert!(html.ends_with("<h3>s</h3>"), "{html}");
    }

    #[test]
    fn render_changelog_leaves_unbalanced_marks_literal_and_escapes_html() {
        let html = render_changelog("- a `tick and **half\n");
        assert!(html.contains("<li>a `tick and **half</li>"), "{html}");
        let html = render_changelog("- <script>alert(1)</script> & co\n");
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt; &amp; co"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn render_changelog_handles_the_real_changelog() {
        // The embedded CHANGELOG.md must render without a panic and keep every
        // version heading.
        let html = render_changelog(CHANGELOG_MD);
        let versions = CHANGELOG_MD.lines().filter(|l| l.starts_with("## ")).count();
        assert_eq!(html.matches("<h2 class=\"ver\">").count(), versions);
        assert!(versions >= 5);
    }

    // ── Manifest parsing ─────────────────────────────────────────────────────

    fn scratch_file(tag: &str, content: Option<&str>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "subraum-manifest-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("manifest.json");
        if let Some(c) = content {
            std::fs::write(&p, c).unwrap();
        }
        p
    }

    #[test]
    fn missing_or_invalid_manifest_degrades_to_empty() {
        let p = scratch_file("missing", None);
        let (v, a) = parse_manifest(&p);
        assert!(v.is_none() && a.is_empty());
        let p = scratch_file("invalid", Some("{not json"));
        let (v, a) = parse_manifest(&p);
        assert!(v.is_none() && a.is_empty());
        let p = scratch_file("wrongshape", Some("[1,2,3]"));
        let (v, a) = parse_manifest(&p);
        assert!(v.is_none() && a.is_empty());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn manifest_entries_need_platform_and_file_and_default_the_rest() {
        let json = r#"{
          "version": "0.4.0",
          "artifacts": [
            {"platform":"windows","arch":"x64","file":"s.exe","size":123,"sha256":"ab"},
            {"platform":"linux","file":"s.deb"},
            {"platform":"macos"},
            {"file":"orphan.bin"},
            {"platform":"android","file":"s.apk","size":"big"}
          ]
        }"#;
        let p = scratch_file("ok", Some(json));
        let (v, a) = parse_manifest(&p);
        assert_eq!(v.as_deref(), Some("0.4.0"));
        assert_eq!(a.len(), 3, "entries without platform or file are skipped");
        assert_eq!((a[0].platform.as_str(), a[0].arch.as_str(), a[0].size, a[0].sha256.as_str()), ("windows", "x64", 123, "ab"));
        assert_eq!((a[1].platform.as_str(), a[1].arch.as_str(), a[1].size, a[1].sha256.as_str()), ("linux", "", 0, ""));
        assert_eq!(a[2].size, 0, "non-numeric size defaults to 0");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn empty_version_string_counts_as_unknown() {
        let p = scratch_file("nover", Some(r#"{"version":"","artifacts":[]}"#));
        let (v, a) = parse_manifest(&p);
        assert!(v.is_none());
        assert!(a.is_empty());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    // ── Page shell ───────────────────────────────────────────────────────────

    #[test]
    fn shell_sets_language_size_title_csp_and_wraps_the_body() {
        let Html(html) = shell(Lang::De, Ui::Larger, "/get", "Downloads", "<p>BODY</p>");
        assert!(html.starts_with("<!doctype html><html lang=\"de\" data-ui=\"xl\">"));
        assert!(html.contains("<title>Downloads | subraum</title>"));
        assert!(html.contains(HTML_CSP));
        assert!(html.contains(&format!("content=\"{}\"", i18n::meta_desc(Lang::De))));
        assert!(html.contains("<main><p>BODY</p></main>"));
        assert!(html.contains("og:image\" content=\"https://subraum.cc/assets/og-image.png\""));
        assert!(html.contains("href=\"/get?lang=de&amp;ui=xl\" class=\"on\">DE</a>"), "switcher keeps path + size");
        assert!(html.contains("<a class=\"brand\" href=\"/?lang=de\">subraum</a>"));
        assert!(html.ends_with("</footer></body></html>"));
    }

    #[test]
    fn footer_links_every_page_in_the_current_language() {
        let f = footer("https://x.test", Lang::Fr);
        assert!(f.contains("href=\"https://x.test/get?lang=fr\">Télécharger</a>"));
        for path in ["/privacy", "/legal", "/license", "/changelog"] {
            assert!(f.contains(&format!("href=\"{path}?lang=fr\"")), "{path}");
        }
        assert!(f.contains(i18n::GITHUB_URL) && f.contains(i18n::RAUMDOCK_URL));
        assert!(f.contains("src=\"/assets/rdoc.svg\"") && f.contains("src=\"/assets/rdoc-light.svg\""));
    }

    // ── relay_to ─────────────────────────────────────────────────────────────

    fn state_with_room(room: &str, members: &[&str]) -> (Arc<AppState>, Vec<mpsc::Receiver<ServerMsg>>) {
        let state = Arc::new(AppState {
            rooms: Mutex::new(HashMap::new()),
            auth: AuthConfig::Open,
            turn: None,
            sessions: Sessions::default(),
            rate: RateLimiter::default(),
            ws_idle: WS_IDLE_TIMEOUT,
            rekeys: Mutex::new(HashMap::new()),
        });
        let mut rxs = Vec::new();
        {
            let mut rooms = state.rooms.lock().unwrap();
            let r = rooms.entry(room.to_string()).or_default();
            for m in members {
                let (tx, rx) = mpsc::channel(8);
                r.insert(m.to_string(), PeerHandle { name: m.to_uppercase(), tx, secret: format!("s-{m}") });
                rxs.push(rx);
            }
        }
        (state, rxs)
    }

    #[test]
    fn relay_to_stamps_the_sender_and_reaches_only_the_target() {
        let (state, mut rxs) = state_with_room("r", &["a", "b", "c"]);
        let me = Some(("r".to_string(), "a".to_string()));
        relay_to(&state, &me, "b", |from| ServerMsg::Offer { from, sdp: "v=0".into() });
        match rxs[1].try_recv() {
            Ok(ServerMsg::Offer { from, sdp }) => assert_eq!((from.as_str(), sdp.as_str()), ("a", "v=0")),
            other => panic!("{other:?}"),
        }
        assert!(rxs[0].try_recv().is_err(), "sender gets nothing back");
        assert!(rxs[2].try_recv().is_err(), "third party gets nothing");
    }

    #[test]
    fn relay_to_drops_unknown_targets_other_rooms_and_unjoined_senders() {
        let (state, mut rxs) = state_with_room("r", &["a", "b"]);
        let me = Some(("r".to_string(), "a".to_string()));
        relay_to(&state, &me, "zzz", |from| ServerMsg::Ice { from, candidate: "c".into() });
        let elsewhere = Some(("other".to_string(), "a".to_string()));
        relay_to(&state, &elsewhere, "b", |from| ServerMsg::Ice { from, candidate: "c".into() });
        relay_to(&state, &None, "b", |from| ServerMsg::Ice { from, candidate: "c".into() });
        assert!(rxs[0].try_recv().is_err());
        assert!(rxs[1].try_recv().is_err());
    }

    #[test]
    fn relay_to_does_not_block_on_a_full_peer_queue() {
        let (state, mut rxs) = state_with_room("r", &["a", "b"]);
        let me = Some(("r".to_string(), "a".to_string()));
        for _ in 0..20 {
            relay_to(&state, &me, "b", |from| ServerMsg::Ice { from, candidate: "c".into() });
        }
        // Queue depth 8 in this fixture: the excess is dropped, nothing hangs.
        let mut n = 0;
        while rxs[1].try_recv().is_ok() {
            n += 1;
        }
        assert_eq!(n, 8);
    }

    #[test]
    fn caps_and_limits_keep_their_documented_values() {
        // ARCHITECTURE §10: warn at 12, refuse at 16. Changing these is a product
        // decision, not a tweak.
        assert_eq!(WARN_CAP, 12);
        assert_eq!(HARD_CAP, 16);
        const {
            assert!(WARN_CAP < HARD_CAP);
            assert!(MAX_SECRET >= 32, "a 16-byte hex secret must fit");
            assert!(MAX_WS_MSG > MAX_SDP, "an SDP must fit in one frame");
        }
    }

    #[test]
    fn public_urls_default_to_the_production_host() {
        // Only when the env is not set — which is the case in the test image.
        if std::env::var("PUBLIC_BASE").is_err() {
            assert_eq!(public_base(), "https://subraum.cc");
        }
        if std::env::var("PUBLIC_WS").is_err() {
            assert_eq!(public_ws(), "wss://subraum.cc/ws");
        }
        if std::env::var("DOWNLOADS_DIR").is_err() {
            assert_eq!(downloads_dir(), "/srv/downloads/subraum");
        }
    }
}
