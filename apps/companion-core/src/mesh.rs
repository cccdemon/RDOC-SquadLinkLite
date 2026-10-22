//! Mesh: one PeerConnection per remote peer. A single shared local audio track
//! (StaticSample) is added to every PC — encode-once, the lib does per-peer
//! SRTP (proven in spikes/track-fanout). Each pair also gets a DataChannel for
//! encrypted text chat. Glare: the lexicographically smaller user_id offers.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use bytes::Bytes;
use protocol::{ChatMsg, ClientMsg, CtrlMsg};
use tokio::sync::mpsc::UnboundedSender;

use crate::crypto::{Hello, Kind, PeerCrypto, Reply, RoomAudio};
use crate::{ChanState, MeshEvent};
use webrtc::api::API;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::signaling_state::RTCSignalingState;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;
use webrtc::track::track_remote::TrackRemote;

pub type DecodeTx = UnboundedSender<(String, Bytes)>;
type ChatSlot = Arc<Mutex<Option<Arc<RTCDataChannel>>>>;

/// Max ICE candidates buffered per peer before the remote description is set.
const MAX_PENDING_ICE: usize = 128;

/// What a link revealed while it was being set up. Read when it dies, so the
/// user's log can say WHICH failure it was: nothing arrived (signaling / a lost
/// offer) versus candidates on both sides but no pair (the path itself is
/// blocked — the case only a TURN relay could fix). Without this the two are
/// indistinguishable from the outside and every dead link looks like "NAT".
#[derive(Default)]
struct LinkDiag {
    /// ICE candidate types we gathered ("host", "srflx", "relay").
    local: Mutex<BTreeSet<String>>,
    /// ICE candidate types the peer sent us.
    remote: Mutex<BTreeSet<String>>,
    /// Did the peer's offer/answer ever arrive?
    remote_desc: AtomicBool,
    /// Did a candidate pair ever get selected (i.e. was the link ever up)?
    paired: AtomicBool,
}

/// Pull the type out of an SDP candidate line ("… typ srflx …"). The line may
/// arrive as raw SDP or wrapped in an RTCIceCandidateInit JSON; both contain it.
fn candidate_type(line: &str) -> Option<String> {
    let rest = line.split(" typ ").nth(1)?;
    // The type is the alphanumeric run right after "typ ". In a JSON-wrapped
    // candidate it may be glued to the closing quote and the next field
    // (`host","sdpMid":"0"…`), so stop at the first non-alphanumeric char
    // instead of trimming only the ends.
    let t: String = rest.trim_start().chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    if t.is_empty() { None } else { Some(t) }
}

impl LinkDiag {
    fn note_local(&self, line: &str) {
        if let Some(t) = candidate_type(line) {
            self.local.lock().unwrap().insert(t);
        }
    }
    fn note_remote(&self, line: &str) {
        if let Some(t) = candidate_type(line) {
            self.remote.lock().unwrap().insert(t);
        }
    }
    fn types(set: &Mutex<BTreeSet<String>>) -> String {
        let s = set.lock().unwrap();
        if s.is_empty() {
            "keine".to_string()
        } else {
            s.iter().cloned().collect::<Vec<_>>().join(",")
        }
    }
    /// One ASCII line naming the failure mode, for the app's log.
    fn summary(&self) -> String {
        let local = Self::types(&self.local);
        let remote = Self::types(&self.remote);
        if self.paired.load(Ordering::SeqCst) {
            return format!("Verbindung stand und ist abgebrochen (lokal {local}, gegenueber {remote})");
        }
        if !self.remote_desc.load(Ordering::SeqCst) {
            return "kein Offer/Answer angekommen - Signalisierung, nicht NAT".to_string();
        }
        if self.remote.lock().unwrap().is_empty() {
            return format!("keine ICE-Kandidaten vom Gegenueber (lokal {local})");
        }
        format!("Kandidaten getauscht (lokal {local}, gegenueber {remote}), aber kein Paar durchgekommen - der Weg ist blockiert")
    }
}

struct PeerConn {
    pc: Arc<RTCPeerConnection>,
    pending_ice: Mutex<Vec<String>>,
    remote_set: AtomicBool,
    chat: ChatSlot,
    diag: Arc<LinkDiag>,
}
impl PeerConn {
    async fn add_or_queue_ice(&self, cand: String) {
        self.diag.note_remote(&cand);
        if self.remote_set.load(Ordering::SeqCst) {
            if let Ok(init) = serde_json::from_str::<RTCIceCandidateInit>(&cand) {
                let _ = self.pc.add_ice_candidate(init).await;
            }
        } else {
            // Cap the pre-remote-description queue: a flood of ICE frames from a
            // room member must not grow memory unbounded before flush_ice().
            let mut q = self.pending_ice.lock().unwrap();
            if q.len() < MAX_PENDING_ICE {
                q.push(cand);
            }
        }
    }
    async fn flush_ice(&self) {
        self.remote_set.store(true, Ordering::SeqCst);
        self.diag.remote_desc.store(true, Ordering::SeqCst);
        let pending = std::mem::take(&mut *self.pending_ice.lock().unwrap());
        for c in pending {
            if let Ok(init) = serde_json::from_str::<RTCIceCandidateInit>(&c) {
                let _ = self.pc.add_ice_candidate(init).await;
            }
        }
    }
}

fn send_ctrl(dc: &Arc<RTCDataChannel>, msg: &CtrlMsg) {
    if let Ok(j) = serde_json::to_string(msg) {
        let dc = dc.clone();
        tokio::spawn(async move {
            let _ = dc.send_text(j).await;
        });
    }
}

fn hello_ctrl(h: &Hello) -> CtrlMsg {
    CtrlMsg::KemHello { kem_pk: STANDARD.encode(&h.kem_pk), x_pk: STANDARD.encode(h.x_pk) }
}
fn reply_ctrl(r: &Reply) -> CtrlMsg {
    CtrlMsg::KemReply { x_pk: STANDARD.encode(r.x_pk), kem_ct: STANDARD.encode(&r.kem_ct) }
}
fn parse_hello(kem_pk: &str, x_pk: &str) -> Option<Hello> {
    let kem_pk = STANDARD.decode(kem_pk).ok()?;
    let x: [u8; 32] = STANDARD.decode(x_pk).ok()?.try_into().ok()?;
    Some(Hello { kem_pk, x_pk: x })
}
fn parse_reply(x_pk: &str, kem_ct: &str) -> Option<Reply> {
    let x: [u8; 32] = STANDARD.decode(x_pk).ok()?.try_into().ok()?;
    let kem_ct = STANDARD.decode(kem_ct).ok()?;
    Some(Reply { x_pk: x, kem_ct })
}

/// Route a decrypted/plaintext inner control message (chat or channel announce).
fn dispatch_inner(peer: &str, msg: CtrlMsg, events: &UnboundedSender<MeshEvent>, chan: &Arc<ChanState>) {
    match msg {
        CtrlMsg::Chat(mut c) => {
            if c.text.chars().count() > crate::MAX_CHAT_CHARS {
                c.text = c.text.chars().take(crate::MAX_CHAT_CHARS).collect();
            }
            let _ = events.send(MeshEvent::Chat { from: peer.to_string(), text: c.text });
        }
        CtrlMsg::Channel { name } => {
            let name: String = name.chars().take(crate::MAX_CHANNEL_LEN).collect();
            chan.set_peer(peer, name.clone());
            let _ = events.send(MeshEvent::PeerChannel { peer: peer.to_string(), name });
        }
        CtrlMsg::Channels { names } => {
            // Clamp count + each name before handing the untrusted directory up.
            let names: Vec<String> = names
                .into_iter()
                .take(crate::MAX_CHANNELS)
                .map(|n| n.chars().take(crate::MAX_CHANNEL_LEN).collect())
                .collect();
            let _ = events.send(MeshEvent::PeerChannels { names });
        }
        CtrlMsg::ChannelRemoved { name } => {
            let name: String = name.chars().take(crate::MAX_CHANNEL_LEN).collect();
            let _ = events.send(MeshEvent::PeerChannelRemoved { name });
        }
        CtrlMsg::RoomKey { gen, key } => {
            // Arrives already decrypted (unwrapped from `Enc`), so `peer` is the
            // DTLS+PQC-authenticated sender. Carry THAT identity up as the
            // claimed authority — never a value from the message body — so the
            // engine can verify the sender really is the elected authority.
            if let Ok(bytes) = STANDARD.decode(&key) {
                if let Ok(arr) = <[u8; 32]>::try_from(bytes.as_slice()) {
                    let _ = events.send(MeshEvent::RoomKey { from: peer.to_string(), gen, key: arr });
                }
            }
        }
        CtrlMsg::RoomGen { gen } => {
            // Same authenticated-sender reasoning as RoomKey: this arrives already
            // unwrapped from `Enc`, so `peer` is the DTLS+PQC-authenticated sender.
            let _ = events.send(MeshEvent::RoomGen { from: peer.to_string(), gen });
        }
        _ => {}
    }
}

