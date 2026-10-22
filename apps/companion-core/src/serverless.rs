//! Serverless 1:1 mode — NO InitConnection server. Two peers exchange the SDP
//! offer/answer as base64 copy-paste codes (out-of-band: Discord, etc.). Uses
//! public STUN for NAT; no TURN (no creds server) → hard-NAT pairs can't relay.
//! Encryption is unchanged: DTLS-SRTP audio + DTLS-SCTP chat.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use bytes::Bytes;
use protocol::ChatMsg;
use tokio::sync::mpsc::UnboundedSender;
use webrtc::api::media_engine::MIME_TYPE_OPUS;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;
use webrtc::track::track_remote::TrackRemote;

use crate::{build_api, setup_audio, Participant, Sink, StopGuard, UiEvent};

type ChatSlot = Arc<Mutex<Option<Arc<RTCDataChannel>>>>;

fn wire_chat(slot: ChatSlot, dc: Arc<RTCDataChannel>, sink: Sink) {
    dc.on_message(Box::new(move |msg: DataChannelMessage| {
        let sink = sink.clone();
        Box::pin(async move {
            if msg.data.len() > crate::MAX_CHAT_BYTES {
                return; // drop oversized frame before allocating a ChatMsg
            }
            if let Ok(mut c) = serde_json::from_slice::<ChatMsg>(&msg.data) {
                if c.text.chars().count() > crate::MAX_CHAT_CHARS {
                    c.text = c.text.chars().take(crate::MAX_CHAT_CHARS).collect();
                }
                sink(UiEvent::Chat { from: "Peer".to_string(), text: c.text });
            }
        })
    }));
    *slot.lock().unwrap() = Some(dc);
}

async fn read_track(track: Arc<TrackRemote>, dtx: UnboundedSender<(String, Bytes)>) {
    while let Ok((p, _)) = track.read_rtp().await {
        if !p.payload.is_empty() {
            let _ = dtx.send(("peer".to_string(), p.payload));
        }
    }
}

async fn badge_of(pc: &RTCPeerConnection) -> String {
    let sctp = pc.sctp();
    let dtls = sctp.transport();
    let ice = dtls.ice_transport();
    for _ in 0..10 {
        if let Some(pair) = ice.get_selected_candidate_pair().await {
            return if format!("{pair}").contains("relay") {
                "RELAY (TURN)".to_string()
            } else {
                "DIREKT".to_string()
            };
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    "DIREKT".to_string()
}

fn decode_code(code: &str) -> Result<String> {
    // Chat apps hard-wrap long pastes; whitespace is never part of base64.
    let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = STANDARD.decode(compact).map_err(|_| anyhow!("ungültiger Code (kein base64)"))?;
    String::from_utf8(bytes).map_err(|_| anyhow!("ungültiger Code (kein UTF-8)"))
}

/// Build a single PeerConnection wired to the audio rig + chat slot + UI sink.
async fn build_pc(
    sink: Sink,
    me: String,
) -> Result<(Arc<RTCPeerConnection>, Arc<AtomicBool>, Arc<AtomicBool>, ChatSlot)> {
    let (transmit, mut opus_rx, decode_tx, _gains, _dsp, _radio, _monitor, stop, _br, _dtx, _dev_tx, _earcon) =
        setup_audio(None, None)?;
    // Until `Serverless` owns `stop`, a failing `?` below must take the rig down.
    let rig = StopGuard::new(stop.clone());
    let api = build_api()?;
    let local = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: 48000,
            channels: 2,
            ..Default::default()
        },
        "audio".to_owned(),
        "subraum".to_owned(),
    ));
    {
        let local = local.clone();
        tokio::spawn(async move {
            while let Some(b) = opus_rx.recv().await {
                let s = Sample { data: b, duration: Duration::from_millis(20), ..Default::default() };
                let _ = local.write_sample(&s).await;
            }
        });
    }

    let cfg = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };
    let pc = Arc::new(api.new_peer_connection(cfg).await?);

    let sender = pc.add_track(Arc::clone(&local) as Arc<dyn TrackLocal + Send + Sync>).await?;
    tokio::spawn(async move {
        let mut b = vec![0u8; 1500];
        while sender.read(&mut b).await.is_ok() {}
    });

    let dtx = decode_tx.clone();
    pc.on_track(Box::new(move |track: Arc<TrackRemote>, _, _| {
        let dtx = dtx.clone();
        Box::pin(async move {
            tokio::spawn(read_track(track, dtx));
        })
    }));

    let chat: ChatSlot = Arc::new(Mutex::new(None));
    {
        let chat = chat.clone();
        let sink = sink.clone();
        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let chat = chat.clone();
            let sink = sink.clone();
            Box::pin(async move {
                wire_chat(chat, dc, sink);
            })
        }));
    }

    let pc_state = pc.clone();
    let sink2 = sink.clone();
    let me2 = me.clone();
    pc.on_peer_connection_state_change(Box::new(move |s| {
        let pc_state = pc_state.clone();
        let sink2 = sink2.clone();
        let me2 = me2.clone();
        Box::pin(async move {
            match s {
                RTCPeerConnectionState::Connected => {
                    let badge = badge_of(&pc_state).await;
                    sink2(UiEvent::Status { connected: true, transmitting: false });
                    sink2(UiEvent::Roster {
                        participants: vec![
                            Participant {
                                user_id: "me".into(),
                                name: me2.clone(),
                                you: true,
                                badge: None,
                                speaking: false,
                                channel: crate::DEFAULT_CHANNEL.into(),
                                fingerprint: None,
                                secure: true,
                                linked: true,
                            },
                            Participant {
                                user_id: "peer".into(),
                                name: "Peer".into(),
                                you: false,
                                badge: Some(badge),
                                speaking: false,
                                channel: crate::DEFAULT_CHANNEL.into(),
                                // Serverless (1:1 copy-paste) PQC handshake is a
                                // follow-up; media is still DTLS-encrypted.
                                fingerprint: None,
                                secure: false,
                                // This roster is only emitted on Connected.
                                linked: true,
                            },
                        ],
                    });
                }
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Disconnected => {
                    sink2(UiEvent::Status { connected: false, transmitting: false });
                }
                _ => {}
            }
        })
    }));

    rig.disarm();
    Ok((pc, transmit, stop, chat))
}

