//! Local control API for external controllers (Stream Deck, Bitfocus
//! Companion, scripts): a WebSocket server on 127.0.0.1 with a per-start
//! token.
//!
//! Discovery: on start we write `control.json` — `{"port":N,"token":"hex"}` —
//! into the app config dir. A controller running as the same user reads it,
//! connects to `ws://127.0.0.1:<port>`, and MUST send
//! `{"t":"auth","token":"…"}` as its first message; everything before a valid
//! auth is ignored and three bad attempts drop the connection. The bind is
//! loopback-only, so nothing off-machine can ever reach it; the token gates
//! other local users on a shared box and makes drive-by localhost probing from
//! a browser useless (browsers can open ws://127.0.0.1 but don't know the
//! token, and it rotates every app start).
//!
//! The server deliberately owns NO feature logic. Commands are forwarded to
//! the webview (which owns mute/deafen/channel/volume state and already gates
//! PTT on self-mute), and the webview reports its state back via the
//! `control_state` Tauri command, which we cache and broadcast to every
//! authenticated client. One source of truth, no drift.

use std::sync::Mutex;

use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio_tungstenite::tungstenite::Message;

/// Commands leaving the control server toward the app. Kept as an enum (not
/// raw JSON passthrough) so the surface is enumerable and testable.
#[derive(Debug, Clone, PartialEq)]
pub enum CtlOut {
    /// Push-to-talk down/up — same event the Raw-Input hook emits, so the
    /// webview's self-mute gating applies unchanged.
    Ptt(bool),
    /// Cycle the channel list (−1 prev / +1 next) — same event as the hotkeys.
    ChanCycle(i32),
    /// Everything else, forwarded to the webview's "ctl" listener verbatim:
    /// mic-toggle, deafen-toggle, channel {name}, rekey, disconnect,
    /// volume {value}, volume-delta {d}.
    Ctl(serde_json::Value),
    /// A controller just authenticated: ask the webview to re-report its state.
    /// Closes the startup race — the webview's mount-time report can land
    /// before this server is up, leaving the cache on its default until the
    /// next organic state change.
    Refresh,
}

/// Max inbound frame we bother parsing. Commands are tiny; anything bigger is
/// not a controller.
const MAX_MSG: usize = 4096;
/// Failed auth attempts before the connection is dropped.
const MAX_AUTH_TRIES: u32 = 3;

/// Commands a client may send after auth: name → forwarded shape.
fn route(v: &serde_json::Value) -> Option<CtlOut> {
    match v.get("t").and_then(|t| t.as_str())? {
        "ptt" => Some(CtlOut::Ptt(v.get("on")?.as_bool()?)),
        "chan-cycle" => {
            let d = v.get("dir")?.as_i64()?;
            if d == -1 || d == 1 {
                Some(CtlOut::ChanCycle(d as i32))
            } else {
                None
            }
        }
        // Webview-owned state: forward as-is, the webview validates (it clamps
        // channel names and volumes exactly like its own UI handlers).
        "mic-toggle" | "deafen-toggle" | "channel" | "rekey" | "disconnect" | "volume"
        | "volume-delta" => Some(CtlOut::Ctl(v.clone())),
        _ => None,
    }
}

pub struct ControlServer {
    pub port: u16,
    pub token: String,
    /// Last state snapshot the webview reported; sent to every client on auth.
    state: Mutex<serde_json::Value>,
    clients: Mutex<Vec<UnboundedSender<Message>>>,
}

impl ControlServer {
    /// Bind on an ephemeral loopback port and start accepting. `out` receives
    /// every authenticated client command; the caller forwards it to the app.
    pub async fn start(out: UnboundedSender<CtlOut>) -> std::io::Result<std::sync::Arc<Self>> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let mut raw = [0u8; 32];
        getrandom::getrandom(&mut raw).expect("OS RNG unavailable");
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();

        let srv = std::sync::Arc::new(ControlServer {
            port,
            token,
            state: Mutex::new(serde_json::json!({ "connected": false })),
            clients: Mutex::new(Vec::new()),
        });

