//! companion-core engine: runs the encrypted P2P mesh (audio + chat) and
//! reports state to any frontend via a `Sink` callback. The headless bin and
//! the Tauri app both drive this same engine.

pub mod audio;
pub mod crypto;
pub mod mesh;
pub mod selfcheck;
pub mod serverless;
pub mod signaling;

use std::collections::{HashMap, VecDeque};
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
    /// The post-quantum session with this peer is established.
    Secure { peer: String },
    /// A peer (the room-key authority) sent us the group-audio room key.
    RoomKey { gen: u32, auth: String, key: [u8; 32] },
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

/// Grace between staging a new group-audio key (able to decrypt it) and
/// activating it for sealing. Long enough that every peer has received+staged
/// the key before anyone seals with it, so a rekey drops no audio.
const ROOM_KEY_GRACE: Duration = Duration::from_millis(400);

/// Stage `k` for decryption immediately, then activate it for sealing — at once
/// if it's our first key (nothing to coordinate), else after `ROOM_KEY_GRACE` so
/// every peer has staged it first. Bumps `room_gen` to the accepted generation.
fn adopt_room_key(
    room: &Arc<crypto::RoomAudio>,
    k: crypto::RoomKey,
    room_gen: &mut u32,
    promote_tx: &mpsc::UnboundedSender<u32>,
) {
    let first = !room.has_send();
    if room.stage(k) {
        let g = room.generation().unwrap_or(0);
        *room_gen = (*room_gen).max(g);
        if first {
            room.promote(g); // no prior key to coordinate → seal immediately
        } else {
            let ptx = promote_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(ROOM_KEY_GRACE).await;
                let _ = ptx.send(g);
            });
        }
    }
}

