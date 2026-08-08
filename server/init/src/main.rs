//! InitConnection — WebSocket signaling for the subraum mesh.
//!
//! Dumb relay: routes offer/answer/ice by `to`, keeps the per-room roster,
//! enforces room-auth + cap, mints ephemeral TURN creds. No media here.
//!
//! Env:
//!   PORT             listen port (default 8080)
//!   ROOM_AUTH_SECRET HMAC secret for room tokens (unset = open dev mode)
//!   TURN_SECRET      coturn shared secret (optional)
//!   TURN_URLS        comma-separated turn: urls (optional)
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
use protocol::{ClientMsg, PeerInfo, ServerMsg};
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

fn len_ok(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max
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

/// Best-effort client IP: first hop of X-Forwarded-For (set by our reverse
/// proxy), else a constant bucket. Good enough for coarse abuse throttling.
/// Real client IP for rate-limiting. X-Forwarded-For is only trusted when the
/// direct peer is our loopback reverse proxy; otherwise the socket peer is used
/// (so a directly-reachable client can't spoof XFF to dodge/forge limits).
fn client_ip(headers: &HeaderMap, peer: SocketAddr) -> String {
    if peer.ip().is_loopback() {
        if let Some(first) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            return first.to_string();
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

struct PeerHandle {
    name: String,
    tx: mpsc::Sender<ServerMsg>,
}

type Room = HashMap<String, PeerHandle>; // user_id -> handle

struct AppState {
    rooms: Mutex<HashMap<String, Room>>,
    auth: AuthConfig,
    turn: Option<TurnConfig>,
    sessions: Sessions,
    rate: RateLimiter,
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

    let app = Router::new()
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
        .with_state(state);

    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);

    // TLS by default (wss). TLS_DISABLE=1 → plain ws (must sit behind a TLS proxy);
    // bind loopback only unless ALLOW_PLAIN_PUBLIC_BIND=1, so a misconfig can't
    // expose plain ws to the network.
    if std::env::var("TLS_DISABLE").is_ok() {
        let bind_ip = if std::env::var("ALLOW_PLAIN_PUBLIC_BIND").is_ok() {
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
        loop {
            let Some(a) = out.find('`') else { break };
            let Some(rel) = out[a + 1..].find('`') else { break };
            let b = a + 1 + rel;
            let code = out[a + 1..b].to_string();
            out.replace_range(a..=b, &format!("<code>{code}</code>"));
        }
        loop {
            let Some(a) = out.find("**") else { break };
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
    let path = std::path::Path::new(&dir).join("manifest.json");
    let Ok(txt) = std::fs::read_to_string(&path) else {
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
    // Sliding-window message rate limit + rekey cooldown, per connection.
    let mut win_start = Instant::now();
    let mut win_count: u32 = 0;
    let mut last_rekey: Option<Instant> = None;

    while let Some(Ok(msg)) = stream.next().await {
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
                let _ = tx.try_send(ServerMsg::Error { code: "bad_json".into(), message: e.to_string() });
                continue;
            }
        };

        match cmsg {
            ClientMsg::Join { room, user_id, name, token } => {
                if me.is_some() {
                    let _ = tx.try_send(ServerMsg::Error {
                        code: "already_joined".into(),
                        message: "this socket already joined a room".into(),
                    });
                    continue;
                }
                // Bound all client-supplied fields before doing anything with them.
                if !len_ok(&room, MAX_ID)
                    || !len_ok(&user_id, MAX_ID)
                    || !len_ok(&name, MAX_NAME)
                    || token.as_deref().map(|t| t.len() > MAX_TOKEN).unwrap_or(false)
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

                let (roster, size) = {
                    let mut rooms = state.rooms.lock().unwrap();
                    let r = rooms.entry(room.clone()).or_default();
                    // Cap check (a rejoining same user_id doesn't count as growth).
                    if r.len() >= HARD_CAP && !r.contains_key(&user_id) {
                        let _ = tx.try_send(ServerMsg::RoomFull { cap: HARD_CAP });
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
                    r.insert(user_id.clone(), PeerHandle { name: name.clone(), tx: tx.clone() });
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
                    // Soft-cap warning to everyone in the room.
                    if size >= WARN_CAP {
                        for h in r.values() {
                            let _ = h.tx.try_send(ServerMsg::Warn { size, cap: WARN_CAP });
                        }
                    }
                    (roster, size)
                };

                let _ = tx.try_send(ServerMsg::Roster { peers: roster });
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
                if let Some(t) = last_rekey {
                    if now.duration_since(t) < REKEY_COOLDOWN {
                        continue;
                    }
                }
                last_rekey = Some(now);
                // Broadcast a key-rotation request to the whole room (incl. the
                // initiator) so every client re-handshakes together.
                if let Some((room, from)) = &me {
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

    // Cleanup: drop from room, notify peers.
    if let Some((room, user_id)) = me {
        let mut rooms = state.rooms.lock().unwrap();
        if let Some(r) = rooms.get_mut(&room) {
            r.remove(&user_id);
            for h in r.values() {
                let _ = h.tx.try_send(ServerMsg::PeerLeft { user_id: user_id.clone() });
            }
            if r.is_empty() {
                rooms.remove(&room);
            }
        }
        tracing::info!(%room, %user_id, "leave");
    }
    writer.abort();
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
