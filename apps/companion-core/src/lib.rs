//! companion-core engine: runs the encrypted P2P mesh (audio + chat) and
//! reports state to any frontend via a `Sink` callback. The headless bin and
//! the Tauri app both drive this same engine.

pub mod audio;
pub mod crypto;
pub mod mesh;
pub mod selfcheck;
pub mod serverless;
pub mod signaling;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use protocol::{ClientMsg, ServerMsg};
use serde::Serialize;
use tokio::sync::mpsc;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_OPUS};
use webrtc::api::{APIBuilder, API};
use webrtc::interceptor::registry::Registry;
use webrtc::media::Sample;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use audio::{Buf, MixMap};
use mesh::Mesh;

/// One room member as shown in the UI.
#[derive(Debug, Clone, Serialize)]
pub struct Participant {
    pub user_id: String,
    pub name: String,
    pub you: bool,
    /// "DIREKT" | "RELAY (TURN)" once the link is up; None while connecting.
    pub badge: Option<String>,
    pub speaking: bool,
    /// Current channel (frequency) name this member is tuned to. Members on a
    /// different channel than you are shown dimmed and you don't hear them.
    pub channel: String,
    /// True once the post-quantum (ML-KEM-768) session with this peer is up.
    pub secure: bool,
    /// True while the peer-to-peer connection to this member is up. False means
    /// no audio, no chat and no channel announces reach them — the UI must say
    /// so rather than showing a member who is silently mute.
    pub linked: bool,
}

/// Events the engine pushes to the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    Roster { participants: Vec<Participant> },
    Chat { from: String, text: String },
    Status { connected: bool, transmitting: bool },
    Log { text: String },
    /// Live network: connected peer count + measured up/down kbps.
    Net { peers: usize, up_kbps: u32, down_kbps: u32 },
    /// Encryption keys rotated: fresh DTLS-SRTP keys negotiated across the mesh.
    Rekeyed { generation: u32, by: String },
    /// Signaling link up/down. P2P audio is independent and keeps running while
    /// down; `up:false` should offer a "resume session" action.
    Signaling { up: bool },
    /// Confirms MY current channel (frequency) name after a switch / on connect.
    Channel { mine: String },
    /// The shared channel directory grew — a peer created/announced a channel
    /// not yet in the UI's list. The UI unions these into its switcher.
    Channels { names: Vec<String> },
    /// A channel was deleted from the shared directory — the UI drops it from
    /// its switcher. `name` is the canonical (match) form.
    ChannelRemoved { name: String },
    /// Group-audio encryption status: the installed room-key generation
    /// (`None` = still negotiating) and whether this node is the key authority.
    RoomAudio { gen: Option<u32>, authority: bool },
}

pub type Sink = Arc<dyn Fn(UiEvent) + Send + Sync>;

/// Inbound chat hard limits. A DataChannel peer is authenticated but not
/// trusted: drop oversized frames before deserializing and clamp text length so
/// a malicious member can't grow native/webview memory with one giant message.
pub(crate) const MAX_CHAT_BYTES: usize = 4 * 1024;
pub(crate) const MAX_CHAT_CHARS: usize = 2000;
/// Max DataChannel control-frame size (larger than chat to fit a KEM handshake:
/// base64 ML-KEM-768 key ≈ 1.6 KB, sealed chat a bit more).
pub(crate) const MAX_CTRL_BYTES: usize = 8 * 1024;

/// Internal events from the mesh layer back to the engine loop.
pub(crate) enum MeshEvent {
    Chat { from: String, text: String },
    Badge { peer: String, badge: String },
    /// A peer announced (over its DataChannel) the channel it's tuned to.
    PeerChannel { peer: String, name: String },
    /// A peer shared its channel directory (union-merge into ours).
    PeerChannels { names: Vec<String> },
    /// A peer deleted a channel from the shared directory (tombstone it).
    PeerChannelRemoved { name: String },
    /// The post-quantum session with this peer is established.
    Secure { peer: String },
    /// The peer connection came up (`up: true`) or died (failed / disconnected /
    /// closed). Drives the link watchdog and the roster's connection state.
    Link { peer: String, up: bool },
    /// A peer sent us the group-audio room key. `from` is the DTLS+PQC-
    /// authenticated sender (used to verify it's really the elected authority).
    RoomKey { from: String, gen: u32, key: [u8; 32] },
    /// A peer told us the room's current key generation. Only acted on when WE
    /// are the authority — it's how a late-joining authority learns the live
    /// generation instead of restarting the counter at 1.
    RoomGen { from: String, gen: u32 },
}

/// Default channel (frequency) every client starts on.
pub(crate) const DEFAULT_CHANNEL: &str = "Funk 1";
/// Max channel-name length accepted at the IPC boundary.
pub const MAX_CHANNEL_LEN: usize = 32;
/// Upper bound on the shared channel directory — clamps an inbound `Channels`
/// frame so a malicious peer can't grow the list without limit.
pub(crate) const MAX_CHANNELS: usize = 64;

/// Canonical form for channel matching: trim + lowercase (case-insensitive).
pub(crate) fn canon_channel(s: &str) -> String {
    s.trim().to_lowercase()
}

/// True if `me` is the room-key authority — the lexicographically smallest
/// user_id in the room (mirrors the handshake glare rule). The authority mints
/// and distributes the shared group-audio key.
fn am_authority(me: &str, members: &HashMap<String, Member>) -> bool {
    members.keys().all(|k| me < k.as_str())
}

/// True if `from` (an authenticated peer) is the room-key authority from OUR
/// view — smaller than us and than every other member. A room key is only
/// adopted from this peer, so a member can't inject/hijack the group key.
fn is_authority_peer(from: &str, me: &str, members: &HashMap<String, Member>) -> bool {
    from < me && members.keys().all(|k| k.as_str() == from || from < k.as_str())
}

/// Max generation jump we accept in one received room key. Bounds a malicious
/// authority (or a forged frame that slipped the authority check) from pinning
/// the room to `u32::MAX` so that no future rotation can ever exceed it. Large
/// enough for a reconnecting member to catch up across many rotations.
const ROOM_GEN_BOUND: u32 = 1024;

/// Grace between staging a new group-audio key (able to decrypt it) and
/// activating it for sealing. Long enough that every peer has received+staged
/// the key before anyone seals with it, so a rekey drops no audio.
const ROOM_KEY_GRACE: Duration = Duration::from_millis(400);

/// How long a node may sit with a working PQC link but no group-audio key before
/// the UI is told. Well past a normal handshake + key hand-out, so it only fires
/// when the authority is genuinely unreachable.
const KEYLESS_WARN_AFTER: Duration = Duration::from_secs(8);
/// How long a peer connection may stay down before the watchdog rebuilds it.
/// Staggered by side: the peer that owns the offer (smaller user_id, the glare
/// rule) retries first; the other side only steps in later, so a pair whose
/// offerer is the unreachable one still recovers without both re-offering at
/// once.
const LINK_RETRY_OFFERER: Duration = Duration::from_secs(8);
const LINK_RETRY_ANSWERER: Duration = Duration::from_secs(24);
/// How long a link stays down before we tell the user about that member.
const LINK_WARN_AFTER: Duration = Duration::from_secs(20);

/// Is another rebuild attempt for `peer` due? `down_for` is how long the link
/// has been down, `since_try` how long ago we last rebuilt it (equal to
/// `down_for` when we never did). The side that owns the offer under the glare
/// rule (smaller user_id) retries first; the other side waits longer so both
/// don't re-offer at once, but it does eventually step in — otherwise a pair
/// whose offerer is the unreachable one never recovers.
fn relink_due(me_id: &str, peer_id: &str, down_for: Duration, since_try: Duration) -> bool {
    let due = if me_id < peer_id { LINK_RETRY_OFFERER } else { LINK_RETRY_ANSWERER };
    down_for >= due && since_try >= due
}

/// Put the coordinator's decisions on the wire.
async fn perform_key_actions(mesh: &Mesh, actions: Vec<KeyAction>) {
    for a in actions {
        match a {
            KeyAction::SendKey { peer, gen, key } => mesh.send_room_key(&peer, gen, &key).await,
            KeyAction::SendGen { peer, gen } => mesh.send_room_gen(&peer, gen).await,
        }
    }
}

/// Members we currently hold a pairwise PQC session with — the only peers a room
/// key can be handed to, since it never travels outside one.
fn secure_peer_ids(
    members: &HashMap<String, Member>,
    crypto: &crypto::PeerCrypto,
) -> Vec<String> {
    members.keys().filter(|id| crypto.is_secure(id)).cloned().collect()
}

/// One thing the engine must put on the wire after the coordinator processed an
/// event. Returned rather than performed, so the coordinator carries no mesh,
/// socket or audio dependency and can be driven straight from a test.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyAction {
    /// Hand the current room key to `peer`, sealed over its pairwise session.
    SendKey { peer: String, gen: u32, key: [u8; 32] },
    /// Tell `peer` — whom we hold to be the authority — the live generation.
    SendGen { peer: String, gen: u32 },
}