/// Authority action: mint the next-generation group-audio key, adopt it (stage
/// now, activate for sealing after the grace), and hand it to every currently-
/// secure peer (sealed over each pairwise session). Bumps `room_gen`.
async fn rotate_room_key(
    room_gen: &mut u32,
    room: &Arc<crypto::RoomAudio>,
    mesh: &Mesh,
    members: &HashMap<String, Member>,
    crypto: &Arc<crypto::PeerCrypto>,
    me_id: &str,
    promote_tx: &mpsc::UnboundedSender<u32>,
) {
    let g = room_gen.saturating_add(1);
    adopt_room_key(room, crypto::RoomKey::generate(g, me_id.to_string()), room_gen, promote_tx);
    if let Some(k) = room.current() {
        let secure_peers: Vec<String> =
            members.keys().filter(|id| crypto.is_secure(id)).cloned().collect();
        for id in secure_peers {
            mesh.send_room_key(&id, k.generation(), k.authority(), &k.key_bytes()).await;
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
}

/// Handle to the running engine; methods are non-blocking.
pub struct Engine {
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    gains: Arc<audio::Gains>,
    dsp: Arc<Mutex<audio::DspConfig>>,
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
    /// Live capture-path DSP config (noise gate / compressor / limiter).
    pub fn set_dsp(&self, cfg: audio::DspConfig) {
        *self.dsp.lock().unwrap() = cfg;
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
        let (gains, stop) = (gains.clone(), stop.clone());
        std::thread::spawn(move || audio::mixer_loop(mix, play, out_rate, gains, stop));
    }

    Ok((transmit, opus_rx, decode_tx, gains, dsp_cfg, monitor, stop, bitrate, dtx, dev_tx, earcon))
}

struct Member {
    name: String,
    badge: Option<String>,
    speaking: bool,
    channel: String,
    secure: bool,
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

    let (transmit, mut opus_rx, decode_tx, gains, dsp_cfg, monitor, stop, bitrate, dtx, dev_tx, earcon) =
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
        "rdoc-squadlink-lite".to_owned(),
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
    let mut incoming = sig.incoming;
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
        // Highest group-audio key generation this node has minted (as authority)
        // or adopted (from the authority).
        let mut room_gen: u32 = 0;
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
                            members = peers.iter().map(|p| (p.user_id.clone(), Member {
                                name: p.name.clone(),
                                badge: None,
                                speaking: false,
                                channel: chan.peer(&p.user_id).unwrap_or_else(|| DEFAULT_CHANNEL.to_string()),
                                secure: crypto.is_secure(&p.user_id),
                            })).collect();
                            for p in &peers { let _ = mesh.on_peer(&p.user_id).await; }
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                        }
                        ServerMsg::PeerJoined { user_id, name } => {
                            members.insert(user_id.clone(), Member {
                                name,
                                badge: None,
                                speaking: false,
                                channel: chan.peer(&user_id).unwrap_or_else(|| DEFAULT_CHANNEL.to_string()),
                                secure: crypto.is_secure(&user_id),
                            });
                            let _ = mesh.on_peer(&user_id).await;
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                            // Future secrecy: if I'm the authority and a room key
                            // already exists, rotate so the joiner only ever holds
                            // a fresh epoch — never the key that protected pre-join
                            // audio. (Skipped before the first key exists, so room
                            // formation mints exactly one key via the Secure path.)
                            if am_authority(&me_id, &members) && room.generation().is_some() {
                                rotate_room_key(&mut room_gen, &room, &mesh, &members, &crypto, &me_id, &promote_tx).await;
                                sink(UiEvent::RoomAudio { gen: room.generation(), authority: true });
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
                                rotate_room_key(&mut room_gen, &room, &mesh, &members, &crypto, &me_id, &promote_tx).await;
                                sink(UiEvent::RoomAudio { gen: room.generation(), authority: true });
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
                                rotate_room_key(&mut room_gen, &room, &mesh, &members, &crypto, &me_id, &promote_tx).await;
                                sink(UiEvent::RoomAudio { gen: room.generation(), authority: true });
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
                        Some(MeshEvent::PeerChannel { peer, name }) => {
                            // A peer announced its channel; reflect it in the roster.
                            // (ChanState was already updated in the mesh RX path.)
                            if let Some(m) = members.get_mut(&peer) { m.channel = name; }
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                        }
                        Some(MeshEvent::PeerChannels { names }) => {
                            // A peer shared its directory. Union it in; if it grew,
                            // persist + re-broadcast so a channel learned from one
                            // peer reaches peers that peer isn't yet linked to, and
                            // tell the UI to add the new entries to its switcher.
                            if merge_channels(&mut known_channels, &names) {
                                chan.set_dir(known_channels.clone());
                                mesh.broadcast_channels(&known_channels).await;
                                sink(UiEvent::Channels { names: known_channels.clone() });
                            }
                        }
                        Some(MeshEvent::Secure { peer }) => {
                            // The PQC session with this peer came up → show the lock.
                            if let Some(m) = members.get_mut(&peer) { m.secure = true; }
                            emit_roster(&sink, &members, &me_id, &me_name, &my_channel, transmit.load(Ordering::SeqCst));
                            // Room-key authority hands the group-audio key to each
                            // peer as its pairwise PQC session comes up (the key
                            // rides sealed inside that session).
                            if am_authority(&me_id, &members) {
                                if room.generation().is_none() {
                                    room_gen = room_gen.max(1);
                                    adopt_room_key(&room, crypto::RoomKey::generate(room_gen, me_id.clone()), &mut room_gen, &promote_tx);
                                }
                                if let Some(k) = room.current() {
                                    mesh.send_room_key(&peer, k.generation(), k.authority(), &k.key_bytes()).await;
                                }
                                sink(UiEvent::RoomAudio { gen: room.generation(), authority: true });
                            }
                        }
                        Some(MeshEvent::RoomKey { gen, auth, key }) => {
                            // The authority sent the group-audio key. `adopt_room_key`
                            // stages it (if strictly better) and activates it for
                            // sealing after the grace, so no audio is dropped.
                            adopt_room_key(&room, crypto::RoomKey::from_bytes(gen, auth, key), &mut room_gen, &promote_tx);
                            sink(UiEvent::RoomAudio { gen: room.generation(), authority: am_authority(&me_id, &members) });
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

    Ok(Engine { cmd_tx, gains, dsp: dsp_cfg, monitor, stop, bitrate, dtx, dev_tx, earcon })
}

#[cfg(test)]
mod authority_tests {
    use super::*;

    fn member() -> Member {
        Member {
            name: "n".into(),
            badge: None,
            speaking: false,
            channel: DEFAULT_CHANNEL.into(),
            secure: false,
        }
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
}
