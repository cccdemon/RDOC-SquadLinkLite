//! WebSocket signaling client.
//!
//! Encryption rule ("Nichts verlässt unverschlüsselt den Rechner"):
//!   - `ws://`  allowed ONLY for loopback (never leaves the machine).
//!   - `wss://` required for every other host; the server's self-signed cert
//!     is pinned by SHA-256 via env CERT_SHA256 (no CA needed).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures::{SinkExt, StreamExt};
use protocol::{ClientMsg, ServerMsg};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::Connector;

pub struct Signaling {
    pub out: mpsc::UnboundedSender<ClientMsg>,
    pub incoming: mpsc::UnboundedReceiver<ServerMsg>,
}

/// Extract the lowercased host from a ws(s):// URL (no deps). Handles
/// `user@host:port`, `[::1]:port`, trailing path. Returns None if malformed.
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostport = authority.rsplit('@').next()?; // drop userinfo
    let host = if let Some(after) = hostport.strip_prefix('[') {
        after.split(']').next()? // [ipv6]
    } else {
        hostport.split(':').next()? // host:port
    };
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// True only when the URL's HOST is exactly a loopback address — parsed, not
/// substring-matched (so `ws://evil.example/127.0.0.1` is NOT loopback).
fn is_loopback(url: &str) -> bool {
    matches!(url_host(url).as_deref(), Some("localhost") | Some("127.0.0.1") | Some("::1"))
}

/// Acceptable signaling endpoint policy (shared with the Tauri command layer):
/// `wss://` to any valid host, or `ws://` only to a loopback host.
pub fn server_url_ok(url: &str) -> bool {
    let u = url.trim();
    if let Some(stripped) = u.strip_prefix("wss://") {
        !stripped.is_empty() && url_host(u).is_some()
    } else if u.starts_with("ws://") {
        is_loopback(u)
    } else {
        false
    }
}

/// Verifier that trusts exactly one cert, matched by its SHA-256 fingerprint.
#[derive(Debug)]
struct PinnedCert {
    sha256: [u8; 32],
}
impl ServerCertVerifier for PinnedCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let mut h = Sha256::new();
        h.update(end_entity.as_ref());
        if h.finalize().as_slice() == self.sha256 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("certificate pin mismatch".into()))
        }
    }
    // The cert itself is pinned, so accept its handshake signatures.
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn pinned_connector(cert_sha256: &str) -> Result<Connector> {
    let bytes = hex::decode(cert_sha256.trim()).map_err(|_| anyhow!("CERT_SHA256 ist kein gültiges Hex"))?;
    if bytes.len() != 32 {
        bail!("CERT_SHA256 muss 32 Byte (64 Hex-Zeichen) sein");
    }
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&bytes);

    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedCert { sha256 }))
        .with_no_client_auth();
    Ok(Connector::Rustls(Arc::new(config)))
}

/// Budget for the whole WS handshake (TCP + TLS + upgrade). The engine runs
/// `connect` inline in its select loop, so without a bound a blackholed server
/// freezes PTT, chat and mesh events for the OS connect timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Inbound silence after which the link counts as dead (server pongs every
/// 25 s ping, so a live link never gets near this).
const RECV_IDLE: Duration = Duration::from_secs(60);

pub async fn connect(url: &str, cert_sha256: Option<&str>) -> Result<Signaling> {
    connect_with_timeout(url, cert_sha256, CONNECT_TIMEOUT).await
}

pub async fn connect_with_timeout(url: &str, cert_sha256: Option<&str>, budget: Duration) -> Result<Signaling> {
    match tokio::time::timeout(budget, connect_inner(url, cert_sha256)).await {
        Ok(r) => r,
        Err(_) => bail!("Signaling-Server antwortet nicht (Timeout nach {} s)", budget.as_secs_f32()),
    }
}