/// Group-audio key coordination: who mints, at which generation, and who gets
/// told what.
///
/// Lifted out of the engine's `select!` arms on purpose. The rules here are
/// where the "one member silent in both directions" bug lived — a late-joining
/// authority minting generation 1 — and inside the loop they could only be
/// exercised by running three real clients with real audio devices. As a struct
/// they can be wired up three-at-a-time in a unit test (see
/// `room_key_tests::late_authority_converges_across_three_nodes`).
struct RoomKeyCoordinator {
    me_id: String,
    room: Arc<crypto::RoomAudio>,
    /// Highest generation ever minted or adopted. Deliberately monotonic and
    /// separate from the live key: a key can be superseded, but a fresh mint
    /// must still land above everything this node has already seen.
    room_gen: u32,
    promote_tx: mpsc::UnboundedSender<u32>,
}

impl RoomKeyCoordinator {
    fn new(
        me_id: String,
        room: Arc<crypto::RoomAudio>,
        promote_tx: mpsc::UnboundedSender<u32>,
    ) -> Self {
        RoomKeyCoordinator { me_id, room, room_gen: 0, promote_tx }
    }

    /// Newest staged generation, i.e. what the encryption footer shows.
    fn generation(&self) -> Option<u32> {
        self.room.generation()
    }

    /// Stage `k` for decryption immediately, then activate it for sealing — at
    /// once if it's our first key (nothing to coordinate), else after
    /// `ROOM_KEY_GRACE` so every peer has staged it before anyone seals with it.
    ///
    /// Returns whether the key was actually taken: `stage` rejects anything that
    /// isn't strictly better than what we hold, so a stale or duplicate key is a
    /// no-op and callers must not report a generation change for it.
    fn adopt(&mut self, k: crypto::RoomKey) -> bool {
        let first = !self.room.has_send();
        if !self.room.stage(k) {
            return false;
        }
        let g = self.room.generation().unwrap_or(0);
        self.room_gen = self.room_gen.max(g);
        if first {
            self.room.promote(g); // no prior key to coordinate → seal now
        } else {
            let ptx = self.promote_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(ROOM_KEY_GRACE).await;
                let _ = ptx.send(g);
            });
        }
        true
    }

    /// Authority action: mint the next generation, adopt it, and hand it to every
    /// currently-secure peer. `secure_peers` is the caller's view of which
    /// pairwise sessions exist — the key can only travel over one of those.
    fn rotate(&mut self, secure_peers: &[String]) -> Vec<KeyAction> {
        let g = self.room_gen.saturating_add(1);
        self.adopt(crypto::RoomKey::generate(g, self.me_id.clone()));
        let Some(k) = self.room.current() else { return Vec::new() };
        secure_peers
            .iter()
            .map(|id| KeyAction::SendKey {
                peer: id.clone(),
                gen: k.generation(),
                key: k.key_bytes(),
            })
            .collect()
    }

    /// The pairwise PQC session with `peer` came up.
    ///
    /// As authority: mint the room's first key if we hold none, then hand the
    /// current one over. As anyone else: if `peer` is the authority and we
    /// already hold a key, report our generation — that is what stops a peer who
    /// joined after us (ids are random, so a late joiner can be the authority)
    /// from starting the counter over at 1 and being rejected by everyone.
    fn on_secure(&mut self, peer: &str, members: &HashMap<String, Member>) -> Vec<KeyAction> {
        if am_authority(&self.me_id, members) {
            if self.room.generation().is_none() {
                self.room_gen = self.room_gen.max(1);
                let g = self.room_gen;
                self.adopt(crypto::RoomKey::generate(g, self.me_id.clone()));
            }
            match self.room.current() {
                Some(k) => vec![KeyAction::SendKey {
                    peer: peer.to_string(),
                    gen: k.generation(),
                    key: k.key_bytes(),
                }],
                None => Vec::new(),
            }
        } else if is_authority_peer(peer, &self.me_id, members) {
            match self.room.generation() {
                Some(gen) => vec![KeyAction::SendGen { peer: peer.to_string(), gen }],
                None => Vec::new(),
            }
        } else {
            Vec::new()
        }
    }

    /// A room key arrived from `from`. Adopted only when `from` really is the
    /// elected authority and the generation is plausible — otherwise any member
    /// could hijack the group key or pin it so no rotation can ever exceed it.
    /// Returns whether it was taken.
    fn on_room_key(
        &mut self,
        from: &str,
        gen: u32,
        key: [u8; 32],
        members: &HashMap<String, Member>,
    ) -> bool {
        is_authority_peer(from, &self.me_id, members)
            && gen <= self.room_gen.saturating_add(ROOM_GEN_BOUND)
            && self.adopt(crypto::RoomKey::from_bytes(gen, from.to_string(), key))
    }

    /// A member reported the room's generation. Only the authority mints, so only
    /// the authority acts on it. Rotating rather than adopting `gen` is what keeps
    /// this safe: we take the counter, never key material, and the key we mint at
    /// `gen + 1` then supersedes every staged key by the ordinary generation rule.
    fn on_room_gen(
        &mut self,
        from: &str,
        gen: u32,
        members: &HashMap<String, Member>,
        secure_peers: &[String],
    ) -> Vec<KeyAction> {
        if am_authority(&self.me_id, members)
            && members.contains_key(from)
            && gen > self.room_gen
            && gen <= self.room_gen.saturating_add(ROOM_GEN_BOUND)
        {
            self.room_gen = gen;
            self.rotate(secure_peers)
        } else {
            Vec::new()
        }
    }
}

/// Union `add` into the directory `dir` (dedupe by canonical form, bounded by
/// `MAX_CHANNELS`). Returns true if `dir` grew — the signal to re-broadcast.
pub(crate) fn merge_channels(dir: &mut Vec<String>, add: &[String]) -> bool {
    let mut grew = false;
    for name in add {
        if dir.len() >= MAX_CHANNELS {
            break;
        }
        let k = canon_channel(name);
        if k.is_empty() {
            continue;
        }
        if !dir.iter().any(|d| canon_channel(d) == k) {
            dir.push(name.clone());
            grew = true;
        }
    }
    grew
}

/// Remove the channel whose canonical form is `canon` from the directory `dir`.
/// Returns true if it was present (and dropped).
fn remove_from_dir(dir: &mut Vec<String>, canon: &str) -> bool {
    let before = dir.len();
    dir.retain(|d| canon_channel(d) != canon);
    dir.len() != before
}

/// True if I or any roster member is currently tuned to the channel `canon` —
/// such a channel must not be deleted (it's in use).
fn channel_in_use(canon: &str, my_channel: &str, members: &HashMap<String, Member>) -> bool {
    canon_channel(my_channel) == canon
        || members.values().any(|m| canon_channel(&m.channel) == canon)
}

/// Shared channel (frequency) state: my tuned channel + each peer's announced
/// channel. Lives behind an `Arc`; the mesh RX-gate consults it on the hot
/// audio path, the engine updates it on switch / peer announce. Names are stored
/// raw (display form); matching canonicalizes both sides.
pub struct ChanState {
    mine: Mutex<String>,
    peers: Mutex<HashMap<String, String>>,
    /// Shared channel directory I currently know (raw display form). The engine
    /// owns the canonical set; this copy lets the mesh announce it to a
    /// newly-connected peer on DataChannel open without reaching into the loop.
    dir: Mutex<Vec<String>>,
}
impl ChanState {
    fn new() -> Self {
        Self {
            mine: Mutex::new(DEFAULT_CHANNEL.to_string()),
            peers: Mutex::new(HashMap::new()),
            dir: Mutex::new(vec![DEFAULT_CHANNEL.to_string()]),
        }
    }
    /// The channel directory to announce to a peer (raw display form).
    pub fn dir(&self) -> Vec<String> {
        self.dir.lock().unwrap().clone()
    }
    /// Replace my known directory (the engine pushes the canonical set here).
    pub fn set_dir(&self, names: Vec<String>) {
        *self.dir.lock().unwrap() = names;
    }
    /// My current channel (raw display form).
    pub fn mine(&self) -> String {
        self.mine.lock().unwrap().clone()
    }
    pub fn set_mine(&self, name: String) {
        *self.mine.lock().unwrap() = name;
    }
    pub fn set_peer(&self, peer: &str, name: String) {
        self.peers.lock().unwrap().insert(peer.to_string(), name);
    }
    pub fn remove_peer(&self, peer: &str) {
        self.peers.lock().unwrap().remove(peer);
    }
    /// A peer's last-announced channel (raw display form), if any.
    pub fn peer(&self, peer: &str) -> Option<String> {
        self.peers.lock().unwrap().get(peer).cloned()
    }
    /// True if `peer`'s announced channel matches mine. An unannounced peer is
    /// assumed to be on the default channel (so initial audio isn't lost).
    pub fn hears(&self, peer: &str) -> bool {
        let mine = canon_channel(&self.mine.lock().unwrap());
        let theirs = self
            .peers
            .lock()
            .unwrap()
            .get(peer)
            .map(|s| canon_channel(s))
            .unwrap_or_else(|| canon_channel(DEFAULT_CHANNEL));
        mine == theirs
    }
}

