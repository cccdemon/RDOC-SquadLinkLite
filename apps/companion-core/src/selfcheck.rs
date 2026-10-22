//! Network self-check: verifies the WebRTC data path (send/receive) via a local
//! two-PeerConnection DataChannel echo, whether a public STUN reflexive
//! candidate is reachable (outbound UDP / NAT traversal viable), and whether the
//! signaling server is reachable. No external test peer needed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use bytes::Bytes;
use serde::Serialize;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

#[derive(Serialize, Clone, Copy, Debug)]
pub struct SelfCheck {
    /// Signaling server (wss) reachable.
    pub signaling: bool,
    /// Our outbound data reached the other end (can send).
    pub can_send: bool,
    /// Data reached us back (can receive).
    pub can_receive: bool,
    /// A public STUN server-reflexive candidate was gathered (internet UDP ok).
    pub stun: bool,
}

pub async fn run(server: &str, cert: Option<&str>) -> SelfCheck {
    let signaling = crate::signaling::connect(server, cert).await.is_ok();
    let (can_send, can_receive, stun) = webrtc_loopback().await.unwrap_or((false, false, false));
    SelfCheck { signaling, can_send, can_receive, stun }
}

fn config() -> RTCConfiguration {
    RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Returns (can_send, can_receive, stun_reachable).
async fn webrtc_loopback() -> Result<(bool, bool, bool)> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let api = crate::build_api()?;
    let a = Arc::new(api.new_peer_connection(config()).await?);
    let b = Arc::new(api.new_peer_connection(config()).await?);
    // Close both whatever the probe does: an early `?` must not leak sockets.
    let r = loopback_probe(a.clone(), b.clone()).await;
    let _ = a.close().await;
    let _ = b.close().await;
    r
}

async fn loopback_probe(a: Arc<RTCPeerConnection>, b: Arc<RTCPeerConnection>) -> Result<(bool, bool, bool)> {

    let b_recv = Arc::new(AtomicBool::new(false)); // B got A's data → A can send
    let a_recv = Arc::new(AtomicBool::new(false)); // A got the echo → A can receive

    // B side: echo back anything it receives.
    {
        let b_recv = b_recv.clone();
        b.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let b_recv = b_recv.clone();
            Box::pin(async move {
                let dc_echo = dc.clone();
                dc.on_message(Box::new(move |_m: DataChannelMessage| {
                    let b_recv = b_recv.clone();
                    let dc_echo = dc_echo.clone();
                    Box::pin(async move {
                        b_recv.store(true, Ordering::SeqCst);
                        let _ = dc_echo.send(&Bytes::from_static(b"pong")).await;
                    })
                }));
            })
        }));
    }

    // A side: open a channel, send "ping" on open, mark receive on the echo.
    let dc = a.create_data_channel("selfcheck", None).await?;
    {
        let a_recv = a_recv.clone();
        dc.on_message(Box::new(move |_m: DataChannelMessage| {
            let a_recv = a_recv.clone();
            Box::pin(async move { a_recv.store(true, Ordering::SeqCst) })
        }));
    }
    {
        let dc_send = dc.clone();
        dc.on_open(Box::new(move || {
            let dc_send = dc_send.clone();
            Box::pin(async move {
                let _ = dc_send.send(&Bytes::from_static(b"ping")).await;
            })
        }));
    }

    // Non-trickle exchange: wait for ICE gathering so the SDP carries candidates.
    let mut ga = a.gathering_complete_promise().await;
    let offer = a.create_offer(None).await?;
    a.set_local_description(offer).await?;
    let _ = ga.recv().await;
    let full_offer = a.local_description().await.ok_or_else(|| anyhow!("no local offer"))?;

    b.set_remote_description(full_offer.clone()).await?;
    let mut gb = b.gathering_complete_promise().await;
    let answer = b.create_answer(None).await?;
    b.set_local_description(answer).await?;
    let _ = gb.recv().await;
    let full_answer = b.local_description().await.ok_or_else(|| anyhow!("no local answer"))?;
    a.set_remote_description(full_answer.clone()).await?;

    let stun = sdp_has_srflx(&full_offer) || sdp_has_srflx(&full_answer);

    // Give the data path up to ~8s to round-trip.
    for _ in 0..40 {
        if a_recv.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let send = b_recv.load(Ordering::SeqCst);
    let recv = a_recv.load(Ordering::SeqCst);
    Ok((send, recv, stun))
}

fn sdp_has_srflx(d: &RTCSessionDescription) -> bool {
    d.sdp.contains("typ srflx")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(sdp: &str) -> RTCSessionDescription {
        let mut d = RTCSessionDescription::default();
        d.sdp = sdp.to_string();
        d
    }

    #[test]
    fn srflx_is_detected_only_from_a_server_reflexive_candidate_line() {
        assert!(sdp_has_srflx(&desc("v=0\r\na=candidate:2 1 udp 1694498815 203.0.113.9 50001 typ srflx raddr 10.0.0.1 rport 1\r\n")));
        assert!(!sdp_has_srflx(&desc("v=0\r\na=candidate:1 1 udp 2130706431 10.0.0.1 1 typ host\r\n")));
        assert!(!sdp_has_srflx(&desc("v=0\r\na=candidate:3 1 udp 1 1.2.3.4 1 typ relay\r\n")));
        assert!(!sdp_has_srflx(&desc("")));
    }

    #[test]
    fn result_serializes_as_four_flat_booleans() {
        let r = SelfCheck { signaling: true, can_send: false, can_receive: true, stun: false };
        let v: serde_json::Value = serde_json::to_value(r).unwrap();
        assert_eq!(v, serde_json::json!({"signaling": true, "can_send": false, "can_receive": true, "stun": false}));
    }

    #[test]
    fn probe_config_asks_a_public_stun_server() {
        let c = config();
        assert_eq!(c.ice_servers.len(), 1);
        assert!(c.ice_servers[0].urls[0].starts_with("stun:"));
        assert!(c.ice_servers[0].username.is_empty(), "STUN needs no credentials");
    }

    /// The echo probe over two loopback PCs — host candidates only, no STUN,
    /// no internet. Send and receive must both prove out; without a STUN
    /// server there is no reflexive candidate, so `stun` is false.
    #[tokio::test]
    async fn loopback_probe_round_trips_without_any_network() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let api = crate::build_api().unwrap();
        let a = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await.unwrap());
        let b = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await.unwrap());
        let r = tokio::time::timeout(Duration::from_secs(20), loopback_probe(a.clone(), b.clone()))
            .await
            .expect("probe hung")
            .unwrap();
        let _ = a.close().await;
        let _ = b.close().await;
        assert_eq!(r, (true, true, false), "(can_send, can_receive, stun)");
    }

    /// Both PCs are closed again even when the probe bails out early, so a
    /// failed self-check leaks no sockets.
    #[tokio::test]
    async fn probe_failure_still_closes_both_peer_connections() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let api = crate::build_api().unwrap();
        let a = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await.unwrap());
        let b = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await.unwrap());
        let _ = a.close().await; // a closed PC cannot create a data channel → early `?`
        let r = loopback_probe(a.clone(), b.clone()).await;
        assert!(r.is_err());
        let _ = b.close().await;
        assert_eq!(a.connection_state(), webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed);
        assert_eq!(b.connection_state(), webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed);
    }
}