pub struct Serverless {
    pc: Arc<RTCPeerConnection>,
    /// Stops the audio rig `build_pc` started; set on drop.
    stop: Arc<AtomicBool>,
    transmit: Arc<AtomicBool>,
    chat: ChatSlot,
    sink: Sink,
    me: String,
}

impl Drop for Serverless {
    fn drop(&mut self) {
        // Release the microphone and the PC: the Tauri layer replaces a
        // `Serverless` on every new offer/accept and nothing else closes them.
        self.stop.store(true, Ordering::SeqCst);
        let pc = self.pc.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = pc.close().await;
            });
        }
    }
}

impl Serverless {
    /// Role A: create an offer, return the base64 code to hand to the peer.
    pub async fn create_offer(sink: Sink, me: String) -> Result<(Serverless, String)> {
        let (pc, transmit, stop, chat) = build_pc(sink.clone(), me.clone()).await?;
        // Own the rig + PC before negotiating, so a failing step below drops
        // them through `Drop` instead of leaking an open microphone.
        let s = Serverless { pc, stop, transmit, chat, sink, me };
        let dc = s.pc.create_data_channel("chat", None).await?;
        wire_chat(s.chat.clone(), dc, s.sink.clone());

        let offer = s.pc.create_offer(None).await?;
        let mut gather = s.pc.gathering_complete_promise().await;
        s.pc.set_local_description(offer).await?;
        let _ = gather.recv().await; // non-trickle: full SDP with candidates
        let sdp = s.pc.local_description().await.ok_or_else(|| anyhow!("no local description"))?.sdp;
        Ok((s, STANDARD.encode(sdp)))
    }

    /// Role B: accept the peer's offer code, return our answer code.
    pub async fn accept_offer(code: String, sink: Sink, me: String) -> Result<(Serverless, String)> {
        // Validate the code first: a typo must not open the mic or allocate a PC.
        let offer = RTCSessionDescription::offer(decode_code(&code)?)?;
        let (pc, transmit, stop, chat) = build_pc(sink.clone(), me.clone()).await?;
        let s = Serverless { pc, stop, transmit, chat, sink, me }; // see create_offer
        s.pc.set_remote_description(offer).await?;
        let answer = s.pc.create_answer(None).await?;
        let mut gather = s.pc.gathering_complete_promise().await;
        s.pc.set_local_description(answer).await?;
        let _ = gather.recv().await;
        let sdp = s.pc.local_description().await.ok_or_else(|| anyhow!("no local description"))?.sdp;
        Ok((s, STANDARD.encode(sdp)))
    }

    /// Role A: finish the handshake with the peer's answer code.
    pub async fn accept_answer(&self, code: String) -> Result<()> {
        self.pc
            .set_remote_description(RTCSessionDescription::answer(decode_code(&code)?)?)
            .await?;
        Ok(())
    }