async fn connect_inner(url: &str, cert_sha256: Option<&str>) -> Result<Signaling> {
    // Same policy as the IPC layer. Without this gate `WSS://host` skips the
    // `wss://` branch and would connect without honoring the certificate pin.
    if !server_url_ok(url) {
        bail!("ungültige Server-Adresse - erlaubt sind wss://host oder ws://127.0.0.1");
    }
    let ws = if url.starts_with("wss://") {
        if let Some(pin) = cert_sha256 {
            // Self-signed server → pin its cert by SHA-256 (no CA).
            let connector = pinned_connector(pin)?;
            let (ws, _) =
                tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector))
                    .await?;
            ws
        } else {
            // No pin → standard CA validation (webpki roots). Works for real
            // certs like Let's Encrypt (e.g. subraum.cc behind Caddy).
            let (ws, _) = tokio_tungstenite::connect_async(url).await?;
            ws
        }
    } else {
        // `server_url_ok` above already guarantees this is ws:// to a loopback
        // host, so no pin is needed: the frames never leave the machine.
        let (ws, _) = tokio_tungstenite::connect_async(url).await?;
        ws
    };

    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ClientMsg>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<ServerMsg>();

    tokio::spawn(async move {
        // Heartbeat: an idle signaling WS is otherwise reaped by proxy/NAT idle
        // timeouts. Ping every 25s (the server auto-replies Pong) to keep it open
        // and to detect a dead connection promptly.
        let mut ping = tokio::time::interval(std::time::Duration::from_secs(25));
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                m = out_rx.recv() => {
                    match m {
                        Some(m) => {
                            if let Ok(s) = serde_json::to_string(&m) {
                                if sink.send(Message::Text(s)).await.is_err() {
                                    break;
                                }
                            }
                        }
                        None => {
                            // Owner dropped the handle: close the socket, or it
                            // lingers server-side until the idle reaper runs.
                            let _ = sink.close().await;
                            break;
                        }
                    }
                }
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    tokio::spawn(async move {
        // Dead-link watchdog: pings go out every 25 s, but a half-open TCP
        // connection answers nothing and `next()` would block until the OS
        // gives up. Silence past RECV_IDLE ends the connection; the engine
        // sees `incoming` close and runs its reconnect path.
        while let Ok(Some(Ok(msg))) = tokio::time::timeout(RECV_IDLE, stream.next()).await {
            if let Message::Text(t) = msg {
                if let Ok(sm) = serde_json::from_str::<ServerMsg>(&t) {
                    if in_tx.send(sm).is_err() {
                        break;
                    }
                }
            }
        }
    });

    Ok(Signaling { out: out_tx, incoming: in_rx })
}

#[cfg(test)]
mod tests {
    use super::{is_loopback, server_url_ok, url_host};

    #[test]
    fn server_policy() {
        assert!(server_url_ok("wss://subraum.cc/ws"));
        assert!(server_url_ok("ws://127.0.0.1:8080/ws"));
        assert!(server_url_ok("ws://localhost:8080/ws"));
        assert!(!server_url_ok("ws://evil.example/ws")); // plain ws, non-loopback
        assert!(!server_url_ok("ws://evil.example/127.0.0.1")); // spoof
        assert!(!server_url_ok("http://subraum.cc")); // wrong scheme
        assert!(!server_url_ok("wss://")); // empty host
    }

    #[test]
    fn host_parsing() {
        assert_eq!(url_host("ws://127.0.0.1:8080/ws").as_deref(), Some("127.0.0.1"));
        assert_eq!(url_host("wss://subraum.cc/ws").as_deref(), Some("subraum.cc"));
        assert_eq!(url_host("ws://[::1]:8080/ws").as_deref(), Some("::1"));
        assert_eq!(url_host("wss://user@Host.Example:443/x").as_deref(), Some("host.example"));
        // host is evil.example, NOT the loopback string in the path
        assert_eq!(url_host("ws://evil.example/127.0.0.1").as_deref(), Some("evil.example"));
    }

    #[test]
    fn loopback_allowed_hosts() {
        assert!(is_loopback("ws://127.0.0.1:8080/ws"));
        assert!(is_loopback("ws://localhost:8080/ws"));
        assert!(is_loopback("ws://[::1]:8080/ws"));
        assert!(is_loopback("ws://LOCALHOST/ws")); // case-insensitive
    }

    #[test]
    fn loopback_rejects_spoofed() {
        // The security finding: path/userinfo containing 127.0.0.1 must NOT pass.
        assert!(!is_loopback("ws://evil.example/127.0.0.1"));
        assert!(!is_loopback("ws://127.0.0.1.evil.example/ws"));
        assert!(!is_loopback("ws://localhost.evil.example/ws"));
        assert!(!is_loopback("ws://127.0.0.1@evil.example/ws"));
        assert!(!is_loopback("wss://subraum.cc/ws"));
    }
}

#[cfg(test)]
mod connect_tests {
    use super::*;
    use std::time::Duration;

    /// A server that accepts TCP but never finishes the WS handshake (black
    /// hole, broken proxy) must not stall the caller: the engine runs this
    /// inline in its select loop, so a hang freezes PTT, chat and the mesh.
    #[tokio::test]
    async fn connect_gives_up_when_the_server_never_answers() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = l.accept().await {
                held.push(s); // keep the socket open, say nothing
            }
        });
        let url = format!("ws://{addr}");
        let r = tokio::time::timeout(
            Duration::from_secs(3),
            connect_with_timeout(&url, None, Duration::from_millis(300)),
        )
        .await
        .expect("connect hung past its budget");
        let err = r.err().expect("connect succeeded against a silent server").to_string();
        assert!(err.contains("Timeout"), "unexpected error: {err}");
    }
}