/// Wire a peer's DataChannel: drive the PQC handshake, route inbound control
/// messages (decrypting once a session exists), and stash the channel for
/// outbound sends. On open, the initiator sends its KemHello and both sides
/// announce their current channel.
fn wire_ctrl(
    peer: String,
    slot: ChatSlot,
    dc: Arc<RTCDataChannel>,
    events: UnboundedSender<MeshEvent>,
    chan: Arc<ChanState>,
    crypto: Arc<PeerCrypto>,
) {
    // On open: kick off the handshake (initiator) + announce our channel.
    {
        let (dc_o, peer_o, chan_o, crypto_o) = (dc.clone(), peer.clone(), chan.clone(), crypto.clone());
        dc.on_open(Box::new(move || {
            let (dc_o, peer_o, chan_o, crypto_o) = (dc_o.clone(), peer_o.clone(), chan_o.clone(), crypto_o.clone());
            Box::pin(async move {
                if let Some(hello) = crypto_o.begin(&peer_o) {
                    send_ctrl(&dc_o, &hello_ctrl(&hello));
                }
                send_ctrl(&dc_o, &CtrlMsg::Channel { name: chan_o.mine() });
                // Hand the newcomer our channel directory so channels created
                // before it joined (incl. empty ones) are selectable for it.
                send_ctrl(&dc_o, &CtrlMsg::Channels { names: chan_o.dir() });
            })
        }));
    }

    let p = peer.clone();
    let slot_dc = dc.clone(); // stashed below; the closure moves its own handle
    let msg_dc = dc.clone(); // moved into the on_message closure (not the outer `dc`)
    dc.on_message(Box::new(move |msg: DataChannelMessage| {
        let (p, events, chan, crypto, dc) = (p.clone(), events.clone(), chan.clone(), crypto.clone(), msg_dc.clone());
        Box::pin(async move {
            if msg.data.len() > crate::MAX_CTRL_BYTES {
                return; // drop oversized frame before deserializing
            }
            match serde_json::from_slice::<CtrlMsg>(&msg.data) {
                Ok(CtrlMsg::KemHello { kem_pk, x_pk }) => {
                    if let Some(hello) = parse_hello(&kem_pk, &x_pk) {
                        if let Some(reply) = crypto.on_hello(&p, &hello) {
                            send_ctrl(&dc, &reply_ctrl(&reply));
                            let fingerprint = crypto.fingerprint(&p).unwrap_or_default();
                            let _ = events.send(MeshEvent::Secure { peer: p, fingerprint });
                        }
                    }
                }
                Ok(CtrlMsg::KemReply { x_pk, kem_ct }) => {
                    if let Some(reply) = parse_reply(&x_pk, &kem_ct) {
                        if crypto.on_reply(&p, &reply) {
                            let fingerprint = crypto.fingerprint(&p).unwrap_or_default();
                            let _ = events.send(MeshEvent::Secure { peer: p, fingerprint });
                        }
                    }
                }
                Ok(CtrlMsg::Enc { data }) => {
                    if let Ok(wire) = STANDARD.decode(&data) {
                        if let Some(pt) = crypto.open(&p, Kind::Chat, &wire) {
                            if let Ok(inner) = serde_json::from_slice::<CtrlMsg>(&pt) {
                                dispatch_inner(&p, inner, &events, &chan);
                            }
                        }
                    }
                }
                // Plaintext chat/channel (pre-handshake or a peer with no session).
                Ok(inner @ (CtrlMsg::Chat(_) | CtrlMsg::Channel { .. } | CtrlMsg::Channels { .. } | CtrlMsg::ChannelRemoved { .. })) => {
                    dispatch_inner(&p, inner, &events, &chan);
                }
                Ok(_) => {}
                // Not a CtrlMsg (e.g. a pre-0.4.0 bare ChatMsg): dropped. No
                // cross-version compatibility on the DataChannel.
                Err(_) => {}
            }
        })
    }));
    *slot.lock().unwrap() = Some(slot_dc);
}

pub struct Mesh {
    api: Arc<API>,
    local: Arc<TrackLocalStaticSample>,
    my_id: String,
    out: UnboundedSender<ClientMsg>,
    decode_tx: DecodeTx,
    ice_servers: Vec<RTCIceServer>,
    peers: HashMap<String, PeerConn>,
    events: UnboundedSender<MeshEvent>,
    chan: Arc<ChanState>,
    crypto: Arc<PeerCrypto>,
    room: Arc<RoomAudio>,
}