pub struct EngineConfig {
    pub server: String,
    pub room: String,
    pub user_id: String,
    pub name: String,
    pub token: Option<String>,
    pub cert_sha256: Option<String>,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// Allow TURN relay fallback when the server offers creds. If false, only
    /// direct/STUN paths are used (no media via a relay).
    pub relay_enabled: bool,
}

enum Cmd {
    ToggleTx,
    SetTx(bool),
    Chat(String),
    Rekey,
    Reconnect,
    SetChannel(String),
    RemoveChannel(String),
}

/// Handle to the running engine; methods are non-blocking.
pub struct Engine {
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    gains: Arc<audio::Gains>,
    dsp: Arc<Mutex<audio::DspConfig>>,
    radio: Arc<Mutex<audio::RadioCfg>>,
    monitor: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    bitrate: Arc<AtomicU32>,
    dtx: Arc<AtomicBool>,
    dev_tx: std::sync::mpsc::Sender<audio::DevCmd>,
    earcon: Arc<audio::Earcon>,
}
impl Drop for Engine {
    fn drop(&mut self) {
        // Tell the audio threads to exit so a later reconnect doesn't stack
        // duplicate capture/playback rigs.
        self.stop.store(true, Ordering::SeqCst);
    }
}
impl Engine {
    pub fn toggle_transmit(&self) {
        let _ = self.cmd_tx.send(Cmd::ToggleTx);
    }
    /// Hold-to-talk: set transmit explicitly (idempotent).
    pub fn set_transmit(&self, on: bool) {
        let _ = self.cmd_tx.send(Cmd::SetTx(on));
    }
    pub fn send_chat(&self, text: String) {
        let _ = self.cmd_tx.send(Cmd::Chat(text));
    }
    /// Rotate the session encryption keys: triggers a room-wide DTLS-SRTP
    /// re-handshake so all links get fresh keys.
    pub fn rotate_key(&self) {
        let _ = self.cmd_tx.send(Cmd::Rekey);
    }
    /// Resume a dropped signaling link (reconnect + re-join) without tearing
    /// down the live P2P mesh.
    pub fn reconnect(&self) {
        let _ = self.cmd_tx.send(Cmd::Reconnect);
    }
    /// Switch to a named channel (frequency). Announced to all peers over the
    /// mesh DataChannels; you then only hear peers on the same channel.
    pub fn set_channel(&self, name: String) {
        let _ = self.cmd_tx.send(Cmd::SetChannel(name));
    }
    /// Delete an (empty) channel from the shared directory and tell every peer
    /// to drop + tombstone it. No-op on the base channel or one in use.
    pub fn remove_channel(&self, name: String) {
        let _ = self.cmd_tx.send(Cmd::RemoveChannel(name));
    }
    /// Live capture-path DSP config (noise gate / compressor / limiter).
    pub fn set_dsp(&self, cfg: audio::DspConfig) {
        *self.dsp.lock().unwrap() = cfg;
    }
    /// Receive-bus radio effect ("Funk-Effekt") — live, read by the mixer.
    pub fn set_radio(&self, cfg: audio::RadioCfg) {
        *self.radio.lock().unwrap() = cfg;
    }
    /// Mic self-check: route the (processed) mic to local playback.
    pub fn set_monitor(&self, on: bool) {
        self.monitor.store(on, Ordering::SeqCst);
    }
    /// Switch the capture device LIVE (no reconnect). `None` = system default.
    pub fn set_input_device(&self, name: Option<String>) {
        let _ = self.dev_tx.send(audio::DevCmd::SetInput(name));
    }
    /// Switch the playback device LIVE (no reconnect). `None` = system default.
    pub fn set_output_device(&self, name: Option<String>) {
        let _ = self.dev_tx.send(audio::DevCmd::SetOutput(name));
    }
    /// Low-bandwidth mode: drop Opus to ~14 kbps + app-level DTX (silence = no
    /// packets). Off = Opus auto bitrate, DTX off.
    pub fn set_low_bandwidth(&self, on: bool) {
        self.bitrate.store(if on { 14_000 } else { 0 }, Ordering::SeqCst);
        self.dtx.store(on, Ordering::SeqCst);
    }
    /// Overall output volume (0.0 mute … 1.0 normal … 2.0 +6 dB). Live.
    pub fn set_master_volume(&self, v: f32) {
        self.gains.set_master(v);
    }
    /// Per-participant output volume (by user_id). Live.
    pub fn set_peer_volume(&self, user_id: &str, v: f32) {
        self.gains.set_peer(user_id, v);
    }
    /// Toggle the local "Funk-Klick" earcon at the start of incoming transmissions.
    pub fn set_earcon(&self, on: bool) {
        self.earcon.set_enabled(on);
    }
    /// Volume of the local "Funk-Klick" earcon (0.0 mute … 1.0 normal … 2.0 +6 dB).
    pub fn set_earcon_volume(&self, v: f32) {
        self.earcon.set_volume(v);
    }
    /// Local confirmation tone when latched ("hands-free") push-to-talk engages
    /// (`on = true`) or releases. Independent of `set_earcon`.
    pub fn ptt_latch_cue(&self, on: bool) {
        self.earcon.latch_cue(on);
    }
}

pub(crate) fn build_api() -> Result<API> {
    let mut m = MediaEngine::default();
    m.register_default_codecs()?;
    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut m)?;
    Ok(APIBuilder::new().with_media_engine(m).with_interceptor_registry(registry).build())
}

/// Spin up the audio rig (device + encode/decode/mix threads) shared by both
/// the server-signaled mesh and the serverless 1:1 mode. Returns the transmit
/// flag, the encoded-Opus receiver (feed to the local track writer), and the
/// decode sender (feed remote RTP payloads in).
pub(crate) fn setup_audio(
    in_name: Option<String>,
    out_name: Option<String>,
) -> Result<(
    Arc<AtomicBool>,
    mpsc::UnboundedReceiver<Bytes>,
    mpsc::UnboundedSender<(String, Bytes)>,
    Arc<audio::Gains>,
    Arc<Mutex<audio::DspConfig>>,
    Arc<Mutex<audio::RadioCfg>>, // receive-bus radio effect ("Funk-Effekt")
    Arc<AtomicBool>, // mic self-check (monitor) toggle
    Arc<AtomicBool>, // shutdown flag for the audio threads
    Arc<AtomicU32>,  // encoder bitrate (0 = auto; low-bw mode)
    Arc<AtomicBool>, // app-level DTX toggle
    std::sync::mpsc::Sender<audio::DevCmd>, // live input/output device switch
    Arc<audio::Earcon>, // local "Funk-Klick" on incoming transmission start
)> {
    let cap: Buf = Arc::new(Mutex::new(VecDeque::new()));
    let play: Buf = Arc::new(Mutex::new(VecDeque::new()));
    let mix: MixMap = Arc::new(Mutex::new(HashMap::new()));
    let transmit = Arc::new(AtomicBool::new(false));
    let gains = Arc::new(audio::Gains::new());
    let dsp_cfg = Arc::new(Mutex::new(audio::DspConfig::default()));
    let radio_cfg = Arc::new(Mutex::new(audio::RadioCfg::default()));
    let monitor = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let bitrate = Arc::new(AtomicU32::new(0)); // 0 = Opus auto
    let dtx = Arc::new(AtomicBool::new(false));
    // Local earcon (default on): fires through the mixer, so it plays out the same
    // output device as voice. The frontend pushes the saved on/off after connect.
    let earcon = Arc::new(audio::Earcon::new(mix.clone(), Arc::new(AtomicBool::new(true))));
    // Live device rates: the device thread publishes the active rate here and the
    // encode/mixer resamplers retune when it changes (live device switch).
    let in_rate = Arc::new(AtomicU32::new(audio::OPUS_SR));
    let out_rate = Arc::new(AtomicU32::new(audio::OPUS_SR));
    let (dev_tx, dev_rx) = std::sync::mpsc::channel::<audio::DevCmd>();

    let (rate_tx, rate_rx) = std::sync::mpsc::channel::<(u32, u32)>();
    {
        let (cap, play, stop, in_rate, out_rate) =
            (cap.clone(), play.clone(), stop.clone(), in_rate.clone(), out_rate.clone());
        std::thread::spawn(move || {
            audio::run_devices(cap, play, rate_tx, in_name, out_name, stop, in_rate, out_rate, dev_rx)
        });
    }
    rate_rx.recv()?; // block until the devices are open (rates live in the atomics)

    let (opus_tx, opus_rx) = mpsc::unbounded_channel::<Bytes>();
    let (decode_tx, decode_rx) = mpsc::unbounded_channel::<(String, Bytes)>();
    {
        let (cap, transmit, dsp_cfg, mix, monitor, stop, bitrate, dtx, in_rate) = (
            cap.clone(), transmit.clone(), dsp_cfg.clone(), mix.clone(),
            monitor.clone(), stop.clone(), bitrate.clone(), dtx.clone(), in_rate.clone(),
        );
        std::thread::spawn(move || {
            audio::encode_loop(cap, in_rate, transmit, opus_tx, dsp_cfg, mix, monitor, stop, bitrate, dtx)
        });
    }
    {
        let mix = mix.clone();
        std::thread::spawn(move || audio::decode_loop(decode_rx, mix));
    }
    {
        let (gains, radio_cfg, stop) = (gains.clone(), radio_cfg.clone(), stop.clone());
        std::thread::spawn(move || audio::mixer_loop(mix, play, out_rate, gains, radio_cfg, stop));
    }

    Ok((transmit, opus_rx, decode_tx, gains, dsp_cfg, radio_cfg, monitor, stop, bitrate, dtx, dev_tx, earcon))
}