#[cfg(test)]
mod scheme_gate_tests {
    use super::*;

    #[tokio::test]
    async fn connect_rejects_an_uppercase_scheme_instead_of_skipping_the_pin() {
        let err = match connect_inner("WSS://127.0.0.1:1/ws", Some("00")).await {
            Ok(_) => panic!("uppercase scheme must be rejected"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("wss://"), "{err}");
    }
    #[test]
    fn server_url_ok_rejects_uppercase_schemes() {
        assert!(!server_url_ok("WSS://subraum.cc/ws"));
        assert!(!server_url_ok("WS://127.0.0.1/ws"));
    }

    #[tokio::test]
    async fn connect_refuses_plain_ws_to_a_remote_host_before_dialing() {
        // No socket is opened: the policy gate fires first, whatever the host.
        let err = connect_inner("ws://evil.example:1/ws", None).await.err().unwrap().to_string();
        assert!(err.contains("ungültige Server-Adresse"), "{err}");
        let err = connect_inner("http://127.0.0.1:1/ws", None).await.err().unwrap().to_string();
        assert!(err.contains("ungültige Server-Adresse"), "{err}");
    }
}

#[cfg(test)]
mod url_and_pin_tests {
    use super::*;

    #[test]
    fn url_host_rejects_malformed_input() {
        assert_eq!(url_host("no-scheme-here"), None);
        assert_eq!(url_host("wss://"), None);
        assert_eq!(url_host("wss://:443/ws"), None);
        assert_eq!(url_host("wss://@/ws"), None);
        // Query and fragment terminate the authority like a path does.
        assert_eq!(url_host("wss://Host.Example?x=1").as_deref(), Some("host.example"));
        assert_eq!(url_host("wss://host.example#frag").as_deref(), Some("host.example"));
        assert_eq!(url_host("ws://[::1]").as_deref(), Some("::1"));
        assert_eq!(url_host("wss://a@b@host:1").as_deref(), Some("host"));
    }

    #[test]
    fn server_url_ok_trims_surrounding_whitespace() {
        assert!(server_url_ok("  wss://subraum.cc/ws\n"));
        assert!(server_url_ok("\tws://localhost:1420 "));
        assert!(!server_url_ok("   "));
    }

    #[test]
    fn pinned_connector_validates_the_fingerprint_format() {
        let err = |s: &str| pinned_connector(s).err().expect("must fail").to_string();
        assert!(err("zz").contains("Hex"));
        assert!(err(&"ab".repeat(31)).contains("32 Byte"));
        assert!(err(&"ab".repeat(33)).contains("32 Byte"));
        assert!(pinned_connector(&format!("  {}  ", "ab".repeat(32))).is_ok()); // trimmed
        assert!(matches!(pinned_connector(&"AB".repeat(32)).unwrap(), Connector::Rustls(_)));
    }

    #[test]
    fn pinned_cert_accepts_only_the_pinned_certificate() {
        let cert = CertificateDer::from(b"any DER bytes stand in for a cert".to_vec());
        let mut h = Sha256::new();
        h.update(cert.as_ref());
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&h.finalize());
        let pin = PinnedCert { sha256 };
        let name = ServerName::try_from("subraum.cc").unwrap();
        assert!(pin.verify_server_cert(&cert, &[], &name, &[], UnixTime::now()).is_ok());
        let other = CertificateDer::from(b"a different certificate".to_vec());
        let err = pin.verify_server_cert(&other, &[], &name, &[], UnixTime::now()).unwrap_err();
        assert!(err.to_string().contains("pin mismatch"), "{err}");
        // Intermediates and the server name play no part: the leaf hash is everything.
        assert!(pin.verify_server_cert(&cert, std::slice::from_ref(&other), &ServerName::try_from("evil.example").unwrap(), &[], UnixTime::now()).is_ok());
        assert!(!pin.supported_verify_schemes().is_empty());
    }
}