        let accept_srv = srv.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                let srv = accept_srv.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let _ = srv.serve_conn(stream, out).await;
                });
            }
        });
        Ok(srv)
    }

    /// The webview reported fresh state: cache it and fan it out.
    pub fn publish_state(&self, state: serde_json::Value) {
        let msg = serde_json::json!({ "t": "state", "state": state });
        *self.state.lock().unwrap() = state;
        let text = msg.to_string();
        // Drop clients whose channel is gone (task ended).
        self.clients.lock().unwrap().retain(|c| c.send(Message::text(text.clone())).is_ok());
    }

    async fn serve_conn(
        &self,
        stream: tokio::net::TcpStream,
        out: UnboundedSender<CtlOut>,
    ) -> anyhow::Result<()> {
        let ws = tokio_tungstenite::accept_async(stream).await?;
        let (mut tx, mut rx) = ws.split();

        // ── Auth gate ────────────────────────────────────────────────────────
        let mut tries = 0u32;
        loop {
            let Some(Ok(msg)) = rx.next().await else { return Ok(()) };
            let Ok(text) = msg.into_text() else { continue };
            if text.len() > MAX_MSG {
                return Ok(());
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
            if v.get("t").and_then(|t| t.as_str()) == Some("auth") {
                let presented = v.get("token").and_then(|t| t.as_str()).unwrap_or("");
                if constant_time_eq(presented.as_bytes(), self.token.as_bytes()) {
                    break;
                }
                tries += 1;
                let _ = tx
                    .send(Message::text(r#"{"t":"auth-failed"}"#.to_string()))
                    .await;
                if tries >= MAX_AUTH_TRIES {
                    return Ok(());
                }
            }
        }

        // Hello + current state snapshot, then register for broadcasts.
        let hello = serde_json::json!({
            "t": "hello",
            "app": "subraum",
            "state": *self.state.lock().unwrap(),
        });
        tx.send(Message::text(hello.to_string())).await?;
        let (ctx, mut crx) = unbounded_channel::<Message>();
        self.clients.lock().unwrap().push(ctx);
        let _ = out.send(CtlOut::Refresh); // pull a fresh snapshot for this client

        // ── Authenticated session ────────────────────────────────────────────
        loop {
            tokio::select! {
                broadcast = crx.recv() => {
                    let Some(m) = broadcast else { break };
                    if tx.send(m).await.is_err() { break; }
                }
                inbound = rx.next() => {
                    let Some(Ok(msg)) = inbound else { break };
                    if msg.is_ping() || msg.is_pong() { continue; }
                    if msg.is_close() { break; }
                    let Ok(text) = msg.into_text() else { continue };
                    if text.len() > MAX_MSG { break; }
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                    if let Some(cmd) = route(&v) {
                        let _ = out.send(cmd);
                    }
                }
            }
        }
        Ok(())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Write the discovery file `control.json` into `dir`. Rewritten on every
/// start, so a controller always finds the live port + token.
pub fn write_discovery(dir: &std::path::Path, port: u16, token: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let body = serde_json::json!({ "port": port, "token": token });
    std::fs::write(dir.join("control.json"), body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel as chan;

    async fn client(port: u16) -> tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    > {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
            .await
            .expect("connect");
        ws
    }

    async fn recv_json(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> serde_json::Value {
        loop {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
                .await
                .expect("timeout")
                .expect("stream ended")
                .expect("ws error");
            if msg.is_text() {
                return serde_json::from_str(&msg.into_text().unwrap()).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn commands_flow_only_after_auth() {
        let (out_tx, mut out_rx) = chan();
        let srv = ControlServer::start(out_tx).await.unwrap();

        let mut ws = client(srv.port).await;
        // Pre-auth commands must be ignored, not forwarded.
        ws.send(Message::text(r#"{"t":"ptt","on":true}"#.to_string())).await.unwrap();
        ws.send(Message::text(format!(r#"{{"t":"auth","token":"{}"}}"#, srv.token)))
            .await
            .unwrap();
        let hello = recv_json(&mut ws).await;
        assert_eq!(hello["t"], "hello");
        assert_eq!(hello["app"], "subraum");
        // Auth triggers a state-refresh request toward the app.
        assert_eq!(out_rx.recv().await, Some(CtlOut::Refresh));

        ws.send(Message::text(r#"{"t":"ptt","on":true}"#.to_string())).await.unwrap();
        assert_eq!(out_rx.recv().await, Some(CtlOut::Ptt(true)));
        ws.send(Message::text(r#"{"t":"chan-cycle","dir":-1}"#.to_string())).await.unwrap();
        assert_eq!(out_rx.recv().await, Some(CtlOut::ChanCycle(-1)));
        ws.send(Message::text(r#"{"t":"mic-toggle"}"#.to_string())).await.unwrap();
        match out_rx.recv().await {
            Some(CtlOut::Ctl(v)) => assert_eq!(v["t"], "mic-toggle"),
            other => panic!("expected Ctl, got {other:?}"),
        }
        // The pre-auth ptt must NOT have arrived: channel is now empty.
        assert!(out_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn wrong_token_is_rejected_and_three_strikes_disconnect() {
        let (out_tx, mut out_rx) = chan();
        let srv = ControlServer::start(out_tx).await.unwrap();

        let mut ws = client(srv.port).await;
        for _ in 0..MAX_AUTH_TRIES {
            ws.send(Message::text(r#"{"t":"auth","token":"wrong"}"#.to_string()))
                .await
                .unwrap();
            let reply = recv_json(&mut ws).await;
            assert_eq!(reply["t"], "auth-failed");
        }
        // Connection is gone: the next read ends the stream.
        let end = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws.next().await {
                    None => break,
                    Some(Ok(m)) if m.is_close() => break,
                    Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await;
        assert!(end.is_ok(), "server must drop after {MAX_AUTH_TRIES} bad tokens");
        assert!(out_rx.try_recv().is_err(), "nothing may have been forwarded");
    }

    #[tokio::test]
    async fn state_snapshot_on_auth_and_broadcast_on_change() {
        let (out_tx, _keep) = chan();
        let srv = ControlServer::start(out_tx).await.unwrap();
        srv.publish_state(serde_json::json!({ "connected": true, "channel": "Funk 1" }));

        let mut ws = client(srv.port).await;
        ws.send(Message::text(format!(r#"{{"t":"auth","token":"{}"}}"#, srv.token)))
            .await
            .unwrap();
        let hello = recv_json(&mut ws).await;
        assert_eq!(hello["state"]["channel"], "Funk 1", "snapshot rides the hello");

        srv.publish_state(serde_json::json!({ "connected": true, "channel": "Funk 2" }));
        let update = recv_json(&mut ws).await;
        assert_eq!(update["t"], "state");
        assert_eq!(update["state"]["channel"], "Funk 2");
    }

    #[tokio::test]
    async fn unknown_and_malformed_commands_are_dropped() {
        let (out_tx, mut out_rx) = chan();
        let srv = ControlServer::start(out_tx).await.unwrap();
        let mut ws = client(srv.port).await;
        ws.send(Message::text(format!(r#"{{"t":"auth","token":"{}"}}"#, srv.token)))
            .await
            .unwrap();
        let _ = recv_json(&mut ws).await; // hello
        assert_eq!(out_rx.recv().await, Some(CtlOut::Refresh));

        for bad in [
            r#"{"t":"shutdown-the-machine"}"#,
            r#"{"t":"chan-cycle","dir":5}"#,
            r#"{"t":"ptt"}"#,
            r#"not json"#,
        ] {
            ws.send(Message::text(bad.to_string())).await.unwrap();
        }
        // A valid command after the garbage still works (connection survived).
        ws.send(Message::text(r#"{"t":"ptt","on":false}"#.to_string())).await.unwrap();
        assert_eq!(out_rx.recv().await, Some(CtlOut::Ptt(false)));
        assert!(out_rx.try_recv().is_err());
    }

    #[test]
    fn discovery_file_shape() {
        let dir = std::env::temp_dir().join(format!("subraum-ctl-test-{}", std::process::id()));
        write_discovery(&dir, 43210, "cafe").unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("control.json")).unwrap())
                .unwrap();
        assert_eq!(v["port"], 43210);
        assert_eq!(v["token"], "cafe");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn discovery_file_creates_nested_dirs_and_is_rewritten_on_every_start() {
        let root = std::env::temp_dir().join(format!("subraum-ctl-nested-{}", std::process::id()));
        let dir = root.join("a").join("b");
        write_discovery(&dir, 1, "first").unwrap();
        write_discovery(&dir, 2, "second").unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("control.json")).unwrap()).unwrap();
        assert_eq!(v["port"], 2, "a controller must find the LIVE port, not a stale one");
        assert_eq!(v["token"], "second");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn route_maps_each_command_shape_and_rejects_the_rest() {
        let j = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(route(&j(r#"{"t":"ptt","on":true}"#)), Some(CtlOut::Ptt(true)));
        assert_eq!(route(&j(r#"{"t":"ptt","on":false}"#)), Some(CtlOut::Ptt(false)));
        assert_eq!(route(&j(r#"{"t":"ptt"}"#)), None, "ptt without `on` is not a command");
        assert_eq!(route(&j(r#"{"t":"ptt","on":"yes"}"#)), None, "`on` must be a bool");
        assert_eq!(route(&j(r#"{"t":"chan-cycle","dir":1}"#)), Some(CtlOut::ChanCycle(1)));
        assert_eq!(route(&j(r#"{"t":"chan-cycle","dir":-1}"#)), Some(CtlOut::ChanCycle(-1)));
        for bad in [r#"{"t":"chan-cycle","dir":0}"#, r#"{"t":"chan-cycle","dir":2}"#, r#"{"t":"chan-cycle"}"#, r#"{"t":"chan-cycle","dir":"1"}"#] {
            assert_eq!(route(&j(bad)), None, "{bad}");
        }
        // Webview-owned commands are forwarded verbatim, payload included.
        for t in ["mic-toggle", "deafen-toggle", "channel", "rekey", "disconnect", "volume", "volume-delta"] {
            let v = j(&format!(r#"{{"t":"{t}","name":"Funk 2","value":0.5}}"#));
            assert_eq!(route(&v), Some(CtlOut::Ctl(v.clone())), "{t}");
        }
        // Nothing else gets through: unknown names, missing / non-string tags, non-objects.
        for bad in [r#"{"t":"auth","token":"x"}"#, r#"{"t":"state"}"#, r#"{"on":true}"#, r#"{"t":7}"#, r#"[1,2]"#, r#""ptt""#, "null"] {
            assert_eq!(route(&j(bad)), None, "{bad}");
        }
    }

    #[test]
    fn constant_time_eq_compares_bytes_and_lengths() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(!constant_time_eq(b"Abc", b"abc"), "case matters");
    }

    #[tokio::test]
    async fn token_is_64_hex_chars_and_rotates_per_start() {
        let (tx1, _k1) = chan();
        let (tx2, _k2) = chan();
        let a = ControlServer::start(tx1).await.unwrap();
        let b = ControlServer::start(tx2).await.unwrap();
        for t in [&a.token, &b.token] {
            assert_eq!(t.len(), 64);
            assert!(t.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        }
        assert_ne!(a.token, b.token, "two starts must never share a token");
        assert_ne!(a.port, b.port, "each start binds its own ephemeral port");
        assert_ne!(a.port, 0);
    }

    #[tokio::test]
    async fn non_auth_frames_before_auth_are_ignored_and_do_not_count_as_strikes() {
        let (out_tx, mut out_rx) = chan();
        let srv = ControlServer::start(out_tx).await.unwrap();
        let mut ws = client(srv.port).await;
        // Plenty of noise: commands, garbage, non-text — none is a strike.
        for noise in [r#"{"t":"ptt","on":true}"#, "not json", r#"{"hello":1}"#, r#"[]"#, r#"{"t":"disconnect"}"#] {
            ws.send(Message::text(noise.to_string())).await.unwrap();
        }
        ws.send(Message::binary(vec![1, 2, 3])).await.unwrap();
        ws.send(Message::Ping(vec![])).await.unwrap();
        // Two real strikes, then the right token: still lets us in (3 would not).
        for _ in 0..(MAX_AUTH_TRIES - 1) {
            ws.send(Message::text(r#"{"t":"auth","token":"nope"}"#.to_string())).await.unwrap();
            assert_eq!(recv_json(&mut ws).await["t"], "auth-failed");
        }
        ws.send(Message::text(format!(r#"{{"t":"auth","token":"{}"}}"#, srv.token))).await.unwrap();
        let hello = recv_json(&mut ws).await;
        assert_eq!(hello["t"], "hello");
        assert_eq!(hello["state"]["connected"], false, "default snapshot before the webview reported anything");
        assert_eq!(out_rx.recv().await, Some(CtlOut::Refresh));
        assert!(out_rx.try_recv().is_err(), "none of the pre-auth commands leaked through");
    }

    #[tokio::test]
    async fn auth_without_a_token_field_is_a_strike() {
        let (out_tx, _k) = chan();
        let srv = ControlServer::start(out_tx).await.unwrap();
        let mut ws = client(srv.port).await;
        ws.send(Message::text(r#"{"t":"auth"}"#.to_string())).await.unwrap();
        assert_eq!(recv_json(&mut ws).await["t"], "auth-failed");
        ws.send(Message::text(r#"{"t":"auth","token":null}"#.to_string())).await.unwrap();
        assert_eq!(recv_json(&mut ws).await["t"], "auth-failed");
        // A token with the right length but one wrong char is still wrong.
        let mut near = srv.token.clone();
        near.replace_range(0..1, if srv.token.starts_with('0') { "1" } else { "0" });
        ws.send(Message::text(format!(r#"{{"t":"auth","token":"{near}"}}"#))).await.unwrap();
        assert_eq!(recv_json(&mut ws).await["t"], "auth-failed");
    }

    async fn stream_ends(ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>) -> bool {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match ws.next().await {
                    None | Some(Err(_)) => break,
                    Some(Ok(m)) if m.is_close() => break,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await
        .is_ok()
    }

    #[tokio::test]
    async fn oversized_frame_before_auth_drops_the_connection() {
        let (out_tx, mut out_rx) = chan();
        let srv = ControlServer::start(out_tx).await.unwrap();
        let mut ws = client(srv.port).await;
        let big = format!(r#"{{"t":"auth","token":"{}"}}"#, "a".repeat(MAX_MSG));
        ws.send(Message::text(big)).await.unwrap();
        assert!(stream_ends(&mut ws).await, "a frame over MAX_MSG is not a controller");
        assert!(out_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn oversized_frame_after_auth_closes_and_the_client_is_pruned_on_next_broadcast() {
        let (out_tx, mut out_rx) = chan();
        let srv = ControlServer::start(out_tx).await.unwrap();
        let mut ws = client(srv.port).await;
        ws.send(Message::text(format!(r#"{{"t":"auth","token":"{}"}}"#, srv.token))).await.unwrap();
        let _ = recv_json(&mut ws).await;
        assert_eq!(out_rx.recv().await, Some(CtlOut::Refresh));
        assert_eq!(srv.clients.lock().unwrap().len(), 1);

        ws.send(Message::text(format!(r#"{{"t":"channel","name":"{}"}}"#, "x".repeat(MAX_MSG)))).await.unwrap();
        assert!(stream_ends(&mut ws).await, "server closes on an oversized post-auth frame");
        assert!(out_rx.try_recv().is_err(), "the oversized command is not forwarded");

        // The dead client's sender is dropped once its task ends; the next
        // publish notices and prunes it. Give the task a moment to unwind.
        for _ in 0..50 {
            srv.publish_state(serde_json::json!({ "connected": true }));
            if srv.clients.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(srv.clients.lock().unwrap().is_empty(), "dead clients must not accumulate");
    }

    #[tokio::test]
    async fn ping_frames_after_auth_keep_the_session_alive() {
        let (out_tx, mut out_rx) = chan();
        let srv = ControlServer::start(out_tx).await.unwrap();
        let mut ws = client(srv.port).await;
        ws.send(Message::text(format!(r#"{{"t":"auth","token":"{}"}}"#, srv.token))).await.unwrap();
        let _ = recv_json(&mut ws).await;
        assert_eq!(out_rx.recv().await, Some(CtlOut::Refresh));
        ws.send(Message::Ping(b"hb".to_vec())).await.unwrap();
        ws.send(Message::binary(vec![0u8; 16])).await.unwrap(); // not text: ignored, not fatal
        ws.send(Message::text(r#"{"t":"rekey"}"#.to_string())).await.unwrap();
        match out_rx.recv().await {
            Some(CtlOut::Ctl(v)) => assert_eq!(v["t"], "rekey"),
            other => panic!("expected the rekey command after a ping, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn every_authenticated_client_receives_each_state_broadcast() {
        let (out_tx, _k) = chan();
        let srv = ControlServer::start(out_tx).await.unwrap();
        let mut a = client(srv.port).await;
        let mut b = client(srv.port).await;
        for ws in [&mut a, &mut b] {
            ws.send(Message::text(format!(r#"{{"t":"auth","token":"{}"}}"#, srv.token))).await.unwrap();
            assert_eq!(recv_json(ws).await["t"], "hello");
        }
        assert_eq!(srv.clients.lock().unwrap().len(), 2);
        srv.publish_state(serde_json::json!({ "connected": true, "muted": true }));
        for ws in [&mut a, &mut b] {
            let m = recv_json(ws).await;
            assert_eq!(m["t"], "state");
            assert_eq!(m["state"]["muted"], true);
        }
        // Closing one client does not disturb the other.
        a.close(None).await.unwrap();
        srv.publish_state(serde_json::json!({ "connected": false }));
        let m = recv_json(&mut b).await;
        assert_eq!(m["state"]["connected"], false);
    }
}