#[derive(Clone)]
struct Member {
    name: String,
    badge: Option<String>,
    speaking: bool,
    channel: String,
    secure: bool,
    /// Peer connection up? Starts false: a member is in the roster the moment
    /// signaling announces them, long before (or without ever) their ICE
    /// completing.
    linked: bool,
    /// Since when the link has been down (None while it is up).
    down_since: Option<std::time::Instant>,
    /// Last rebuild attempt by the link watchdog.
    last_relink: Option<std::time::Instant>,
    /// Whether the user has already been told about this dead link.
    link_warned: bool,
}

impl Member {
    fn new(name: String, channel: String, secure: bool) -> Self {
        Member {
            name,
            badge: None,
            speaking: false,
            channel,
            secure,
            linked: false,
            down_since: Some(std::time::Instant::now()),
            last_relink: None,
            link_warned: false,
        }
    }
}

fn emit_roster(
    sink: &Sink,
    members: &HashMap<String, Member>,
    me_id: &str,
    me_name: &str,
    me_channel: &str,
    transmitting: bool,
) {
    let mut participants = vec![Participant {
        user_id: me_id.to_string(),
        name: me_name.to_string(),
        you: true,
        badge: None,
        speaking: transmitting,
        channel: me_channel.to_string(),
        secure: true, // our own row: we always hold the crypto
        linked: true,
    }];
    let mut others: Vec<Participant> = members
        .iter()
        .map(|(id, m)| Participant {
            user_id: id.clone(),
            name: m.name.clone(),
            you: false,
            badge: m.badge.clone(),
            speaking: m.speaking,
            channel: m.channel.clone(),
            secure: m.secure,
            linked: m.linked,
        })
        .collect();
    others.sort_by(|a, b| a.name.cmp(&b.name));
    participants.extend(others);
    sink(UiEvent::Roster { participants });
}