#[cfg(test)]
mod relay_tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    /// A fake InitConnection on loopback: hands back the accepted socket.
    async fn fake_server() -> (String, TcpListener) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", l.local_addr().unwrap());
        (url, l)
    }

    /// Outbound `ClientMsg`s reach the server as JSON text; inbound text frames
    /// come back as `ServerMsg`s; anything that is not a `ServerMsg` is dropped
    /// without killing the link.
    #[tokio::test]
    async fn frames_are_relayed_both_ways_and_garbage_is_ignored() {
        let (url, l) = fake_server().await;
        let server = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = accept_async(s).await.unwrap();
            // Garbage first, then a binary frame, then a real message.
            ws.send(Message::Text("{\"t\":\"nope\"}".into())).await.unwrap();
            ws.send(Message::Binary(vec![1, 2, 3])).await.unwrap();
            ws.send(Message::Text(serde_json::to_string(&ServerMsg::Warn { size: 12, cap: 12 }).unwrap())).await.unwrap();
            // Then read what the client sent.
            let got = loop {
                match ws.next().await {
                    Some(Ok(Message::Text(t))) => break t,
                    Some(Ok(_)) => continue,
                    other => panic!("{other:?}"),
                }
            };
            got
        });

        let sig = connect(&url, None).await.unwrap();
        let mut incoming = sig.incoming;
        match tokio::time::timeout(Duration::from_secs(3), incoming.recv()).await.unwrap() {
            Some(ServerMsg::Warn { size, cap }) => assert_eq!((size, cap), (12, 12)),
            other => panic!("{other:?}"),
        }
        sig.out.send(ClientMsg::Ptt { active: true }).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(3), server).await.unwrap().unwrap();
        assert!(matches!(serde_json::from_str::<ClientMsg>(&got).unwrap(), ClientMsg::Ptt { active: true }), "{got}");
    }

    /// A pin given for a plain loopback `ws://` URL is irrelevant and must not
    /// break the connection (the engine passes the configured pin through).
    #[tokio::test]
    async fn pin_is_ignored_for_loopback_ws() {
        let (url, l) = fake_server().await;
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = accept_async(s).await.unwrap();
            while let Some(Ok(_)) = ws.next().await {}
        });
        assert!(connect(&url, Some("deadbeef")).await.is_ok());
    }

    /// Dropping the handle closes the socket, so the server sees us leave
    /// instead of waiting for its idle reaper.
    #[tokio::test]
    async fn dropping_the_handle_closes_the_socket() {
        let (url, l) = fake_server().await;
        let server = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = accept_async(s).await.unwrap();
            loop {
                match ws.next().await {
                    Some(Ok(Message::Close(_))) | None => return true,
                    Some(Ok(_)) => continue,
                    Some(Err(_)) => return true,
                }
            }
        });
        let sig = connect(&url, None).await.unwrap();
        drop(sig);
        assert!(tokio::time::timeout(Duration::from_secs(3), server).await.expect("server never saw the close").unwrap());
    }

    /// When the server closes, `incoming` ends — the engine's cue to reconnect.
    #[tokio::test]
    async fn server_close_ends_the_incoming_stream() {
        let (url, l) = fake_server().await;
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = accept_async(s).await.unwrap();
            ws.close(None).await.unwrap();
        });
        let sig = connect(&url, None).await.unwrap();
        let mut incoming = sig.incoming;
        let end = tokio::time::timeout(Duration::from_secs(3), incoming.recv()).await.unwrap();
        assert!(end.is_none());
    }

    #[tokio::test]
    async fn connect_fails_fast_when_nothing_listens() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/ws", l.local_addr().unwrap());
        drop(l); // port is now closed
        let r = tokio::time::timeout(Duration::from_secs(5), connect(&url, None)).await.unwrap();
        assert!(r.is_err());
    }
}