    fn set(&self, on: bool) {
        self.transmit.store(on, Ordering::SeqCst);
        (self.sink)(UiEvent::Status { connected: true, transmitting: on });
    }
    pub fn toggle_transmit(&self) {
        let n = !self.transmit.load(Ordering::SeqCst);
        self.set(n);
    }
    pub fn set_transmit(&self, on: bool) {
        if self.transmit.load(Ordering::SeqCst) != on {
            self.set(on);
        }
    }
    pub fn send_chat(&self, text: String) {
        let dc = self.chat.lock().unwrap().clone();
        let me = self.me.clone();
        let sink = self.sink.clone();
        tokio::spawn(async move {
            let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            // Echo locally only once the peer really got it: a line that
            // shows as sent but never left is worse than an error.
            let Some(dc) = dc else {
                sink(UiEvent::Log { text: "Chat: noch nicht verbunden".into() });
                return;
            };
            let Ok(j) = serde_json::to_string(&ChatMsg { text: text.clone(), ts }) else { return };
            if j.len() > crate::MAX_CHAT_BYTES {
                sink(UiEvent::Log { text: "Chat: Nachricht zu lang".into() });
                return;
            }
            if dc.send_text(j).await.is_err() {
                sink(UiEvent::Log { text: "Chat: Senden fehlgeschlagen".into() });
                return;
            }
            sink(UiEvent::Chat { from: me, text });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink() -> Sink {
        Arc::new(|_| {})
    }

    #[test]
    fn decode_code_tolerates_wrapped_and_padded_input() {
        let enc = STANDARD.encode("v=0\r\nhello");
        let wrapped = format!("  {}\n{}\t", &enc[..4], &enc[4..]);
        assert_eq!(decode_code(&wrapped).unwrap(), "v=0\r\nhello");
    }

    /// A mistyped code must fail fast, before `build_pc` opens the microphone
    /// and allocates a PeerConnection that nothing would ever close.
    #[tokio::test]
    async fn bad_offer_code_is_rejected_before_any_audio_rig_is_built() {
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            Serverless::accept_offer("###".into(), sink(), "me".into()),
        )
        .await
        .expect("accept_offer hung");
        let err = r.err().expect("bad code accepted").to_string();
        assert!(err.contains("ungültiger Code"), "unexpected error: {err}");
    }

    /// Replacing or discarding a serverless session must release the microphone
    /// and close the PeerConnection; nothing else ever will.
    #[tokio::test]
    async fn dropping_the_session_stops_the_rig_and_closes_the_pc() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let api = build_api().unwrap();
        let pc = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await.unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let s = Serverless {
            pc: pc.clone(),
            stop: stop.clone(),
            transmit: Arc::new(AtomicBool::new(false)),
            chat: Arc::new(Mutex::new(None)),
            sink: sink(),
            me: "me".into(),
        };
        drop(s);
        assert!(stop.load(Ordering::SeqCst), "audio rig not stopped on drop");
        for _ in 0..50 {
            if pc.connection_state() == RTCPeerConnectionState::Closed {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("peer connection still open 1 s after drop");
    }

    #[test]
    fn decode_code_names_the_failure() {
        assert!(decode_code("not base64 at all!!").unwrap_err().to_string().contains("kein base64"));
        // Valid base64, invalid UTF-8.
        let bad_utf8 = STANDARD.encode([0xff, 0xfe, 0xfd]);
        assert!(decode_code(&bad_utf8).unwrap_err().to_string().contains("kein UTF-8"));
        assert_eq!(decode_code("").unwrap(), "");
    }

    /// Captures every UiEvent the session emits.
    fn recording_sink() -> (Sink, Arc<Mutex<Vec<UiEvent>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        let sink: Sink = Arc::new(move |ev| l2.lock().unwrap().push(ev));
        (sink, log)
    }

    async fn bare_session(sink: Sink) -> (Serverless, Arc<RTCPeerConnection>) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let api = build_api().unwrap();
        let pc = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await.unwrap());
        let s = Serverless {
            pc: pc.clone(),
            stop: Arc::new(AtomicBool::new(false)),
            transmit: Arc::new(AtomicBool::new(false)),
            chat: Arc::new(Mutex::new(None)),
            sink,
            me: "me".into(),
        };
        (s, pc)
    }

    fn statuses(log: &Mutex<Vec<UiEvent>>) -> Vec<bool> {
        log.lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                UiEvent::Status { transmitting, .. } => Some(*transmitting),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn transmit_toggles_are_idempotent_and_reported() {
        let (sink, log) = recording_sink();
        let (s, _pc) = bare_session(sink).await;
        s.toggle_transmit();
        assert!(s.transmit.load(Ordering::SeqCst));
        s.set_transmit(true); // already on: no second event
        s.set_transmit(false);
        s.set_transmit(false); // already off: no event
        s.toggle_transmit();
        assert_eq!(statuses(&log), vec![true, false, true]);
    }

    #[tokio::test]
    async fn accept_answer_rejects_a_bad_code() {
        let (sink, _) = recording_sink();
        let (s, _pc) = bare_session(sink).await;
        let err = s.accept_answer("%%%".into()).await.unwrap_err().to_string();
        assert!(err.contains("ungültiger Code"), "{err}");
        // Valid base64 that is not SDP fails in the SDP parser, still an error.
        assert!(s.accept_answer(STANDARD.encode("hello")).await.is_err());
    }

    fn logs(log: &Mutex<Vec<UiEvent>>) -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                UiEvent::Log { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    /// A chat line is echoed locally only once it really left; otherwise the
    /// user gets a log line saying why.
    #[tokio::test]
    async fn send_chat_reports_instead_of_pretending() {
        let (sink, log) = recording_sink();
        let (s, pc) = bare_session(sink).await;
        s.send_chat("hello".into());
        settle().await;
        assert_eq!(logs(&log), vec!["Chat: noch nicht verbunden".to_string()]);

        // A channel exists but is not open: the send fails and says so.
        let dc = pc.create_data_channel("chat", None).await.unwrap();
        *s.chat.lock().unwrap() = Some(dc);
        s.send_chat("hello".into());
        settle().await;
        assert_eq!(logs(&log).last().unwrap(), "Chat: Senden fehlgeschlagen");

        // Oversized text is refused before any send attempt.
        s.send_chat("x".repeat(crate::MAX_CHAT_BYTES + 1));
        settle().await;
        assert_eq!(logs(&log).last().unwrap(), "Chat: Nachricht zu lang");
        // Nothing was ever echoed as sent.
        assert!(!log.lock().unwrap().iter().any(|e| matches!(e, UiEvent::Chat { .. })));
    }

    /// Two PCs on loopback (no STUN), A's chat channel wired into B via
    /// `wire_chat`: delivery, text clamping and the oversized-frame drop.
    #[tokio::test]
    async fn wire_chat_delivers_clamps_and_drops_oversized_frames() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let api = build_api().unwrap();
        let a = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await.unwrap());
        let b = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await.unwrap());
        let (sink, log) = recording_sink();
        let slot: ChatSlot = Arc::new(Mutex::new(None));
        {
            let (slot, sink) = (slot.clone(), sink.clone());
            b.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
                let (slot, sink) = (slot.clone(), sink.clone());
                Box::pin(async move { wire_chat(slot, dc, sink) })
            }));
        }
        let dc = a.create_data_channel("chat", None).await.unwrap();
        let opened = Arc::new(AtomicBool::new(false));
        {
            let opened = opened.clone();
            dc.on_open(Box::new(move || {
                let opened = opened.clone();
                Box::pin(async move { opened.store(true, Ordering::SeqCst) })
            }));
        }
        let mut ga = a.gathering_complete_promise().await;
        let offer = a.create_offer(None).await.unwrap();
        a.set_local_description(offer).await.unwrap();
        let _ = ga.recv().await;
        b.set_remote_description(a.local_description().await.unwrap()).await.unwrap();
        let mut gb = b.gathering_complete_promise().await;
        let answer = b.create_answer(None).await.unwrap();
        b.set_local_description(answer).await.unwrap();
        let _ = gb.recv().await;
        a.set_remote_description(b.local_description().await.unwrap()).await.unwrap();
        for _ in 0..100 {
            if opened.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(opened.load(Ordering::SeqCst), "chat channel never opened");
        assert!(slot.lock().unwrap().is_some(), "wire_chat did not stash the channel");

        let long = "ü".repeat(crate::MAX_CHAT_CHARS + 10);
        dc.send_text(serde_json::to_string(&ChatMsg { text: "hi".into(), ts: 1 }).unwrap()).await.unwrap();
        dc.send_text(serde_json::to_string(&ChatMsg { text: long, ts: 2 }).unwrap()).await.unwrap();
        dc.send_text("not json".to_string()).await.unwrap();
        dc.send_text(serde_json::to_string(&ChatMsg { text: "y".repeat(crate::MAX_CHAT_BYTES), ts: 3 }).unwrap()).await.unwrap();
        for _ in 0..50 {
            if log.lock().unwrap().len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let chats: Vec<(String, String)> = log
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                UiEvent::Chat { from, text } => Some((from.clone(), text.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(chats.len(), 2, "{chats:?}");
        assert_eq!(chats[0], ("Peer".to_string(), "hi".to_string()));
        assert_eq!(chats[1].1.chars().count(), crate::MAX_CHAT_CHARS);
        let _ = a.close().await;
        let _ = b.close().await;
    }
}