/// Start the engine: open audio, connect signaling, join, and run the mesh.
/// Returns a handle; state flows out through `sink`.
pub async fn start(cfg: EngineConfig, sink: Sink) -> Result<Engine> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (transmit, mut opus_rx, decode_tx, gains, dsp_cfg, radio_cfg, monitor, stop, bitrate, dtx, dev_tx, earcon) =
        setup_audio(cfg.input_device.clone(), cfg.output_device.clone())?;
    // Cloned into the mesh loop (fires the click); the original stays in Engine
    // so the frontend can toggle it on/off.
    let earcon_loop = earcon.clone();

    let api = Arc::new(build_api()?);
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
    // Group-audio keying, shared by the outbound writer below, the mesh RX
    // tasks, and the engine loop (which installs keys from the authority).
    let room = Arc::new(crypto::RoomAudio::new());
    {
        let local = local.clone();
        let room = room.clone();
        let my_id_seal = cfg.user_id.clone();
        tokio::spawn(async move {
            while let Some(b) = opus_rx.recv().await {
                // Seal the Opus frame with the room key (raw passthrough until a
                // key is installed) — sealed ONCE here, then fanned out to every
                // peer, so encode-once is preserved.
                let payload = room.seal_outbound(&my_id_seal, &b);
                let sample = Sample {
                    data: Bytes::from(payload),
                    duration: Duration::from_millis(20),
                    ..Default::default()
                };
                let _ = local.write_sample(&sample).await;
            }
        });
    }

    let sig = signaling::connect(&cfg.server, cfg.cert_sha256.as_deref()).await?;
    let out = sig.out.clone();
    let incoming = sig.incoming;
    out.send(ClientMsg::Join {
        room: cfg.room.clone(),
        user_id: cfg.user_id.clone(),
        name: cfg.name.clone(),
        token: cfg.token.clone(),
    })?;
    sink(UiEvent::Status { connected: true, transmitting: false });

    let (mesh_tx, mut mesh_rx) = mpsc::unbounded_channel::<MeshEvent>();
    // Uplink: the mesh sends ClientMsg here; the loop forwards to the CURRENT
    // signaling connection, so the mesh survives a signaling reconnect.
    let (up_tx, mut up_rx) = mpsc::unbounded_channel::<ClientMsg>();
    // Shared channel (frequency) state: the mesh RX-gate reads it on the hot
    // audio path; the engine loop updates it on switch / peer announce.
    let chan = Arc::new(ChanState::new());
    // Per-peer post-quantum sessions (ML-KEM-768 + X25519), established over each
    // DataChannel; used to AEAD-seal chat + audio.
    let crypto = Arc::new(crypto::PeerCrypto::new(cfg.user_id.clone()));
    let mut mesh = Mesh::new(api, local, cfg.user_id.clone(), up_tx, decode_tx, mesh_tx, chan.clone(), crypto.clone(), room.clone());

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Cmd>();
    // Delayed group-audio key promotions: a grace timer fires the staged
    // generation here, and the loop activates it for sealing (see #ROOM_KEY_GRACE).
    let (promote_tx, mut promote_rx) = mpsc::unbounded_channel::<u32>();

    let me_id = cfg.user_id.clone();
    let me_name = cfg.name.clone();
    let relay_enabled = cfg.relay_enabled;
    // Retained so we can reconnect signaling (resume session) keeping the mesh.
    let rc_server = cfg.server.clone();
    let rc_cert = cfg.cert_sha256.clone();
    let rc_room = cfg.room.clone();
    let rc_token = cfg.token.clone();
    let rc_user = cfg.user_id.clone();
    let rc_name = cfg.name.clone();
    tokio::spawn(async move {
        let mut members: HashMap<String, Member> = HashMap::new();
        // My current channel (display form). Mirrors `chan.mine()`; kept here so
        // the roster + UiEvent::Channel show the exact name I typed.
        let mut my_channel = DEFAULT_CHANNEL.to_string();
        sink(UiEvent::Channel { mine: my_channel.clone() }); // tell the UI our start channel
        // Shared channel directory: every channel this client has created,
        // switched to, or learned from a peer. Announced on DataChannel open and
        // whenever it grows, so a created channel reaches everyone — even peers
        // who join later or never tune to it.
        let mut known_channels: Vec<String> = vec![DEFAULT_CHANNEL.to_string()];
        // Tombstones (canonical form): channels deleted from the directory. A
        // directory broadcast can't resurrect a tombstoned channel; switching to
        // a channel clears its tombstone (recreation).
        let mut removed_channels: HashSet<String> = HashSet::new();
        // Who mints the group-audio key, at which generation, and who is told
        // what. The rules live in the coordinator so they are testable; the loop
        // only performs the actions it hands back.
        let mut keys = RoomKeyCoordinator::new(me_id.clone(), room.clone(), promote_tx.clone());
        sink(UiEvent::RoomAudio { gen: None, authority: false }); // negotiating until a key lands
        let mut key_gen: u32 = 1; // generation #1 = the initial DTLS-SRTP keys
        let mut cur_in = Some(incoming);
        let mut cur_out = Some(out);
        let mut net_iv = tokio::time::interval(Duration::from_secs(2));
        let mut last_bytes = (0u64, 0u64);
        let mut last_inst = std::time::Instant::now();
        let mut net_primed = false;
        // Auto-reconnect of the signaling link: when down, retry with backoff.
        let mut next_try: Option<tokio::time::Instant> = None;
        let mut backoff = 2u64;
        // Group-audio watchdog: since when do we have a working PQC link to some
        // peer but still no room key? The key only ever travels over the pairwise
        // session with the AUTHORITY, so if that one peer is the one we can't
        // reach (STUN-only + a hostile NAT), every other link comes up fine and we
        // stay deaf to sealed audio while our own raw frames are still heard.
        // Inherent to encode-once fan-out — one sealed payload for all peers means
        // we can't be served plaintext selectively — so surface it instead.
        let mut keyless_since: Option<std::time::Instant> = None;
        let mut keyless_warned = false;
        loop {
            tokio::select! {
                _ = net_iv.tick() => {
                    let (up, down) = mesh.stats_bytes().await;
                    if !net_primed {
                        last_bytes = (up, down);
                        last_inst = std::time::Instant::now();
                        net_primed = true;
                    } else {
                        let dt = last_inst.elapsed().as_secs_f64().max(0.001);
                        let up_kbps = ((up.saturating_sub(last_bytes.0)) as f64 * 8.0 / 1000.0 / dt) as u32;
                        let down_kbps = ((down.saturating_sub(last_bytes.1)) as f64 * 8.0 / 1000.0 / dt) as u32;
                        last_bytes = (up, down);
                        last_inst = std::time::Instant::now();
                        sink(UiEvent::Net { peers: members.len(), up_kbps, down_kbps });
                    }
                    // Group-audio watchdog (see `keyless_since`): a secure link but
                    // no room key means the authority is unreachable, not that the
                    // handshake is still running. Warn once per stuck episode.
                    if room.generation().is_none() && members.values().any(|m| m.secure) {
                        let since = *keyless_since.get_or_insert_with(std::time::Instant::now);
                        if !keyless_warned && since.elapsed() >= KEYLESS_WARN_AFTER {
                            keyless_warned = true;
                            sink(UiEvent::Log { text:
                                "Sprach-Schluessel nicht erhalten - keine direkte Verbindung zum Schluessel-Verwalter. Du wirst gehoert, hoerst die anderen aber nicht. Abhilfe: \"Session neu verschluesseln\" druecken - das baut jede Verbindung neu auf und verteilt den Schluessel erneut. Hilft das nicht, sollte der Teilnehmer mit dem Stern in seiner Verschluesselungs-Zeile die Session neu betreten.".into() });
                        }
                    } else {
                        keyless_since = None;
                        keyless_warned = false;
                    }
                    // Link watchdog: a member whose peer connection never came up
                    // (or died) is silently mute — no audio, no chat, no channel
                    // announces reach them, and before this nothing retried and
                    // nothing said so. Rebuild on a stagger (offerer side first,
                    // see LINK_RETRY_*) and warn once per dead episode.
                    let now = std::time::Instant::now();
                    let mut retry: Vec<String> = Vec::new();
                    let mut warn: Vec<String> = Vec::new();
                    for (id, m) in members.iter_mut() {
                        if m.linked {
                            continue;
                        }
                        let since = *m.down_since.get_or_insert(now);
                        if relink_due(
                            me_id.as_str(),
                            id.as_str(),
                            now.duration_since(since),
                            now.duration_since(m.last_relink.unwrap_or(since)),
                        ) {
                            m.last_relink = Some(now);
                            retry.push(id.clone());
                        }
                        if !m.link_warned && now.duration_since(since) >= LINK_WARN_AFTER {
                            m.link_warned = true;
                            warn.push(m.name.clone());
                        }
                    }
                    for id in &retry {
                        let _ = mesh.relink(id).await;
                    }
                    for name in warn {
                        sink(UiEvent::Log { text: format!(
                            "Keine Verbindung zu {name} - ihr hoert euch gegenseitig nicht und seht die Kanaele des anderen nicht. Neuaufbau laeuft automatisch.") });
                    }
                }
                // Forward mesh-originated signaling to the current connection.
                Some(up) = up_rx.recv() => {
                    if let Some(o) = &cur_out { let _ = o.send(up); }
                }
                // Auto-reconnect attempt (only while signaling is down).
                _ = async {
                    match next_try {
                        Some(t) => tokio::time::sleep_until(t).await,
                        None => std::future::pending().await,
                    }
                }, if cur_in.is_none() => {
                    match signaling::connect(&rc_server, rc_cert.as_deref()).await {
                        Ok(sig2) => {
                            let o2 = sig2.out.clone();
                            let _ = o2.send(ClientMsg::Join {
                                room: rc_room.clone(),
                                user_id: rc_user.clone(),
                                name: rc_name.clone(),
                                token: rc_token.clone(),
                            });
                            cur_out = Some(o2);
                            cur_in = Some(sig2.incoming);
                            next_try = None;
                            backoff = 2;
                            sink(UiEvent::Signaling { up: true });
                            sink(UiEvent::Log { text: "Signaling wiederverbunden.".into() });
                        }
                        Err(e) => {
                            backoff = (backoff * 2).min(30);
                            next_try = Some(tokio::time::Instant::now() + Duration::from_secs(backoff));
                            sink(UiEvent::Log { text: format!("Reconnect fehlgeschlagen ({e}) - neuer Versuch in {backoff}s.") });
                        }
                    }
                }
                msg = async {
                    match cur_in.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    let Some(msg) = msg else {
                        // Signaling dropped — P2P mesh keeps running. Start auto-
                        // reconnecting (backoff); the UI shows the lost/retry state.
                        cur_in = None;
                        cur_out = None;
                        backoff = 2;
                        next_try = Some(tokio::time::Instant::now() + Duration::from_secs(backoff));
                        sink(UiEvent::Signaling { up: false });
                        sink(UiEvent::Log { text: "Signaling verloren - automatischer Reconnect laeuft.".into() });
                        continue;
                    };
                    match msg {
                        ServerMsg::Roster { peers } => {
                            // Seed each member's channel from any announce already
                            // received (DC may outlive a signaling reconnect).
                            // Keep the live link state of members we already
                            // know — a signaling reconnect must not make every
                            // healthy peer look disconnected.
                            let mut fresh: HashMap<String, Member> = HashMap::new();
                            for p in &peers {
                                let m = match members.remove(&p.user_id) {
                                    Some(mut old) => {
                                        old.name = p.name.clone();
                                        old.secure = crypto.is_secure(&p.user_id);
                                        old
                                    }
                                    None => Member::new(
                                        p.name.clone(),
                                        chan.peer(&p.user_id).unwrap_or_else(|| DEFAULT_CHANNEL.to_string()),
                                        crypto.is_secure(&p.user_id),
                                    ),
                                };
                                fresh.insert(p.user_id.clone(), m);
                            }
                            members = fresh;
                            for p in &peers { let _ = mesh.on_peer(&p.user_id).await; }
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                        }
                        ServerMsg::PeerJoined { user_id, name } => {
                            members.insert(user_id.clone(), Member::new(
                                name,
                                chan.peer(&user_id).unwrap_or_else(|| DEFAULT_CHANNEL.to_string()),
                                crypto.is_secure(&user_id),
                            ));
                            let _ = mesh.on_peer(&user_id).await;
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                            // Future secrecy: if I'm the authority and a room key
                            // already exists, rotate so the joiner only ever holds
                            // a fresh epoch — never the key that protected pre-join
                            // audio. (Skipped before the first key exists, so room
                            // formation mints exactly one key via the Secure path.)
                            if am_authority(&me_id, &members) && keys.generation().is_some() {
                                let acts = keys.rotate(&secure_peer_ids(&members, &crypto));
                                perform_key_actions(&mesh, acts).await;
                                sink(UiEvent::RoomAudio { gen: keys.generation(), authority: true });
                            }
                        }
                        ServerMsg::PeerLeft { user_id } => {
                            members.remove(&user_id);
                            chan.remove_peer(&user_id);
                            mesh.on_left(&user_id).await;
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                            // Forward secrecy: if I'm the authority (which now
                            // includes the case where the previous authority just
                            // left), mint a fresh group-audio key so the departed
                            // member can't decrypt further audio, and hand it to
                            // every remaining secure peer.
                            if am_authority(&me_id, &members) {
                                let acts = keys.rotate(&secure_peer_ids(&members, &crypto));
                                perform_key_actions(&mesh, acts).await;
                                sink(UiEvent::RoomAudio { gen: keys.generation(), authority: true });
                            }
                        }
                        ServerMsg::Offer { from, sdp } => { let _ = mesh.on_offer(&from, sdp).await; }
                        ServerMsg::Answer { from, sdp } => { let _ = mesh.on_answer(&from, sdp).await; }
                        ServerMsg::Ice { from, candidate } => { mesh.on_ice(&from, candidate).await; }
                        ServerMsg::Ptt { user_id, active } => {
                            let was = members.get(&user_id).map(|m| m.speaking).unwrap_or(false);
                            if let Some(m) = members.get_mut(&user_id) { m.speaking = active; }
                            // Onset of an incoming transmission → local "Funk-Klick",
                            // but only for peers on MY channel (off-channel is muted).
                            if active && !was && chan.hears(&user_id) { earcon_loop.click(); }
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                        }
                        ServerMsg::Rekey { by } => {
                            let _ = mesh.rekey().await;
                            key_gen = key_gen.saturating_add(1);
                            sink(UiEvent::Rekeyed { generation: key_gen, by });
                            // "Neu verschlüsseln" rotates the group-audio room key
                            // too (not just DTLS-SRTP) — the authority mints + hands
                            // out a fresh key so voice gets new key material as well.
                            if am_authority(&me_id, &members) {
                                let acts = keys.rotate(&secure_peer_ids(&members, &crypto));
                                perform_key_actions(&mesh, acts).await;
                                sink(UiEvent::RoomAudio { gen: keys.generation(), authority: true });
                            }
                        }
                        ServerMsg::Turn(t) => {
                            if relay_enabled {
                                mesh.add_turn(t.urls, t.username, t.credential);
                            } else {
                                sink(UiEvent::Log { text: "TURN-Relay angeboten, aber deaktiviert (nur direkt/STUN).".into() });
                            }
                        }
                        ServerMsg::Warn { size, cap } => { sink(UiEvent::Log { text: format!("Room {size}/{cap} — Audioqualität kann leiden") }); }
                        ServerMsg::RoomFull { cap } => { sink(UiEvent::Log { text: format!("Room voll @ {cap}") }); break; }
                        ServerMsg::Error { code, message } => { sink(UiEvent::Log { text: format!("{code}: {message}") }); }
                    }
                }
                ev = mesh_rx.recv() => {
                    match ev {
                        Some(MeshEvent::Chat { from, text }) => {
                            // `from` is the peer's user_id → show their display name.
                            let name = members.get(&from).map(|m| m.name.clone()).unwrap_or(from);
                            sink(UiEvent::Chat { from: name, text });
                        }
                        Some(MeshEvent::Badge { peer, badge }) => {
                            if let Some(m) = members.get_mut(&peer) { m.badge = Some(badge); }
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                        }
                        Some(MeshEvent::Link { peer, up }) => {
                            if let Some(m) = members.get_mut(&peer) {
                                if up {
                                    m.linked = true;
                                    m.down_since = None;
                                    m.last_relink = None;
                                    if m.link_warned {
                                        m.link_warned = false;
                                        sink(UiEvent::Log { text: format!("Verbindung zu {} steht wieder.", m.name) });
                                    }
                                } else if m.linked || m.down_since.is_none() {
                                    // Fresh drop: forget the stale badge/session
                                    // state so the UI stops implying a live link.
                                    m.linked = false;
                                    m.badge = None;
                                    m.secure = false;
                                    m.down_since = Some(std::time::Instant::now());
                                }
                                emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                            }
                        }
                        Some(MeshEvent::PeerChannel { peer, name }) => {
                            // A peer announced its channel; reflect it in the roster.
                            // (ChanState was already updated in the mesh RX path.)
                            if let Some(m) = members.get_mut(&peer) { m.channel = name; }
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                        }
                        Some(MeshEvent::PeerChannels { names }) => {
                            // A peer shared its directory. Union it in — but drop
                            // any name we've tombstoned, so a stale directory can't
                            // resurrect a deleted channel. If it grew, persist +
                            // re-broadcast (transitive) and tell the UI.
                            let fresh: Vec<String> = names
                                .into_iter()
                                .filter(|n| !removed_channels.contains(&canon_channel(n)))
                                .collect();
                            if merge_channels(&mut known_channels, &fresh) {
                                chan.set_dir(known_channels.clone());
                                mesh.broadcast_channels(&known_channels).await;
                                sink(UiEvent::Channels { names: known_channels.clone() });
                            }
                        }
                        Some(MeshEvent::PeerChannelRemoved { name }) => {
                            // A peer deleted a channel. Honor it only if nobody here
                            // is tuned to it (else it's still in use). Tombstone so a
                            // later directory broadcast can't bring it back, drop it
                            // from the directory, re-broadcast on first sight
                            // (transitive), and tell the UI.
                            let canon = canon_channel(&name);
                            if !canon.is_empty()
                                && canon != canon_channel(DEFAULT_CHANNEL)
                                && !channel_in_use(&canon, &my_channel, &members)
                                && removed_channels.insert(canon.clone())
                            {
                                remove_from_dir(&mut known_channels, &canon);
                                chan.set_dir(known_channels.clone());
                                mesh.broadcast_channel_removed(&canon).await;
                                sink(UiEvent::ChannelRemoved { name: canon });
                            }
                        }
                        Some(MeshEvent::Secure { peer }) => {
                            // The PQC session with this peer came up → show the lock.
                            if let Some(m) = members.get_mut(&peer) { m.secure = true; }
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                            // The authority hands over the key; everyone else reports
                            // the live generation to the authority. Both directions
                            // are decided in the coordinator.
                            let acts = keys.on_secure(&peer, &members);
                            let authority = am_authority(&me_id, &members);
                            perform_key_actions(&mesh, acts).await;
                            if authority {
                                sink(UiEvent::RoomAudio { gen: keys.generation(), authority: true });
                            }
                        }
                        Some(MeshEvent::RoomKey { from, gen, key }) => {
                            // `from` is the DTLS+PQC-authenticated sender, never a
                            // value out of the message body — the coordinator checks
                            // it really is the elected authority before adopting.
                            if keys.on_room_key(&from, gen, key, &members) {
                                sink(UiEvent::RoomAudio { gen: keys.generation(), authority: am_authority(&me_id, &members) });
                            }
                        }
                        Some(MeshEvent::RoomGen { from, gen }) => {
                            // A member told us how far the room already is. Only the
                            // authority acts on it — see `on_room_gen`.
                            let acts = keys.on_room_gen(&from, gen, &members, &secure_peer_ids(&members, &crypto));
                            if !acts.is_empty() {
                                perform_key_actions(&mesh, acts).await;
                                sink(UiEvent::RoomAudio { gen: keys.generation(), authority: true });
                            }
                        }
                        None => {}
                    }
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(Cmd::ToggleTx) => {
                            let n = !transmit.load(Ordering::SeqCst);
                            transmit.store(n, Ordering::SeqCst);
                            // Local self-feedback: click on our own transmit-onset too
                            // (the server doesn't echo our PTT back to us).
                            if n { earcon_loop.click(); }
                            if let Some(o) = &cur_out { let _ = o.send(ClientMsg::Ptt { active: n }); }
                            sink(UiEvent::Status { connected: true, transmitting: n });
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, n);
                        }
                        Some(Cmd::SetTx(on)) => {
                            if transmit.load(Ordering::SeqCst) != on {
                                transmit.store(on, Ordering::SeqCst);
                                if on { earcon_loop.click(); }
                                if let Some(o) = &cur_out { let _ = o.send(ClientMsg::Ptt { active: on }); }
                                sink(UiEvent::Status { connected: true, transmitting: on });
                                emit_roster(&sink, &members, &me_id, &me_name, &my_channel, on);
                            }
                        }
                        Some(Cmd::Chat(t)) => {
                            mesh.broadcast_chat(&t).await;
                            sink(UiEvent::Chat { from: me_name.clone(), text: t });
                        }
                        Some(Cmd::Rekey) => {
                            // Broadcast → everyone (incl. us) rekeys on the echoed Rekey.
                            if let Some(o) = &cur_out { let _ = o.send(ClientMsg::Rekey); }
                        }
                        Some(Cmd::Reconnect) => {
                            // Manual "resume now": trigger an immediate attempt.
                            if cur_in.is_none() {
                                backoff = 2;
                                next_try = Some(tokio::time::Instant::now());
                            }
                        }
                        Some(Cmd::SetChannel(name)) => {
                            // Switch frequency: store raw display name, update the
                            // shared gate state, and announce to all peers. From now
                            // on I only hear peers on `name`.
                            let name: String = name.trim().chars().take(MAX_CHANNEL_LEN).collect();
                            let name = if name.is_empty() { DEFAULT_CHANNEL.to_string() } else { name };
                            my_channel = name.clone();
                            chan.set_mine(name.clone());
                            // Switching to a channel recreates it → clear any
                            // tombstone so it can re-enter the shared directory.
                            removed_channels.remove(&canon_channel(&name));
                            mesh.broadcast_channel(&name).await;
                            // Creating a channel means switching to it → fold it
                            // into the shared directory and push the grown set.
                            if merge_channels(&mut known_channels, std::slice::from_ref(&name)) {
                                chan.set_dir(known_channels.clone());
                                mesh.broadcast_channels(&known_channels).await;
                            }
                            sink(UiEvent::Channel { mine: name });
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                        }
                        Some(Cmd::RemoveChannel(name)) => {
                            // Delete an empty channel from the shared directory and
                            // tell every peer to drop + tombstone it. Refuse the base
                            // channel or one anyone is currently tuned to.
                            let canon = canon_channel(&name);
                            if !canon.is_empty()
                                && canon != canon_channel(DEFAULT_CHANNEL)
                                && !channel_in_use(&canon, &my_channel, &members)
                            {
                                removed_channels.insert(canon.clone());
                                remove_from_dir(&mut known_channels, &canon);
                                chan.set_dir(known_channels.clone());
                                mesh.broadcast_channel_removed(&canon).await;
                                sink(UiEvent::ChannelRemoved { name: canon });
                            }
                        }
                        None => {
                            sink(UiEvent::Status { connected: false, transmitting: false });
                            break;
                        }
                    }
                }
                g = promote_rx.recv() => {
                    // A group-audio key's grace period elapsed → activate it for
                    // sealing (peers have had time to stage it, so no gap).
                    if let Some(g) = g {
                        room.promote(g);
                        sink(UiEvent::RoomAudio { gen: room.sending_generation(), authority: am_authority(&me_id, &members) });
                    }
                }
            }
        }
    });

    Ok(Engine { cmd_tx, gains, dsp: dsp_cfg, radio: radio_cfg, monitor, stop, bitrate, dtx, dev_tx, earcon })
}