impl Mesh {
    // Crate-internal: it takes a `MeshEvent` sender, which is `pub(crate)`. As
    // `pub` it was unusable from outside anyway, and rustc warned about the
    // leaked private type.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        api: Arc<API>,
        local: Arc<TrackLocalStaticSample>,
        my_id: String,
        out: UnboundedSender<ClientMsg>,
        decode_tx: DecodeTx,
        events: UnboundedSender<MeshEvent>,
        chan: Arc<ChanState>,
        crypto: Arc<PeerCrypto>,
        room: Arc<RoomAudio>,
    ) -> Self {
        let ice_servers = vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }];
        Self { api, local, my_id, out, decode_tx, ice_servers, peers: HashMap::new(), events, chan, crypto, room }
    }

    pub fn add_turn(&mut self, urls: Vec<String>, username: String, credential: String) {
        self.ice_servers.push(RTCIceServer { urls, username, credential, ..Default::default() });
    }

    fn config(&self) -> RTCConfiguration {
        RTCConfiguration { ice_servers: self.ice_servers.clone(), ..Default::default() }
    }

    async fn ensure(&mut self, peer: &str) -> Result<()> {
        if self.peers.contains_key(peer) {
            return Ok(());
        }
        let pc = Arc::new(self.api.new_peer_connection(self.config()).await?);

        // Send our audio to this peer (shared track → encode-once).
        let sender = pc.add_track(Arc::clone(&self.local) as Arc<dyn TrackLocal + Send + Sync>).await?;
        tokio::spawn(async move {
            let mut b = vec![0u8; 1500];
            while sender.read(&mut b).await.is_ok() {}
        });

        // What this link manages to gather / receive — read if it ever dies.
        let diag = Arc::new(LinkDiag::default());

        // Local ICE candidates → signaling.
        let out = self.out.clone();
        let to = peer.to_string();
        let diag_ice = diag.clone();
        pc.on_ice_candidate(Box::new(move |c| {
            let out = out.clone();
            let to = to.clone();
            let diag_ice = diag_ice.clone();
            Box::pin(async move {
                if let Some(c) = c {
                    if let Ok(init) = c.to_json() {
                        diag_ice.note_local(&init.candidate);
                        if let Ok(s) = serde_json::to_string(&init) {
                            let _ = out.send(ClientMsg::Ice { to, candidate: s });
                        }
                    }
                }
            })
        }));

        // Connection-state + transparency badge (DIREKT vs RELAY/TURN).
        let pid_s = peer.to_string();
        let pc_state = Arc::clone(&pc);
        let ev_state = self.events.clone();
        let diag_state = diag.clone();
        pc.on_peer_connection_state_change(Box::new(move |s| {
            let pid_s = pid_s.clone();
            let pc_state = Arc::clone(&pc_state);
            let ev_state = ev_state.clone();
            let diag_state = diag_state.clone();
            Box::pin(async move {
                match s {
                    RTCPeerConnectionState::Connected => {
                        diag_state.paired.store(true, Ordering::SeqCst);
                        let badge = selected_kind(&pc_state).await.unwrap_or("DIREKT").to_string();
                        let _ = ev_state.send(MeshEvent::Badge { peer: pid_s.clone(), badge });
                        let _ = ev_state.send(MeshEvent::Link { peer: pid_s, up: true });
                    }
                    // A dead link used to be invisible: no badge, no audio, no
                    // channel announces, and nothing telling the engine to retry.
                    // Report it so the loop can rebuild the connection and the UI
                    // can say so instead of showing a silently mute member.
                    RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Disconnected
                    | RTCPeerConnectionState::Closed => {
                        let _ = ev_state.send(MeshEvent::Link { peer: pid_s, up: false });
                    }
                    _ => {}
                }
            })
        }));

        // Remote audio track → decode pipeline (channel RX-gate inside read_track).
        let dtx = self.decode_tx.clone();
        let pid = peer.to_string();
        let chan_rt = self.chan.clone();
        let room_rt = self.room.clone();
        pc.on_track(Box::new(move |track: Arc<TrackRemote>, _, _| {
            let dtx = dtx.clone();
            let pid = pid.clone();
            let chan_rt = chan_rt.clone();
            let room_rt = room_rt.clone();
            Box::pin(async move {
                tokio::spawn(read_track(track, pid, dtx, chan_rt, room_rt));
            })
        }));

        // Answerer side accepts the chat DataChannel the offerer creates.
        let chat: ChatSlot = Arc::new(Mutex::new(None));
        let chat_h = chat.clone();
        let pid2 = peer.to_string();
        let ev_chat = self.events.clone();
        let chan_dc = self.chan.clone();
        let crypto_dc = self.crypto.clone();
        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let chat_h = chat_h.clone();
            let pid2 = pid2.clone();
            let ev_chat = ev_chat.clone();
            let chan_dc = chan_dc.clone();
            let crypto_dc = crypto_dc.clone();
            Box::pin(async move {
                wire_ctrl(pid2, chat_h, dc, ev_chat, chan_dc, crypto_dc);
            })
        }));

        self.peers.insert(
            peer.to_string(),
            PeerConn {
                pc,
                pending_ice: Mutex::new(Vec::new()),
                remote_set: AtomicBool::new(false),
                chat,
                diag,
            },
        );
        Ok(())
    }

    /// Saw a peer (roster or peer-joined): establish/refresh the link. Only the
    /// smaller id offers (glare rule).
    ///
    /// A peer can appear here for a true (re)join — their process restarted, so
    /// their old PC is gone — or for a bare signaling reconnect with the mesh
    /// still alive. In both cases the offerer must REBUILD its PC: reusing the
    /// old one only renegotiates SDP without restarting ICE, so after a rejoin
    /// the offerer's half stays wired to a dead transport → no media. Tearing it
    /// down first restarts ICE; on a signaling reconnect it's a harmless rebuild.
    /// The answerer keeps/creates a PC and lets `on_offer` swap in a fresh one
    /// when the offer arrives (it detects an already-negotiated PC). This mirrors
    /// `rekey()`'s smaller-side-re-offers handshake, so each pair gets exactly
    /// one offerer.
    pub async fn on_peer(&mut self, peer: &str) -> Result<()> {
        if self.my_id.as_str() < peer {
            if let Some(old) = self.peers.remove(peer) {
                self.crypto.remove(peer); // stale session; a fresh DC re-handshakes
                let _ = old.pc.close().await;
            }
            self.ensure(peer).await?;
            self.offer(peer).await?;
        } else {
            self.ensure(peer).await?;
        }
        Ok(())
    }

    /// Why the link to `peer` is not up, in one line for the user's log. A peer
    /// with no connection object at all never got as far as an offer.
    pub fn link_why(&self, peer: &str) -> String {
        match self.peers.get(peer) {
            Some(p) => p.diag.summary(),
            None => "keine Verbindung aufgebaut".to_string(),
        }
    }

    /// Rebuild the connection to `peer` from scratch and re-offer, whatever
    /// state it was in. Unlike [`Mesh::on_peer`] this ignores the glare rule —
    /// the caller (the engine's link watchdog) staggers the two sides by time
    /// instead, so a pair whose ICE never came up still gets retried when the
    /// side that would normally offer is the unreachable one.
    /// That means both sides can offer at once; [`Mesh::on_offer`] resolves the
    /// collision (the smaller id keeps its offer, the larger one answers).
    pub async fn relink(&mut self, peer: &str) -> Result<()> {
        // Never tear down a negotiation that is still running — a slow ICE run
        // is not a dead link, and rebuilding it would restart the clock forever.
        // Only an absent, failed, disconnected or closed connection is rebuilt;
        // webrtc-rs reaches those states on its own when checking gives up.
        //
        // One exception: a PC with no remote description never leaves `New` —
        // ICE cannot start without the peer's candidates — so `New` there means
        // "the offer or answer got lost", not "still negotiating". The watchdog
        // stagger (LINK_RETRY_*) already gives a fresh offer time to be answered.
        if let Some(p) = self.peers.get(peer) {
            let negotiating = p.remote_set.load(Ordering::SeqCst);
            match p.pc.connection_state() {
                RTCPeerConnectionState::New | RTCPeerConnectionState::Connecting if negotiating => {
                    return Ok(())
                }
                _ => {}
            }
        }
        if let Some(old) = self.peers.remove(peer) {
            self.crypto.remove(peer); // stale session; a fresh DC re-handshakes
            let _ = old.pc.close().await;
        }
        self.ensure(peer).await?;
        self.offer(peer).await?;
        Ok(())
    }

    async fn offer(&mut self, peer: &str) -> Result<()> {
        let p = self.peers.get(peer).unwrap();
        // Offerer creates the chat channel (must exist before the offer).
        let dc = p.pc.create_data_channel("chat", None).await?;
        wire_ctrl(peer.to_string(), p.chat.clone(), dc, self.events.clone(), self.chan.clone(), self.crypto.clone());

        let offer = p.pc.create_offer(None).await?;
        p.pc.set_local_description(offer.clone()).await?;
        let _ = self.out.send(ClientMsg::Offer { to: peer.to_string(), sdp: offer.sdp });
        Ok(())
    }

    pub async fn on_offer(&mut self, from: &str, sdp: String) -> Result<()> {
        // Glare: both sides offered. The designated offerer (smaller id) keeps its
        // offer and ignores the peer's; the other side drops its own half-built PC
        // and answers instead.
        if let Some(p) = self.peers.get(from) {
            if p.pc.signaling_state() == RTCSignalingState::HaveLocalOffer {
                if self.my_id.as_str() < from {
                    return Ok(());
                }
                if let Some(old) = self.peers.remove(from) {
                    self.crypto.remove(from);
                    let _ = old.pc.close().await;
                }
            }
        }
        // A fresh offer for an already-established peer is a renegotiation /
        // rekey: drop the old PC so ensure() builds a new one with new keys.
        if let Some(p) = self.peers.get(from) {
            if p.remote_set.load(Ordering::SeqCst) {
                if let Some(old) = self.peers.remove(from) {
                    self.crypto.remove(from); // stale session; the new DC re-handshakes
                    let _ = old.pc.close().await;
                }
            }
        }
        self.ensure(from).await?;
        let p = self.peers.get(from).unwrap();
        p.pc.set_remote_description(RTCSessionDescription::offer(sdp)?).await?;
        p.flush_ice().await;
        let answer = p.pc.create_answer(None).await?;
        p.pc.set_local_description(answer.clone()).await?;
        let _ = self.out.send(ClientMsg::Answer { to: from.to_string(), sdp: answer.sdp });
        Ok(())
    }

    pub async fn on_answer(&mut self, from: &str, sdp: String) -> Result<()> {
        if let Some(p) = self.peers.get(from) {
            p.pc.set_remote_description(RTCSessionDescription::answer(sdp)?).await?;
            p.flush_ice().await;
        }
        Ok(())
    }

    pub async fn on_ice(&mut self, from: &str, cand: String) {
        if let Some(p) = self.peers.get(from) {
            p.add_or_queue_ice(cand).await;
        }
    }

    /// Tear down every link: close each PC and drop its crypto session. Used on
    /// engine shutdown so peers see the DTLS/ICE teardown right away instead of
    /// a ghost member that only fades once their ICE times out.
    pub async fn close_all(&mut self) {
        for (id, p) in self.peers.drain() {
            self.crypto.remove(&id);
            let _ = p.pc.close().await;
        }
    }

    pub async fn on_left(&mut self, peer: &str) {
        self.crypto.remove(peer);
        if let Some(p) = self.peers.remove(peer) {
            let _ = p.pc.close().await;
        }
    }

    /// Room-wide key rotation: re-handshake every link to get fresh DTLS-SRTP
    /// keys. Glare-aware to avoid a teardown race: for each pair only the
    /// smaller user_id tears down + re-offers; the larger side keeps its old PC
    /// and lets `on_offer` replace it when the fresh offer arrives. Everyone
    /// receives the broadcast Rekey, so every pair gets exactly one re-offer.
    pub async fn rekey(&mut self) -> Result<()> {
        let ids: Vec<String> = self.peers.keys().cloned().collect();
        for id in &ids {
            if self.my_id.as_str() < id.as_str() {
                if let Some(old) = self.peers.remove(id) {
                    let _ = old.pc.close().await;
                }
                self.on_peer(id).await?; // ensure a fresh PC + re-offer
            }
            // else: answerer — on_offer() will swap in the new PC.
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn peer_pc(&self, id: &str) -> Option<Arc<RTCPeerConnection>> {
        self.peers.get(id).map(|p| p.pc.clone())
    }

    /// Sum transport bytes (sent, received) across all peer connections.
    /// Counts the real DTLS/SRTP+SCTP traffic — for live up/down bandwidth.
    pub async fn stats_bytes(&self) -> (u64, u64) {
        let mut up = 0u64;
        let mut down = 0u64;
        for p in self.peers.values() {
            let report = p.pc.get_stats().await;
            for v in report.reports.values() {
                if let webrtc::stats::StatsReportType::Transport(t) = v {
                    up += t.bytes_sent as u64;
                    down += t.bytes_received as u64;
                }
            }
        }
        (up, down)
    }

    /// Announce my (just-switched) channel to every connected peer's DataChannel.
    pub async fn broadcast_channel(&self, name: &str) {
        let json = match serde_json::to_string(&CtrlMsg::Channel { name: name.to_string() }) {
            Ok(j) => j,
            Err(_) => return,
        };
        for p in self.peers.values() {
            let dc = p.chat.lock().unwrap().clone();
            if let Some(dc) = dc {
                let _ = dc.send_text(json.clone()).await;
            }
        }
    }

    /// Announce my full channel directory to every peer (shared-directory sync).
    pub async fn broadcast_channels(&self, names: &[String]) {
        let json = match serde_json::to_string(&CtrlMsg::Channels { names: names.to_vec() }) {
            Ok(j) => j,
            Err(_) => return,
        };
        for p in self.peers.values() {
            let dc = p.chat.lock().unwrap().clone();
            if let Some(dc) = dc {
                let _ = dc.send_text(json.clone()).await;
            }
        }
    }

    /// Tell every peer a channel was deleted from the shared directory.
    pub async fn broadcast_channel_removed(&self, name: &str) {
        let json = match serde_json::to_string(&CtrlMsg::ChannelRemoved { name: name.to_string() }) {
            Ok(j) => j,
            Err(_) => return,
        };
        for p in self.peers.values() {
            let dc = p.chat.lock().unwrap().clone();
            if let Some(dc) = dc {
                let _ = dc.send_text(json.clone()).await;
            }
        }
    }

    /// Broadcast a chat line over every peer's DataChannel — AEAD-sealed for
    /// peers with a PQC session, plaintext otherwise (pre-handshake fallback).
    pub async fn broadcast_chat(&self, text: &str) {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let chat = CtrlMsg::Chat(ChatMsg { text: text.to_string(), ts });
        let inner = match serde_json::to_vec(&chat) {
            Ok(b) => b,
            Err(_) => return,
        };
        for (id, p) in self.peers.iter() {
            let dc = p.chat.lock().unwrap().clone();
            let Some(dc) = dc else { continue };
            let out = match self.crypto.seal(id, Kind::Chat, &inner) {
                Some(wire) => CtrlMsg::Enc { data: STANDARD.encode(wire) },
                None => chat.clone(),
            };
            if let Ok(j) = serde_json::to_string(&out) {
                let _ = dc.send_text(j).await;
            }
        }
    }

    /// Send the current group-audio room key to one peer, SEALED over that
    /// peer's pairwise PQC session. No-op if the peer has no session yet (the
    /// engine retries when the peer becomes secure).
    pub async fn send_room_key(&self, peer: &str, gen: u32, key: &[u8; 32]) {
        self.send_sealed(peer, &CtrlMsg::RoomKey { gen, key: STANDARD.encode(key) }).await;
    }

    /// Tell `peer` (which we believe is the room-key authority) what generation
    /// the room is already on, so a late-joining authority doesn't mint a
    /// generation below the one everyone else has staged. Sealed like the key
    /// itself so the counter is bound to an authenticated sender.
    pub async fn send_room_gen(&self, peer: &str, gen: u32) {
        self.send_sealed(peer, &CtrlMsg::RoomGen { gen }).await;
    }

    /// AEAD-seal one control message into an `Enc` envelope and push it down the
    /// peer's DataChannel. Silently drops when there is no session or no channel
    /// yet — every caller is driven by an event that fires again.
    async fn send_sealed(&self, peer: &str, msg: &CtrlMsg) {
        let inner = match serde_json::to_vec(msg) {
            Ok(b) => b,
            Err(_) => return,
        };
        let Some(sealed) = self.crypto.seal(peer, Kind::Chat, &inner) else {
            return;
        };
        let Some(p) = self.peers.get(peer) else {
            return;
        };
        let dc = p.chat.lock().unwrap().clone();
        if let Some(dc) = dc {
            let env = CtrlMsg::Enc { data: STANDARD.encode(sealed) };
            if let Ok(j) = serde_json::to_string(&env) {
                let _ = dc.send_text(j).await;
            }
        }
    }
}

/// Classify the live connection from the selected ICE candidate pair:
/// any `relay` candidate → via TURN, else direct. Retries briefly because the
/// pair can lag the Connected state. (ARCHITECTURE §9 transparency.)
async fn selected_kind(pc: &RTCPeerConnection) -> Option<&'static str> {
    let sctp = pc.sctp();
    let dtls = sctp.transport();
    let ice = dtls.ice_transport();
    for _ in 0..10 {
        if let Some(pair) = ice.get_selected_candidate_pair().await {
            // Pair fields are private; its Display lists each candidate's type
            // ("host"/"srflx"/"prflx"/"relay"). A relay candidate ⇒ via TURN.
            let relay = format!("{pair}").contains("relay");
            return Some(if relay { "RELAY (TURN)" } else { "DIREKT" });
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    None
}

async fn read_track(
    track: Arc<TrackRemote>,
    peer: String,
    dtx: DecodeTx,
    chan: Arc<ChanState>,
    room: Arc<RoomAudio>,
) {
    while let Ok((pkt, _)) = track.read_rtp().await {
        // Channel RX-gate: only feed the decoder when this peer is tuned
        // to the same channel as me. Off-channel audio is dropped here.
        if !pkt.payload.is_empty() && chan.hears(&peer) {
            // Unseal the group-audio frame (raw passthrough pre-key).
            if let Some(opus) = room.open_inbound(&peer, &pkt.payload) {
                let _ = dtx.send((peer.clone(), Bytes::from(opus)));
            }
        }
    }
}

#[cfg(test)]
mod diag_tests {
    use super::*;

    const HOST: &str = "candidate:1 1 udp 2130706431 192.168.1.5 50000 typ host generation 0";
    const SRFLX: &str = "candidate:2 1 udp 1694498815 203.0.113.9 50001 typ srflx raddr 192.168.1.5 rport 50000";

    #[test]
    fn candidate_type_is_read_from_the_sdp_line() {
        assert_eq!(candidate_type(HOST).as_deref(), Some("host"));
        assert_eq!(candidate_type(SRFLX).as_deref(), Some("srflx"));
        assert_eq!(candidate_type("garbage"), None);
    }

    #[test]
    fn a_lost_offer_is_not_reported_as_a_blocked_path() {
        // We gathered candidates but the peer's offer/answer never arrived.
        let d = LinkDiag::default();
        d.note_local(HOST);
        assert!(d.summary().contains("Signalisierung"));
    }

    #[test]
    fn silence_from_the_peer_is_named_as_such() {
        let d = LinkDiag::default();
        d.note_local(HOST);
        d.remote_desc.store(true, Ordering::SeqCst);
        assert!(d.summary().contains("keine ICE-Kandidaten vom Gegenueber"));
    }

    #[test]
    fn candidates_on_both_sides_but_no_pair_is_a_blocked_path() {
        let d = LinkDiag::default();
        d.note_local(HOST);
        d.note_local(SRFLX);
        d.note_remote(SRFLX);
        d.remote_desc.store(true, Ordering::SeqCst);
        let s = d.summary();
        assert!(s.contains("kein Paar durchgekommen"), "{s}");
        assert!(s.contains("host,srflx"), "{s}"); // both local types listed
    }

    #[test]
    fn a_link_that_was_up_reports_a_drop_not_a_setup_failure() {
        let d = LinkDiag::default();
        d.note_local(HOST);
        d.note_remote(HOST);
        d.remote_desc.store(true, Ordering::SeqCst);
        d.paired.store(true, Ordering::SeqCst);
        assert!(d.summary().contains("abgebrochen"));
    }

    #[test]
    fn candidate_type_reads_a_json_wrapped_candidate_too() {
        let json = r#"{"candidate":"candidate:3 1 udp 1 10.0.0.1 1 typ relay raddr 1.2.3.4","sdpMid":"0"}"#;
        assert_eq!(candidate_type(json).as_deref(), Some("relay"));
        // Trailing punctuation from a JSON-quoted type is stripped.
        assert_eq!(candidate_type(r#"{"candidate":"… typ host"}"#).as_deref(), Some("host"));
        // A UDP host candidate ends right after its type, so in the JSON wrapper
        // the type is glued to the next field. This is what every remote host
        // candidate looks like on the wire; it used to read as `host","sdpMid…`.
        let glued = r#"{"candidate":"candidate:1 1 udp 2130706431 10.0.0.1 5000 typ host","sdpMid":"0","sdpMLineIndex":0}"#;
        assert_eq!(candidate_type(glued).as_deref(), Some("host"));
        assert_eq!(candidate_type("x typ "), None);
        assert_eq!(candidate_type("x typ \"\""), None);
        assert_eq!(candidate_type(""), None);
    }

    #[test]
    fn empty_candidate_sets_read_as_keine() {
        let d = LinkDiag::default();
        assert_eq!(LinkDiag::types(&d.local), "keine");
        d.note_local(SRFLX);
        d.note_local(SRFLX); // a set: no duplicates
        d.note_local(HOST);
        assert_eq!(LinkDiag::types(&d.local), "host,srflx");
        d.note_remote("not a candidate line");
        assert_eq!(LinkDiag::types(&d.remote), "keine");
    }
}

#[cfg(test)]
mod ctrl_tests {
    use super::*;
    use crate::crypto::Initiator;
    use tokio::sync::mpsc::unbounded_channel;

    fn hello() -> Hello {
        Initiator::start("a", "b").1
    }

    #[test]
    fn hello_and_reply_round_trip_through_their_ctrl_frames() {
        let h = hello();
        match hello_ctrl(&h) {
            CtrlMsg::KemHello { kem_pk, x_pk } => {
                let back = parse_hello(&kem_pk, &x_pk).unwrap();
                assert_eq!(back.kem_pk, h.kem_pk);
                assert_eq!(back.x_pk, h.x_pk);
            }
            other => panic!("{other:?}"),
        }
        let (r, _) = crate::crypto::respond("a", "b", &h).unwrap();
        match reply_ctrl(&r) {
            CtrlMsg::KemReply { x_pk, kem_ct } => {
                let back = parse_reply(&x_pk, &kem_ct).unwrap();
                assert_eq!(back.kem_ct, r.kem_ct);
                assert_eq!(back.x_pk, r.x_pk);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn malformed_handshake_frames_are_rejected() {
        let h = hello();
        let kem_b64 = STANDARD.encode(&h.kem_pk);
        assert!(parse_hello("not base64!", &STANDARD.encode(h.x_pk)).is_none());
        assert!(parse_hello(&kem_b64, "###").is_none());
        // X25519 key must be exactly 32 bytes.
        assert!(parse_hello(&kem_b64, &STANDARD.encode([0u8; 31])).is_none());
        assert!(parse_hello(&kem_b64, &STANDARD.encode([0u8; 33])).is_none());
        assert!(parse_reply(&STANDARD.encode([0u8; 16]), &STANDARD.encode([0u8; 8])).is_none());
        assert!(parse_reply(&STANDARD.encode([0u8; 32]), "***").is_none());
        // Length is all parse_reply checks; the KEM layer validates the ciphertext.
        assert!(parse_reply(&STANDARD.encode([0u8; 32]), &STANDARD.encode([0u8; 8])).is_some());
    }

    fn chan_and_events() -> (Arc<ChanState>, UnboundedSender<MeshEvent>, tokio::sync::mpsc::UnboundedReceiver<MeshEvent>) {
        let (tx, rx) = unbounded_channel();
        (Arc::new(ChanState::new()), tx, rx)
    }

    #[test]
    fn chat_text_is_clamped_to_the_character_limit() {
        let (chan, tx, mut rx) = chan_and_events();
        let long: String = "ä".repeat(crate::MAX_CHAT_CHARS + 50);
        dispatch_inner("p", CtrlMsg::Chat(ChatMsg { text: long, ts: 0 }), &tx, &chan);
        match rx.try_recv().unwrap() {
            MeshEvent::Chat { from, text } => {
                assert_eq!(from, "p");
                assert_eq!(text.chars().count(), crate::MAX_CHAT_CHARS);
            }
            _ => panic!("expected Chat"),
        }
    }

    #[test]
    fn channel_announce_updates_the_gate_state_and_is_clamped() {
        let (chan, tx, mut rx) = chan_and_events();
        let long: String = "x".repeat(crate::MAX_CHANNEL_LEN + 10);
        dispatch_inner("p", CtrlMsg::Channel { name: long }, &tx, &chan);
        let clamped: String = "x".repeat(crate::MAX_CHANNEL_LEN);
        assert_eq!(chan.peer("p").as_deref(), Some(clamped.as_str()));
        assert!(!chan.hears("p"), "peer moved off my channel");
        match rx.try_recv().unwrap() {
            MeshEvent::PeerChannel { peer, name } => {
                assert_eq!(peer, "p");
                assert_eq!(name, clamped);
            }
            _ => panic!("expected PeerChannel"),
        }
    }

    #[test]
    fn channel_directory_is_bounded_in_count_and_name_length() {
        let (chan, tx, mut rx) = chan_and_events();
        let names: Vec<String> = (0..crate::MAX_CHANNELS + 20)
            .map(|i| format!("{i}-{}", "y".repeat(crate::MAX_CHANNEL_LEN)))
            .collect();
        dispatch_inner("p", CtrlMsg::Channels { names }, &tx, &chan);
        match rx.try_recv().unwrap() {
            MeshEvent::PeerChannels { names } => {
                assert_eq!(names.len(), crate::MAX_CHANNELS);
                assert!(names.iter().all(|n| n.chars().count() == crate::MAX_CHANNEL_LEN));
            }
            _ => panic!("expected PeerChannels"),
        }
        dispatch_inner("p", CtrlMsg::ChannelRemoved { name: "z".repeat(99) }, &tx, &chan);
        match rx.try_recv().unwrap() {
            MeshEvent::PeerChannelRemoved { name } => assert_eq!(name.len(), crate::MAX_CHANNEL_LEN),
            _ => panic!("expected PeerChannelRemoved"),
        }
    }

    #[test]
    fn room_key_needs_exactly_32_bytes_and_names_the_channel_owner() {
        let (chan, tx, mut rx) = chan_and_events();
        dispatch_inner("auth", CtrlMsg::RoomKey { gen: 4, key: STANDARD.encode([9u8; 32]) }, &tx, &chan);
        match rx.try_recv().unwrap() {
            MeshEvent::RoomKey { from, gen, key } => {
                assert_eq!(from, "auth"); // the authenticated sender, never a body field
                assert_eq!(gen, 4);
                assert_eq!(key, [9u8; 32]);
            }
            _ => panic!("expected RoomKey"),
        }
        dispatch_inner("auth", CtrlMsg::RoomKey { gen: 4, key: STANDARD.encode([9u8; 31]) }, &tx, &chan);
        dispatch_inner("auth", CtrlMsg::RoomKey { gen: 4, key: "not-base64!".into() }, &tx, &chan);
        assert!(rx.try_recv().is_err(), "malformed keys must not surface");
        dispatch_inner("m", CtrlMsg::RoomGen { gen: 7 }, &tx, &chan);
        match rx.try_recv().unwrap() {
            MeshEvent::RoomGen { from, gen } => assert_eq!((from.as_str(), gen), ("m", 7)),
            _ => panic!("expected RoomGen"),
        }
    }

    #[test]
    fn handshake_and_envelope_variants_are_not_dispatched_as_inner_messages() {
        let (chan, tx, mut rx) = chan_and_events();
        dispatch_inner("p", hello_ctrl(&hello()), &tx, &chan);
        dispatch_inner("p", CtrlMsg::Enc { data: "AAAA".into() }, &tx, &chan);
        assert!(rx.try_recv().is_err());
    }
}

#[cfg(test)]
mod peer_conn_tests {
    use super::*;
    use crate::build_api;
    use tokio::sync::mpsc::unbounded_channel;
    use webrtc::api::media_engine::MIME_TYPE_OPUS;
    use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;

    fn track() -> Arc<TrackLocalStaticSample> {
        Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability { mime_type: MIME_TYPE_OPUS.to_owned(), clock_rate: 48000, channels: 2, ..Default::default() },
            "audio".to_owned(),
            "t".to_owned(),
        ))
    }

    fn mesh(id: &str) -> (Mesh, tokio::sync::mpsc::UnboundedReceiver<ClientMsg>) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (out_tx, out_rx) = unbounded_channel::<ClientMsg>();
        let (dec, mut dec_rx) = unbounded_channel::<(String, Bytes)>();
        let (ev, mut ev_rx) = unbounded_channel::<MeshEvent>();
        tokio::spawn(async move { while dec_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
        let m = Mesh::new(
            Arc::new(build_api().unwrap()), track(), id.into(), out_tx, dec, ev,
            Arc::new(ChanState::new()), Arc::new(PeerCrypto::new(id.into())), Arc::new(RoomAudio::new()),
        );
        (m, out_rx)
    }

    /// ICE frames that arrive before the remote description are queued — but
    /// only up to a cap, so a flood from one member cannot grow memory.
    #[tokio::test]
    async fn ice_queue_is_capped_until_the_remote_description_lands() {
        let (mut m, _out) = mesh("b");
        m.ensure("a").await.unwrap();
        let cand = r#"{"candidate":"candidate:1 1 udp 1 10.0.0.1 1 typ host","sdpMid":"0","sdpMLineIndex":0}"#;
        for _ in 0..(MAX_PENDING_ICE + 40) {
            m.on_ice("a", cand.to_string()).await;
        }
        let p = m.peers.get("a").unwrap();
        assert_eq!(p.pending_ice.lock().unwrap().len(), MAX_PENDING_ICE);
        assert!(!p.remote_set.load(Ordering::SeqCst));
        assert_eq!(LinkDiag::types(&p.diag.remote), "host");
        // Flushing marks the remote description as seen and drains the queue
        // (the PC has no remote description, so the adds fail silently).
        p.flush_ice().await;
        assert!(p.remote_set.load(Ordering::SeqCst));
        assert!(p.diag.remote_desc.load(Ordering::SeqCst));
        assert!(p.pending_ice.lock().unwrap().is_empty());
        // Garbage after the flush is ignored rather than queued.
        m.on_ice("a", "garbage".into()).await;
        assert!(m.peers.get("a").unwrap().pending_ice.lock().unwrap().is_empty());
    }

    /// The larger id never offers on its own: it builds a PC and waits.
    #[tokio::test]
    async fn answerer_side_builds_a_pc_but_sends_no_offer() {
        let (mut m, mut out) = mesh("b");
        m.on_peer("a").await.unwrap();
        assert!(m.peer_pc("a").is_some());
        assert!(out.try_recv().is_err(), "answerer sent something unprompted");
        // The offerer side does offer, and creates the chat channel first.
        let (mut o, mut out_o) = mesh("a");
        o.on_peer("b").await.unwrap();
        let first = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(ClientMsg::Offer { to, .. }) = out_o.recv().await {
                    return to;
                }
            }
        })
        .await
        .expect("no offer");
        assert_eq!(first, "b");
        assert!(o.peers.get("b").unwrap().chat.lock().unwrap().is_some(), "offerer must own the chat DC");
    }

    #[tokio::test]
    async fn unknown_peers_are_harmless_everywhere() {
        let (mut m, mut out) = mesh("a");
        assert_eq!(m.link_why("ghost"), "keine Verbindung aufgebaut");
        m.on_ice("ghost", "x".into()).await;
        m.on_answer("ghost", "v=0".into()).await.unwrap();
        m.on_left("ghost").await;
        m.broadcast_channel("c").await;
        m.broadcast_channels(&["c".into()]).await;
        m.broadcast_channel_removed("c").await;
        m.broadcast_chat("hi").await;
        m.send_room_key("ghost", 1, &[0u8; 32]).await;
        m.send_room_gen("ghost", 1).await;
        assert_eq!(m.stats_bytes().await, (0, 0));
        assert!(out.try_recv().is_err());
    }

    #[tokio::test]
    async fn add_turn_extends_the_ice_server_list() {
        let (mut m, _out) = mesh("a");
        assert_eq!(m.config().ice_servers.len(), 1);
        m.add_turn(vec!["turn:relay.example:3478".into()], "u".into(), "c".into());
        let cfg = m.config();
        assert_eq!(cfg.ice_servers.len(), 2);
        assert_eq!(cfg.ice_servers[1].username, "u");
        assert_eq!(cfg.ice_servers[1].urls, vec!["turn:relay.example:3478".to_string()]);
    }

    /// After `on_left` the peer's PC is closed and its PQC session forgotten.
    #[tokio::test]
    async fn on_left_closes_the_pc_and_forgets_the_session() {
        let (mut m, _out) = mesh("a");
        m.on_peer("b").await.unwrap();
        let pc = m.peer_pc("b").unwrap();
        m.on_left("b").await;
        assert!(m.peer_pc("b").is_none());
        assert_eq!(pc.connection_state(), RTCPeerConnectionState::Closed);
        assert!(!m.crypto.is_secure("b"));
        assert_eq!(m.link_why("b"), "keine Verbindung aufgebaut");
    }

    /// An invalid SDP in an offer is an error, not a panic, and leaves a PC
    /// behind that a later valid offer can replace.
    #[tokio::test]
    async fn garbage_offer_is_an_error_not_a_panic() {
        let (mut m, _out) = mesh("b");
        assert!(m.on_offer("a", "this is not sdp".into()).await.is_err());
        assert!(m.peer_pc("a").is_some());
    }
}

#[cfg(test)]
mod rekey_tests {
    use super::*;
    use crate::build_api;
    use tokio::sync::mpsc::unbounded_channel;
    use tokio::sync::Mutex as AsyncMutex;
    use webrtc::api::media_engine::MIME_TYPE_OPUS;
    use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;

    fn track() -> Arc<TrackLocalStaticSample> {
        Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: 48000,
                channels: 2,
                ..Default::default()
            },
            "audio".to_owned(),
            "t".to_owned(),
        ))
    }

    async fn wait_connected(m: &AsyncMutex<Mesh>, peer: &str) -> bool {
        for _ in 0..150 {
            if let Some(pc) = m.lock().await.peer_pc(peer) {
                if pc.connection_state() == RTCPeerConnectionState::Connected {
                    return true;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    /// Two real meshes over a mock relay: A tells B the room's key generation and
    /// B surfaces it as `MeshEvent::RoomGen`. Exercises the whole new wire path —
    /// seal over the pairwise PQC session, `Enc` envelope, `dispatch_inner` — so a
    /// mis-tagged variant or a missing dispatch arm fails here rather than in the
    /// field, where the symptom is silent audio for one member.
    #[tokio::test]
    async fn room_gen_reaches_the_peer_sealed() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (out_a_tx, mut out_a_rx) = unbounded_channel::<ClientMsg>();
        let (out_b_tx, mut out_b_rx) = unbounded_channel::<ClientMsg>();
        let (dec_a, mut dec_a_rx) = unbounded_channel::<(String, Bytes)>();
        let (dec_b, mut dec_b_rx) = unbounded_channel::<(String, Bytes)>();
        let (ev_a, mut ev_a_rx) = unbounded_channel::<MeshEvent>();
        let (ev_b, mut ev_b_rx) = unbounded_channel::<MeshEvent>();
        tokio::spawn(async move { while dec_a_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while dec_b_rx.recv().await.is_some() {} });

        let crypto_a = Arc::new(crate::crypto::PeerCrypto::new("a".into()));
        let a = Arc::new(AsyncMutex::new(Mesh::new(
            Arc::new(build_api().unwrap()), track(), "a".into(), out_a_tx, dec_a, ev_a,
            Arc::new(ChanState::new()), crypto_a.clone(),
            Arc::new(crate::crypto::RoomAudio::new()),
        )));
        let b = Arc::new(AsyncMutex::new(Mesh::new(
            Arc::new(build_api().unwrap()), track(), "b".into(), out_b_tx, dec_b, ev_b,
            Arc::new(ChanState::new()), Arc::new(crate::crypto::PeerCrypto::new("b".into())),
            Arc::new(crate::crypto::RoomAudio::new()),
        )));

        {
            let b2 = b.clone();
            tokio::spawn(async move {
                while let Some(msg) = out_a_rx.recv().await {
                    let mut g = b2.lock().await;
                    match msg {
                        ClientMsg::Offer { sdp, .. } => { let _ = g.on_offer("a", sdp).await; }
                        ClientMsg::Answer { sdp, .. } => { let _ = g.on_answer("a", sdp).await; }
                        ClientMsg::Ice { candidate, .. } => { g.on_ice("a", candidate).await; }
                        _ => {}
                    }
                }
            });
        }
        {
            let a2 = a.clone();
            tokio::spawn(async move {
                while let Some(msg) = out_b_rx.recv().await {
                    let mut g = a2.lock().await;
                    match msg {
                        ClientMsg::Offer { sdp, .. } => { let _ = g.on_offer("b", sdp).await; }
                        ClientMsg::Answer { sdp, .. } => { let _ = g.on_answer("b", sdp).await; }
                        ClientMsg::Ice { candidate, .. } => { g.on_ice("b", candidate).await; }
                        _ => {}
                    }
                }
            });
        }

        a.lock().await.on_peer("b").await.unwrap();
        assert!(wait_connected(&a, "b").await, "A->B connect failed");
        assert!(wait_connected(&b, "a").await, "B->A connect failed");

        // The seal only exists once the PQC handshake finished on A's side.
        let mut secure = false;
        for _ in 0..100 {
            if crypto_a.is_secure("b") {
                secure = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(secure, "PQC session A<->B never came up");

        a.lock().await.send_room_gen("b", 5).await;

        let mut got = None;
        for _ in 0..100 {
            match tokio::time::timeout(Duration::from_millis(100), ev_b_rx.recv()).await {
                Ok(Some(MeshEvent::RoomGen { from, gen })) => {
                    got = Some((from, gen));
                    break;
                }
                Ok(Some(_)) => continue, // Secure / Badge / Channel noise
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        // `from` must be the authenticated channel owner, never a body field.
        assert_eq!(got, Some(("a".to_string(), 5)), "B never got the sealed RoomGen");
        while ev_a_rx.try_recv().is_ok() {}
    }

    // Two real meshes wired through a mock relay: connect, rotate keys, and
    // verify both links re-handshake into brand-new PeerConnections (= new
    // DTLS-SRTP keys) and reach Connected again.
    #[tokio::test]
    async fn rekey_rebuilds_and_reconnects() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (out_a_tx, mut out_a_rx) = unbounded_channel::<ClientMsg>();
        let (out_b_tx, mut out_b_rx) = unbounded_channel::<ClientMsg>();
        let (dec_a, mut dec_a_rx) = unbounded_channel::<(String, Bytes)>();
        let (dec_b, mut dec_b_rx) = unbounded_channel::<(String, Bytes)>();
        let (ev_a, mut ev_a_rx) = unbounded_channel::<MeshEvent>();
        let (ev_b, mut ev_b_rx) = unbounded_channel::<MeshEvent>();
        tokio::spawn(async move { while dec_a_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while dec_b_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while ev_a_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while ev_b_rx.recv().await.is_some() {} });

        let a = Arc::new(AsyncMutex::new(Mesh::new(
            Arc::new(build_api().unwrap()), track(), "a".into(), out_a_tx, dec_a, ev_a,
            Arc::new(ChanState::new()), Arc::new(crate::crypto::PeerCrypto::new("a".into())),
            Arc::new(crate::crypto::RoomAudio::new()),
        )));
        let b = Arc::new(AsyncMutex::new(Mesh::new(
            Arc::new(build_api().unwrap()), track(), "b".into(), out_b_tx, dec_b, ev_b,
            Arc::new(ChanState::new()), Arc::new(crate::crypto::PeerCrypto::new("b".into())),
            Arc::new(crate::crypto::RoomAudio::new()),
        )));

        // Mock relay: forward each side's outbound to the other (stamped `from`).
        {
            let b2 = b.clone();
            tokio::spawn(async move {
                while let Some(msg) = out_a_rx.recv().await {
                    let mut g = b2.lock().await;
                    match msg {
                        ClientMsg::Offer { sdp, .. } => { let _ = g.on_offer("a", sdp).await; }
                        ClientMsg::Answer { sdp, .. } => { let _ = g.on_answer("a", sdp).await; }
                        ClientMsg::Ice { candidate, .. } => { g.on_ice("a", candidate).await; }
                        _ => {}
                    }
                }
            });
        }
        {
            let a2 = a.clone();
            tokio::spawn(async move {
                while let Some(msg) = out_b_rx.recv().await {
                    let mut g = a2.lock().await;
                    match msg {
                        ClientMsg::Offer { sdp, .. } => { let _ = g.on_offer("b", sdp).await; }
                        ClientMsg::Answer { sdp, .. } => { let _ = g.on_answer("b", sdp).await; }
                        ClientMsg::Ice { candidate, .. } => { g.on_ice("b", candidate).await; }
                        _ => {}
                    }
                }
            });
        }

        a.lock().await.on_peer("b").await.unwrap();
        assert!(wait_connected(&a, "b").await, "initial A->B connect failed");
        assert!(wait_connected(&b, "a").await, "initial B->A connect failed");

        let pc_a1 = a.lock().await.peer_pc("b").unwrap();
        let pc_b1 = b.lock().await.peer_pc("a").unwrap();

        // Broadcast Rekey reaches both clients.
        a.lock().await.rekey().await.unwrap();
        b.lock().await.rekey().await.unwrap();

        assert!(wait_connected(&a, "b").await, "A->B reconnect after rekey failed");
        assert!(wait_connected(&b, "a").await, "B->A reconnect after rekey failed");

        let pc_a2 = a.lock().await.peer_pc("b").unwrap();
        let pc_b2 = b.lock().await.peer_pc("a").unwrap();
        assert!(!Arc::ptr_eq(&pc_a1, &pc_a2), "A kept the same PC (no rekey)");
        assert!(!Arc::ptr_eq(&pc_b1, &pc_b2), "B kept the same PC (no rekey)");
    }

    async fn wait_offer(rx: &mut tokio::sync::mpsc::UnboundedReceiver<ClientMsg>) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(ClientMsg::Offer { .. })) => return true,
                Ok(Some(_)) => continue,
                _ => return false,
            }
        }
    }

    /// The offer was lost in transit (signaling hiccup), so the PC never gets
    /// an answer and sits in `New` forever. The watchdog's relink must rebuild
    /// and re-offer instead of reading `New` as "still negotiating".
    #[tokio::test]
    async fn relink_rebuilds_a_link_whose_offer_was_never_answered() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (out_tx, mut out_rx) = unbounded_channel::<ClientMsg>();
        let (dec, mut dec_rx) = unbounded_channel::<(String, Bytes)>();
        let (ev, mut ev_rx) = unbounded_channel::<MeshEvent>();
        tokio::spawn(async move { while dec_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
        let mut a = Mesh::new(
            Arc::new(build_api().unwrap()), track(), "a".into(), out_tx, dec, ev,
            Arc::new(ChanState::new()), Arc::new(crate::crypto::PeerCrypto::new("a".into())),
            Arc::new(crate::crypto::RoomAudio::new()),
        );

        a.on_peer("b").await.unwrap();
        assert!(wait_offer(&mut out_rx).await, "initial offer never sent");
        let pc1 = a.peer_pc("b").unwrap();
        assert_eq!(pc1.connection_state(), RTCPeerConnectionState::New);

        a.relink("b").await.unwrap();
        let pc2 = a.peer_pc("b").unwrap();
        assert!(!Arc::ptr_eq(&pc1, &pc2), "relink kept the unanswered PC");
        assert!(wait_offer(&mut out_rx).await, "relink sent no fresh offer");
    }

    /// Both sides relink before either offer is delivered (the watchdog may
    /// fire on both ends inside its stagger window). Each PC sits in
    /// `HaveLocalOffer` when the peer's offer arrives; `on_offer` must resolve
    /// the glare so the pair still converges to one working link.
    #[tokio::test]
    async fn simultaneous_relinks_converge_to_one_link_per_pair() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (out_a_tx, mut out_a_rx) = unbounded_channel::<ClientMsg>();
        let (out_b_tx, mut out_b_rx) = unbounded_channel::<ClientMsg>();
        let (dec_a, mut dec_a_rx) = unbounded_channel::<(String, Bytes)>();
        let (dec_b, mut dec_b_rx) = unbounded_channel::<(String, Bytes)>();
        let (ev_a, mut ev_a_rx) = unbounded_channel::<MeshEvent>();
        let (ev_b, mut ev_b_rx) = unbounded_channel::<MeshEvent>();
        tokio::spawn(async move { while dec_a_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while dec_b_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while ev_a_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while ev_b_rx.recv().await.is_some() {} });

        let a = Arc::new(AsyncMutex::new(Mesh::new(
            Arc::new(build_api().unwrap()), track(), "a".into(), out_a_tx, dec_a, ev_a,
            Arc::new(ChanState::new()), Arc::new(crate::crypto::PeerCrypto::new("a".into())),
            Arc::new(crate::crypto::RoomAudio::new()),
        )));
        let b = Arc::new(AsyncMutex::new(Mesh::new(
            Arc::new(build_api().unwrap()), track(), "b".into(), out_b_tx, dec_b, ev_b,
            Arc::new(ChanState::new()), Arc::new(crate::crypto::PeerCrypto::new("b".into())),
            Arc::new(crate::crypto::RoomAudio::new()),
        )));

        // Both relink first; nothing is delivered yet.
        a.lock().await.relink("b").await.unwrap();
        b.lock().await.relink("a").await.unwrap();

        // Hold back everything up to and including each side's offer, so both
        // offers are in flight before either is processed.
        async fn until_offer(rx: &mut tokio::sync::mpsc::UnboundedReceiver<ClientMsg>) -> Vec<ClientMsg> {
            let mut held = Vec::new();
            loop {
                let m = tokio::time::timeout(Duration::from_secs(3), rx.recv())
                    .await
                    .expect("no offer sent")
                    .expect("channel closed");
                let done = matches!(m, ClientMsg::Offer { .. });
                held.push(m);
                if done { return held; }
            }
        }
        let held_a = until_offer(&mut out_a_rx).await;
        let held_b = until_offer(&mut out_b_rx).await;

        // Relay: first the held messages, then whatever follows live.
        async fn relay(
            held: Vec<ClientMsg>,
            mut rx: tokio::sync::mpsc::UnboundedReceiver<ClientMsg>,
            to: Arc<AsyncMutex<Mesh>>,
            from: &'static str,
        ) {
            let mut pending: std::collections::VecDeque<ClientMsg> = held.into();
            loop {
                let msg = match pending.pop_front() {
                    Some(m) => m,
                    None => match rx.recv().await { Some(m) => m, None => return },
                };
                let mut g = to.lock().await;
                match msg {
                    ClientMsg::Offer { sdp, .. } => { let _ = g.on_offer(from, sdp).await; }
                    ClientMsg::Answer { sdp, .. } => { let _ = g.on_answer(from, sdp).await; }
                    ClientMsg::Ice { candidate, .. } => { g.on_ice(from, candidate).await; }
                    _ => {}
                }
            }
        }
        tokio::spawn(relay(held_a, out_a_rx, b.clone(), "a"));
        tokio::spawn(relay(held_b, out_b_rx, a.clone(), "b"));

        assert!(wait_connected(&a, "b").await, "A->B never connected after simultaneous relink");
        assert!(wait_connected(&b, "a").await, "B->A never connected after simultaneous relink");
    }

    /// Engine shutdown must close every PC: a dropped-but-open PC keeps ICE
    /// alive, so remote peers see a ghost member until their ICE times out.
    #[tokio::test]
    async fn close_all_closes_every_peer_connection() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (out_tx, mut out_rx) = unbounded_channel::<ClientMsg>();
        let (dec, mut dec_rx) = unbounded_channel::<(String, Bytes)>();
        let (ev, mut ev_rx) = unbounded_channel::<MeshEvent>();
        tokio::spawn(async move { while out_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while dec_rx.recv().await.is_some() {} });
        tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
        let mut a = Mesh::new(
            Arc::new(build_api().unwrap()), track(), "a".into(), out_tx, dec, ev,
            Arc::new(ChanState::new()), Arc::new(crate::crypto::PeerCrypto::new("a".into())),
            Arc::new(crate::crypto::RoomAudio::new()),
        );
        a.on_peer("b").await.unwrap();
        a.on_peer("c").await.unwrap();
        let pcs: Vec<_> = ["b", "c"].iter().map(|p| a.peer_pc(p).unwrap()).collect();

        a.close_all().await;

        assert!(a.peer_pc("b").is_none() && a.peer_pc("c").is_none(), "peers still registered");
        for pc in pcs {
            assert_eq!(pc.connection_state(), RTCPeerConnectionState::Closed, "PC left open");
        }
    }
}