#[cfg(test)]
mod link_tests {
    use super::*;

    #[test]
    fn offerer_side_retries_first() {
        // "a" < "b": we own the offer, so we rebuild after the short delay.
        assert!(relink_due("a", "b", LINK_RETRY_OFFERER, LINK_RETRY_OFFERER));
        // The answerer side must still be waiting at that point.
        assert!(!relink_due("b", "a", LINK_RETRY_OFFERER, LINK_RETRY_OFFERER));
    }

    #[test]
    fn answerer_steps_in_eventually() {
        // A pair whose offerer is the unreachable one must still recover.
        assert!(relink_due("b", "a", LINK_RETRY_ANSWERER, LINK_RETRY_ANSWERER));
    }

    #[test]
    fn no_retry_before_the_delay() {
        let short = Duration::from_secs(1);
        assert!(!relink_due("a", "b", short, short));
        assert!(!relink_due("b", "a", short, short));
    }

    #[test]
    fn a_recent_attempt_blocks_the_next_one() {
        // Down for ages, but we just rebuilt it — don't hammer the peer.
        assert!(!relink_due("a", "b", Duration::from_secs(600), Duration::from_secs(1)));
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;

    fn member() -> Member {
        Member::new("n".into(), DEFAULT_CHANNEL.into(), false)
    }

    #[test]
    fn authority_is_the_smallest_id() {
        // `members` holds the OTHER peers (self is never in the map).
        let mut others: HashMap<String, Member> = HashMap::new();
        assert!(am_authority("bob", &others)); // solo → authority

        others.insert("carol".into(), member());
        assert!(am_authority("bob", &others)); // bob < carol → still authority

        others.insert("alice".into(), member());
        assert!(!am_authority("bob", &others)); // alice smaller & present → not authority

        // From alice's own view, everyone else is larger → she is authority.
        let alices: HashMap<String, Member> =
            ["bob", "carol"].into_iter().map(|id| (id.to_string(), member())).collect();
        assert!(am_authority("alice", &alices));
    }

    #[test]
    fn only_the_min_id_peer_is_accepted_as_room_key_authority() {
        // Room from bob's view = {bob(me), carol, dave}. Authority = bob himself,
        // so no peer should be accepted as authority.
        let others: HashMap<String, Member> =
            ["carol", "dave"].into_iter().map(|id| (id.to_string(), member())).collect();
        assert!(!is_authority_peer("carol", "bob", &others)); // not the smallest
        assert!(!is_authority_peer("zoe", "bob", &others)); // larger than me → never

        // Now alice (smaller than bob) is present → she is the authority.
        let with_alice: HashMap<String, Member> =
            ["alice", "carol", "dave"].into_iter().map(|id| (id.to_string(), member())).collect();
        assert!(is_authority_peer("alice", "bob", &with_alice)); // accepted
        assert!(!is_authority_peer("carol", "bob", &with_alice)); // alice is smaller → carol rejected
    }

    /// Regression: a peer that joins LATER can be the new room-key authority,
    /// because user ids are random and the authority is just the smallest one.
    /// Minting from an empty state then produces generation 1, which every
    /// existing member rejects as older than what it has staged — the newcomer
    /// seals under a key nobody holds and can't open anyone else's, so it is
    /// silent in both directions while the rest of the room is fine. The fix is
    /// `CtrlMsg::RoomGen`: members report the live generation, the new authority
    /// mints above it.
    #[test]
    fn late_joining_authority_converges_on_a_higher_generation() {
        // Room has been running under authority "b", now at generation 2.
        let held = crypto::RoomAudio::new();
        held.install(crypto::RoomKey::generate(2, "b".into()));

        // "a" joins with the smallest id → authority flips to the newcomer.
        let a_view: HashMap<String, Member> =
            ["b", "c"].into_iter().map(|id| (id.to_string(), member())).collect();
        let b_view: HashMap<String, Member> =
            ["a", "c"].into_iter().map(|id| (id.to_string(), member())).collect();
        assert!(am_authority("a", &a_view));
        assert!(!am_authority("b", &b_view));
        // ...and "b" accepts "a" as authority, so it reports its generation there.
        assert!(is_authority_peer("a", "b", &b_view));

        // The bug: generation 1 from the fresh authority is a no-op everywhere.
        assert!(!held.stage(crypto::RoomKey::generate(1, "a".into())));
        assert_eq!(held.generation(), Some(2));

        // The fix: `RoomGen { gen: 2 }` lifts the newcomer's counter, so it mints 3.
        let mut new_auth_gen = 0u32;
        new_auth_gen = new_auth_gen.max(2); // what the RoomGen arm does
        let fresh = crypto::RoomKey::generate(new_auth_gen + 1, "a".into());
        assert!(held.stage(fresh.clone()));
        assert_eq!(held.generation(), Some(3));

        // Both sides are now on one generation → audio opens again.
        let newcomer = crypto::RoomAudio::new();
        newcomer.install(fresh);
        let wire = newcomer.seal_outbound("a", b"opus");
        assert_eq!(held.open_inbound("a", &wire), Some(b"opus".to_vec()));
    }

    fn member_on(ch: &str) -> Member {
        Member::new("n".into(), ch.into(), false)
    }

    #[test]
    fn channel_delete_helpers() {
        let mut dir = vec!["Funk 1".to_string(), "Bravo".to_string()];
        assert!(remove_from_dir(&mut dir, "bravo")); // present → dropped (canon match)
        assert!(!dir.iter().any(|d| canon_channel(d) == "bravo"));
        assert!(!remove_from_dir(&mut dir, "bravo")); // already gone → false

        let mut members = HashMap::new();
        members.insert("x".to_string(), member_on("Bravo"));
        assert!(channel_in_use("bravo", "Funk 1", &members)); // a member is on it
        assert!(channel_in_use("funk 1", "Funk 1", &members)); // I'm on it
        assert!(!channel_in_use("charlie", "Funk 1", &members)); // nobody
    }
}

/// Three-node room-key simulation — the automated stand-in for "have three people
/// join a session and compare the generation in the encryption footer".
///
/// Only the key rules take part: no PeerConnections, no audio devices, no
/// signaling. That is deliberate. The bug these guard against was never in the
/// transport — the sealed `RoomGen`/`RoomKey` wire path has its own end-to-end
/// test in `mesh::rekey_tests` — it was in who mints at which generation, which
/// is exactly what `RoomKeyCoordinator` owns.
#[cfg(test)]
mod room_key_tests {
    use super::*;

    /// One simulated client: its roster view plus the coordinator under test.
    struct Node {
        id: String,
        members: HashMap<String, Member>,
        keys: RoomKeyCoordinator,
        room: Arc<crypto::RoomAudio>,
        promote_rx: mpsc::UnboundedReceiver<u32>,
    }

    impl Node {
        fn new(id: &str) -> Node {
            let room = Arc::new(crypto::RoomAudio::new());
            let (tx, rx) = mpsc::unbounded_channel();
            Node {
                id: id.to_string(),
                members: HashMap::new(),
                keys: RoomKeyCoordinator::new(id.to_string(), room.clone(), tx),
                room,
                promote_rx: rx,
            }
        }

        /// Set this node's roster (everyone but itself), all pairwise sessions up.
        fn sees(&mut self, ids: &[&str]) {
            self.members = ids
                .iter()
                .filter(|i| **i != self.id)
                .map(|i| {
                    (i.to_string(), Member::new(i.to_string(), DEFAULT_CHANNEL.to_string(), true))
                })
                .collect();
        }

        fn secure_peers(&self) -> Vec<String> {
            self.members.keys().cloned().collect()
        }

        /// Service the rekey grace the engine loop would service, so the sealing
        /// key catches up with the staged one.
        fn settle(&mut self) {
            while let Ok(g) = self.promote_rx.try_recv() {
                self.room.promote(g);
            }
        }

        fn generation(&self) -> Option<u32> {
            self.room.generation()
        }
    }

    fn node<'a>(nodes: &'a mut [Node], id: &str) -> &'a mut Node {
        nodes.iter_mut().find(|n| n.id == id).expect("unknown node")
    }

    /// Deliver actions to their targets and keep going until the room is quiet —
    /// a receiver may itself emit actions (a reported generation makes the
    /// authority rotate, which sends the new key back out).
    fn route(nodes: &mut [Node], work: Vec<(String, KeyAction)>) {
        let mut queue = work;
        // Bounded so a rule that ping-pongs forever fails the test instead of
        // hanging it.
        for _ in 0..64 {
            if queue.is_empty() {
                return;
            }
            let mut next = Vec::new();
            for (from, act) in std::mem::take(&mut queue) {
                match act {
                    KeyAction::SendKey { peer, gen, key } => {
                        let t = node(nodes, &peer);
                        let members = t.members.clone();
                        t.keys.on_room_key(&from, gen, key, &members);
                        t.settle();
                    }
                    KeyAction::SendGen { peer, gen } => {
                        let t = node(nodes, &peer);
                        let members = t.members.clone();
                        let secure = t.secure_peers();
                        let acts = t.keys.on_room_gen(&from, gen, &members, &secure);
                        t.settle();
                        next.extend(acts.into_iter().map(|a| (peer.clone(), a)));
                    }
                }
            }
            queue = next;
        }
        panic!("room-key actions never settled — rules are ping-ponging");
    }

    /// Run `on_secure` on `who` for `peer` and route whatever falls out.
    fn secure(nodes: &mut [Node], who: &str, peer: &str) {
        let n = node(nodes, who);
        let members = n.members.clone();
        let acts = n.keys.on_secure(peer, &members);
        n.settle();
        let work = acts.into_iter().map(|a| (who.to_string(), a)).collect();
        route(nodes, work);
    }

    /// Everyone holds the same generation AND the same key bytes, and that key
    /// actually opens audio from every sender. Same check as comparing the
    /// "#Generation" in each client's encryption footer, but stricter.
    ///
    /// Async because a rotation defers the send-key switch by `ROOM_KEY_GRACE`
    /// via a spawned timer. The clock is paused (`start_paused`), so awaiting
    /// past the grace costs no real time — but it has to be awaited, or those
    /// tasks never run and every node would still be sealing with the previous
    /// generation.
    async fn assert_converged(nodes: &mut [Node]) {
        tokio::time::sleep(ROOM_KEY_GRACE + Duration::from_millis(50)).await;
        for n in nodes.iter_mut() {
            n.settle();
        }
        let gens: Vec<Option<u32>> = nodes.iter().map(|n| n.generation()).collect();
        assert!(gens[0].is_some(), "no node ever got a key: {gens:?}");
        assert!(gens.iter().all(|g| *g == gens[0]), "generations diverged: {gens:?}");

        let keys: Vec<[u8; 32]> =
            nodes.iter().map(|n| n.room.current().unwrap().key_bytes()).collect();
        assert!(keys.iter().all(|k| *k == keys[0]), "same generation but different keys");

        // The property users actually feel: every node can open every other
        // node's audio.
        let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        for sender in &ids {
            let wire = node(nodes, sender).room.seal_outbound(sender, b"opus-frame");
            for listener in &ids {
                if listener == sender {
                    continue;
                }
                let got = node(nodes, listener).room.open_inbound(sender, &wire);
                assert_eq!(
                    got.as_deref(),
                    Some(&b"opus-frame"[..]),
                    "{listener} cannot hear {sender}"
                );
            }
        }
    }

    /// Bring "m" and "t" up as a running room and rotate once, so the room sits
    /// above generation 1 when the third client arrives.
    fn established_pair() -> Vec<Node> {
        let mut nodes = vec![Node::new("m"), Node::new("t")];
        node(&mut nodes, "m").sees(&["t"]);
        node(&mut nodes, "t").sees(&["m"]);

        // "m" is the smaller id → authority. Its session with "t" comes up.
        secure(&mut nodes, "m", "t");
        secure(&mut nodes, "t", "m");

        // One rotation, as a join or a manual re-encrypt would trigger.
        let secure_peers = node(&mut nodes, "m").secure_peers();
        let acts = node(&mut nodes, "m").keys.rotate(&secure_peers);
        node(&mut nodes, "m").settle();
        route(&mut nodes, acts.into_iter().map(|a| ("m".to_string(), a)).collect());

        assert_eq!(node(&mut nodes, "m").generation(), Some(2));
        nodes
    }

    /// The reported field failure: a client joins last, happens to draw the
    /// smallest user id, and therefore becomes the key authority from an empty
    /// state. Before the fix it minted generation 1, which the established
    /// members rejected as stale — leaving it sealing under a key nobody held and
    /// unable to open theirs, silent in both directions.
    #[tokio::test(start_paused = true)]
    async fn late_authority_converges_across_three_nodes() {
        let mut nodes = established_pair();
        nodes.push(Node::new("a")); // "a" < "m" < "t" → the newcomer is authority

        node(&mut nodes, "m").sees(&["t", "a"]);
        node(&mut nodes, "t").sees(&["m", "a"]);
        node(&mut nodes, "a").sees(&["m", "t"]);

        // Worst ordering: the newcomer's sessions come up first, so it mints from
        // an empty state before anyone has told it where the room stands.
        secure(&mut nodes, "a", "m");
        secure(&mut nodes, "a", "t");
        assert_eq!(node(&mut nodes, "a").generation(), Some(1), "newcomer mints from scratch");

        // Now the established members see the same sessions and report the live
        // generation to the peer they accept as authority.
        secure(&mut nodes, "m", "a");
        secure(&mut nodes, "t", "a");

        assert_converged(&mut nodes).await;
        let gen = node(&mut nodes, "a").generation().unwrap();
        assert!(gen > 2, "authority must mint above the room's generation, got {gen}");
    }

    /// Same room, but the generation report never arrives — what every build
    /// before 0.2.1 did. Pins the failure so the test above cannot silently pass
    /// for the wrong reason.
    #[tokio::test(start_paused = true)]
    async fn without_the_generation_report_the_newcomer_is_cut_off() {
        let mut nodes = established_pair();
        nodes.push(Node::new("a"));
        node(&mut nodes, "m").sees(&["t", "a"]);
        node(&mut nodes, "t").sees(&["m", "a"]);
        node(&mut nodes, "a").sees(&["m", "t"]);

        // The newcomer mints generation 1 and hands it out; the others drop it as
        // older than what they hold. Nothing reports the room's generation back.
        let members = node(&mut nodes, "a").members.clone();
        let acts = node(&mut nodes, "a").keys.on_secure("m", &members);
        node(&mut nodes, "a").settle();
        for (peer, gen, key) in acts.iter().filter_map(|a| match a {
            KeyAction::SendKey { peer, gen, key } => Some((peer.clone(), *gen, *key)),
            _ => None,
        }) {
            let t = node(&mut nodes, &peer);
            let m = t.members.clone();
            assert!(!t.keys.on_room_key("a", gen, key, &m), "stale key must be rejected");
        }

        assert_eq!(node(&mut nodes, "a").generation(), Some(1));
        assert_eq!(node(&mut nodes, "m").generation(), Some(2));

        // And that is exactly what silence looks like on the wire.
        let wire = node(&mut nodes, "a").room.seal_outbound("a", b"opus-frame");
        assert!(node(&mut nodes, "m").room.open_inbound("a", &wire).is_none(), "m should not hear a");
        let wire = node(&mut nodes, "m").room.seal_outbound("m", b"opus-frame");
        assert!(node(&mut nodes, "a").room.open_inbound("m", &wire).is_none(), "a should not hear m");
    }

    /// The authority leaving is the other way the role moves. The survivor with
    /// the smallest id takes over and must mint ABOVE the departed authority's
    /// generation, or the remaining members would reject its key just the same.
    #[tokio::test(start_paused = true)]
    async fn authority_leaving_hands_over_without_a_gap() {
        let mut nodes = established_pair();
        nodes.push(Node::new("z"));
        node(&mut nodes, "m").sees(&["t", "z"]);
        node(&mut nodes, "t").sees(&["m", "z"]);
        node(&mut nodes, "z").sees(&["m", "t"]);
        secure(&mut nodes, "m", "z"); // authority "m" hands the key to the joiner
        secure(&mut nodes, "z", "m");
        assert_converged(&mut nodes).await;

        // "m" leaves → "t" is now the smallest id and rotates.
        nodes.retain(|n| n.id != "m");
        node(&mut nodes, "t").sees(&["z"]);
        node(&mut nodes, "z").sees(&["t"]);
        let secure_peers = node(&mut nodes, "t").secure_peers();
        let acts = node(&mut nodes, "t").keys.rotate(&secure_peers);
        node(&mut nodes, "t").settle();
        route(&mut nodes, acts.into_iter().map(|a| ("t".to_string(), a)).collect());

        assert_converged(&mut nodes).await;
    }
}
