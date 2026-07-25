//! Application-layer post-quantum crypto.
//!
//! A hybrid **ML-KEM-768 + X25519** handshake runs over the (already
//! DTLS-authenticated) P2P DataChannel; the derived symmetric keys AEAD-encrypt
//! chat and the group-audio room key **inside** DTLS. This defeats passive
//! "harvest now, decrypt later": an attacker must break *both* ML-KEM and the
//! DTLS transport. It does NOT replace DTLS (which still authenticates the
//! peer); it layers quantum-safe confidentiality on top.
//!
//! Glare: the lexicographically smaller `user_id` is the initiator (side A);
//! the pairwise `Session` keys are per-direction so A→B and B→A never share a
//! key or nonce.
//!
//! Two symmetric layers sit on top of the handshake:
//! - [`Session`] — the per-peer pairwise AEAD used for chat and to ship the
//!   room key confidentially.
//! - [`RoomAudio`] / [`RoomKey`] — a single room-wide key so an audio frame is
//!   sealed ONCE and fanned out to every peer (preserving encode-once). The
//!   authority (smallest `user_id`) mints it and distributes it over the
//!   pairwise sessions, rotating it when membership changes.

use anyhow::{anyhow, Result};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use hkdf::Hkdf;
use ml_kem::kem::{Decapsulate, Encapsulate};
use ml_kem::{EncodedSizeUser, KemCore, MlKem768};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use x25519_dalek::{EphemeralSecret, PublicKey};

type Ek = <MlKem768 as KemCore>::EncapsulationKey;
type Dk = <MlKem768 as KemCore>::DecapsulationKey;

const KDF_LABEL: &[u8] = b"rdoc-squadlink-pqc-v1";

/// Frame kind, folded into the AEAD AAD so a sealed audio frame can't be
/// replayed as chat (and vice-versa).
#[derive(Clone, Copy)]
pub enum Kind {
    Audio,
    Chat,
}
impl Kind {
    fn tag(self) -> u8 {
        match self {
            Kind::Audio => 1,
            Kind::Chat => 2,
        }
    }
}

/// Initiator (A) → responder (B): A's ML-KEM encapsulation key + X25519 pub key.
#[derive(Clone)]
pub struct Hello {
    pub kem_pk: Vec<u8>,
    pub x_pk: [u8; 32],
}
/// Responder (B) → initiator (A): B's X25519 pub key + the ML-KEM ciphertext.
#[derive(Clone)]
pub struct Reply {
    pub x_pk: [u8; 32],
    pub kem_ct: Vec<u8>,
}

fn x_pub_bytes(pk: &PublicKey) -> [u8; 32] {
    *pk.as_bytes()
}

/// HKDF-SHA256 → (key, 4-byte nonce prefix) for one direction, salted by the
/// transcript hash and separated by a per-direction `info` label.
fn kdf_dir(salt: &[u8], ikm: &[u8], info: &[u8]) -> (Key, [u8; 4]) {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut okm = [0u8; 36]; // 32-byte key + 4-byte nonce prefix
    hk.expand(info, &mut okm).expect("hkdf expand");
    let mut key = Key::default();
    key.copy_from_slice(&okm[..32]);
    let mut prefix = [0u8; 4];
    prefix.copy_from_slice(&okm[32..36]);
    (key, prefix)
}

/// Build the shared transcript both sides mix into the KDF salt.
fn transcript(a_id: &str, b_id: &str, h: &Hello, r: &Reply) -> [u8; 32] {
    let mut d = Sha256::new();
    d.update(KDF_LABEL);
    d.update((a_id.len() as u32).to_be_bytes());
    d.update(a_id.as_bytes());
    d.update((b_id.len() as u32).to_be_bytes());
    d.update(b_id.as_bytes());
    d.update(&h.kem_pk);
    d.update(h.x_pk);
    d.update(r.x_pk);
    d.update(&r.kem_ct);
    d.finalize().into()
}

/// Derive both sessions from the two shared secrets + transcript. Returns the
/// session for whichever side we are (`initiator`).
fn derive(
    initiator: bool,
    ss_x: [u8; 32],
    ss_kem: &[u8],
    a_id: &str,
    b_id: &str,
    h: &Hello,
    r: &Reply,
) -> Session {
    let salt = transcript(a_id, b_id, h, r);
    let mut ikm = Vec::with_capacity(32 + ss_kem.len());
    ikm.extend_from_slice(&ss_x);
    ikm.extend_from_slice(ss_kem);
    let (k_ab, p_ab) = kdf_dir(&salt, &ikm, b"A->B");
    let (k_ba, p_ba) = kdf_dir(&salt, &ikm, b"B->A");
    if initiator {
        Session::new(k_ab, p_ab, k_ba, p_ba)
    } else {
        Session::new(k_ba, p_ba, k_ab, p_ab)
    }
}

/// Initiator state held between `start()` and `finish()`.
pub struct Initiator {
    dk: Dk,
    x_secret: EphemeralSecret,
    a_id: String,
    b_id: String,
    hello: Hello,
}

impl Initiator {
    /// Begin a handshake; returns the `Hello` to send to the peer.
    pub fn start(a_id: &str, b_id: &str) -> (Initiator, Hello) {
        let (dk, ek) = MlKem768::generate(&mut OsRng);
        let x_secret = EphemeralSecret::random_from_rng(OsRng);
        let x_pk = x_pub_bytes(&PublicKey::from(&x_secret));
        let hello = Hello { kem_pk: ek.as_bytes().to_vec(), x_pk };
        (
            Initiator { dk, x_secret, a_id: a_id.into(), b_id: b_id.into(), hello: hello.clone() },
            hello,
        )
    }

    /// Finish with the peer's `Reply` → the encryption session.
    pub fn finish(self, reply: &Reply) -> Result<Session> {
        let ct = kem_ct_from_bytes(&reply.kem_ct)?;
        let ss_kem = self.dk.decapsulate(&ct).map_err(|_| anyhow!("ML-KEM decapsulate failed"))?;
        let their_x = x_pub_from_bytes(reply.x_pk);
        let ss_x = self.x_secret.diffie_hellman(&their_x).to_bytes();
        Ok(derive(true, ss_x, ss_kem.as_slice(), &self.a_id, &self.b_id, &self.hello, reply))
    }
}

/// Responder: consume the initiator's `Hello`, return the `Reply` + session.
pub fn respond(a_id: &str, b_id: &str, hello: &Hello) -> Result<(Reply, Session)> {
    let ek = ek_from_bytes(&hello.kem_pk)?;
    let (kem_ct, ss_kem) = ek.encapsulate(&mut OsRng).map_err(|_| anyhow!("ML-KEM encapsulate failed"))?;
    let x_secret = EphemeralSecret::random_from_rng(OsRng);
    let x_pk = x_pub_bytes(&PublicKey::from(&x_secret));
    let their_x = x_pub_from_bytes(hello.x_pk);
    let ss_x = x_secret.diffie_hellman(&their_x).to_bytes();
    let reply = Reply { x_pk, kem_ct: kem_ct.to_vec() };
    let sess = derive(false, ss_x, ss_kem.as_slice(), a_id, b_id, hello, &reply);
    Ok((reply, sess))
}

fn ek_from_bytes(b: &[u8]) -> Result<Ek> {
    let enc = ml_kem::Encoded::<Ek>::try_from(b).map_err(|_| anyhow!("bad ML-KEM enc key length"))?;
    Ok(Ek::from_bytes(&enc))
}
fn kem_ct_from_bytes(b: &[u8]) -> Result<ml_kem::Ciphertext<MlKem768>> {
    ml_kem::Ciphertext::<MlKem768>::try_from(b).map_err(|_| anyhow!("bad ML-KEM ciphertext length"))
}
fn x_pub_from_bytes(b: [u8; 32]) -> PublicKey {
    PublicKey::from(b)
}

/// A live AEAD session: per-direction ChaCha20-Poly1305 keys + nonce prefixes,
/// a send counter, and a receive anti-replay window.
pub struct Session {
    send: ChaCha20Poly1305,
    recv: ChaCha20Poly1305,
    send_prefix: [u8; 4],
    recv_prefix: [u8; 4],
    send_ctr: u64,
    recv_max: u64,
    recv_window: u64, // bitmap of the 64 counters below recv_max
}

impl Session {
    fn new(send_key: Key, send_prefix: [u8; 4], recv_key: Key, recv_prefix: [u8; 4]) -> Self {
        Session {
            send: ChaCha20Poly1305::new(&send_key),
            recv: ChaCha20Poly1305::new(&recv_key),
            send_prefix,
            recv_prefix,
            send_ctr: 0,
            recv_max: 0,
            recv_window: 0,
        }
    }

    fn nonce(prefix: [u8; 4], ctr: u64) -> Nonce {
        let mut n = [0u8; 12];
        n[..4].copy_from_slice(&prefix);
        n[4..].copy_from_slice(&ctr.to_be_bytes());
        *Nonce::from_slice(&n)
    }

    /// Encrypt one frame. Output = 8-byte counter ‖ ciphertext+tag.
    pub fn seal(&mut self, kind: Kind, plaintext: &[u8]) -> Vec<u8> {
        let ctr = self.send_ctr;
        self.send_ctr = self.send_ctr.wrapping_add(1);
        let nonce = Self::nonce(self.send_prefix, ctr);
        let aad = [kind.tag()];
        let ct = self
            .send
            .encrypt(&nonce, Payload { msg: plaintext, aad: &aad })
            .expect("aead encrypt");
        let mut out = Vec::with_capacity(8 + ct.len());
        out.extend_from_slice(&ctr.to_be_bytes());
        out.extend_from_slice(&ct);
        out
    }

    /// Decrypt one frame. Returns None on auth failure or replay.
    pub fn open(&mut self, kind: Kind, wire: &[u8]) -> Option<Vec<u8>> {
        if wire.len() < 8 {
            return None;
        }
        let mut ctr_bytes = [0u8; 8];
        ctr_bytes.copy_from_slice(&wire[..8]);
        let ctr = u64::from_be_bytes(ctr_bytes);
        if !self.replay_ok(ctr) {
            return None;
        }
        let nonce = Self::nonce(self.recv_prefix, ctr);
        let aad = [kind.tag()];
        let pt = self.recv.decrypt(&nonce, Payload { msg: &wire[8..], aad: &aad }).ok()?;
        self.replay_mark(ctr);
        Some(pt)
    }

    /// True if `ctr` is fresh (not before the window, not already seen).
    fn replay_ok(&self, ctr: u64) -> bool {
        if ctr > self.recv_max {
            return true;
        }
        let diff = self.recv_max - ctr;
        if diff >= 64 {
            return false; // too old
        }
        (self.recv_window >> diff) & 1 == 0
    }
    fn replay_mark(&mut self, ctr: u64) {
        if ctr > self.recv_max {
            let shift = ctr - self.recv_max;
            self.recv_window = if shift >= 64 { 0 } else { self.recv_window << shift };
            self.recv_window |= 1;
            self.recv_max = ctr;
        } else {
            let diff = self.recv_max - ctr;
            if diff < 64 {
                self.recv_window |= 1 << diff;
            }
        }
    }
}

/// Per-peer handshake + session store, shared (behind an `Arc`) between the
/// mesh RX path (open) and the engine/broadcast path (seal). Glare: the
/// lexicographically smaller `user_id` is the initiator.
pub struct PeerCrypto {
    my_id: String,
    sessions: std::sync::Mutex<std::collections::HashMap<String, Session>>,
    pending: std::sync::Mutex<std::collections::HashMap<String, Initiator>>,
}

impl PeerCrypto {
    pub fn new(my_id: String) -> Self {
        PeerCrypto {
            my_id,
            sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            pending: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// True if we open the handshake toward `peer` (we hold the smaller id).
    pub fn is_initiator(&self, peer: &str) -> bool {
        self.my_id.as_str() < peer
    }

    /// Begin (or restart) a handshake as initiator → the `Hello` to send. `None`
    /// when we're the responder (we wait for the peer's `Hello`).
    pub fn begin(&self, peer: &str) -> Option<Hello> {
        if !self.is_initiator(peer) {
            return None;
        }
        let (init, hello) = Initiator::start(&self.my_id, peer);
        self.pending.lock().unwrap().insert(peer.to_string(), init);
        self.sessions.lock().unwrap().remove(peer); // a new handshake supersedes
        Some(hello)
    }

    /// Responder: consume the peer's `Hello` → the `Reply` to send. Establishes
    /// our session. Ignored if we're actually the initiator (id ordering).
    pub fn on_hello(&self, peer: &str, hello: &Hello) -> Option<Reply> {
        if self.is_initiator(peer) {
            return None; // we don't respond to a peer that should be responding to us
        }
        // Transcript uses a_id = initiator (smaller id) = peer, b_id = us.
        match respond(peer, &self.my_id, hello) {
            Ok((reply, sess)) => {
                self.sessions.lock().unwrap().insert(peer.to_string(), sess);
                Some(reply)
            }
            Err(_) => None,
        }
    }

    /// Initiator: consume the peer's `Reply` → establish the session. Returns
    /// true on success.
    pub fn on_reply(&self, peer: &str, reply: &Reply) -> bool {
        let init = self.pending.lock().unwrap().remove(peer);
        if let Some(init) = init {
            if let Ok(sess) = init.finish(reply) {
                self.sessions.lock().unwrap().insert(peer.to_string(), sess);
                return true;
            }
        }
        false
    }

    pub fn is_secure(&self, peer: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(peer)
    }

    pub fn seal(&self, peer: &str, kind: Kind, pt: &[u8]) -> Option<Vec<u8>> {
        self.sessions.lock().unwrap().get_mut(peer).map(|s| s.seal(kind, pt))
    }

    pub fn open(&self, peer: &str, kind: Kind, wire: &[u8]) -> Option<Vec<u8>> {
        self.sessions.lock().unwrap().get_mut(peer).and_then(|s| s.open(kind, wire))
    }

    /// Drop a peer's session + any pending handshake (left / rekey).
    pub fn remove(&self, peer: &str) {
        self.sessions.lock().unwrap().remove(peer);
        self.pending.lock().unwrap().remove(peer);
    }
}

/// Room-wide symmetric key for **group audio**. One key shared by all members,
/// so an audio frame is sealed ONCE by the sender and fanned out to every peer —
/// the encode-once model is preserved (unlike the per-direction pairwise
/// `Session`, which would force a per-peer seal). The key itself is distributed
/// confidentially over the pairwise PQC `Session`s (see `CtrlMsg::RoomKey`), so
/// it inherits the hybrid post-quantum protection.
///
/// Because many senders share the key, each frame carries a fresh **random
/// 96-bit nonce** instead of a per-sender counter — no cross-sender coordination
/// is needed and nonce reuse is negligible (birthday bound ≈ 2^48 frames). The
/// sender's `user_id` is bound into the AAD so a frame can't be re-attributed to
/// another member. There is no anti-replay window: DTLS-SRTP already gives
/// transport-level replay protection and a replayed stale audio frame is
/// harmless (it just plays late), unlike a replayed chat line.
#[derive(Clone)]
pub struct RoomKey {
    gen: u32,
    /// The minting authority's `user_id`. Used only to break ties when two nodes
    /// mint the same generation during a split roster view — the smaller id wins,
    /// so every node deterministically converges on one key.
    auth: String,
    key: [u8; 32],
}

impl RoomKey {
    /// A fresh random room key at generation `gen`, minted by `auth`.
    pub fn generate(gen: u32, auth: String) -> Self {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        RoomKey { gen, auth, key }
    }
    /// Reconstruct a room key received from the authority.
    pub fn from_bytes(gen: u32, auth: String, key: [u8; 32]) -> Self {
        RoomKey { gen, auth, key }
    }
    pub fn generation(&self) -> u32 {
        self.gen
    }
    pub fn authority(&self) -> &str {
        &self.auth
    }
    pub fn key_bytes(&self) -> [u8; 32] {
        self.key
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(Key::from_slice(&self.key))
    }

    /// Seal one audio frame from `sender`. Output = 12-byte nonce ‖ ct+tag.
    pub fn seal_audio(&self, sender: &str, pt: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let aad = room_aad(sender);
        let ct = self
            .cipher()
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: pt, aad: &aad })
            .expect("aead encrypt");
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    /// Open one audio frame claimed to be from `sender`. None on auth failure.
    pub fn open_audio(&self, sender: &str, wire: &[u8]) -> Option<Vec<u8>> {
        if wire.len() < 12 {
            return None;
        }
        let aad = room_aad(sender);
        self.cipher()
            .decrypt(Nonce::from_slice(&wire[..12]), Payload { msg: &wire[12..], aad: &aad })
            .ok()
    }
}

/// AAD for a room-audio frame: the audio kind tag ‖ the sender's id, so a sealed
/// frame is bound to both its kind and its author.
fn room_aad(sender: &str) -> Vec<u8> {
    let mut a = Vec::with_capacity(1 + sender.len());
    a.push(Kind::Audio.tag());
    a.extend_from_slice(sender.as_bytes());
    a
}

/// Audio payload framing tags (first byte of the RTP payload we write).
const AUDIO_RAW: u8 = 0; // plaintext Opus (pre-key fallback so audio isn't lost)
const AUDIO_SEALED: u8 = 1; // AUDIO_SEALED ‖ gen(4) ‖ nonce(12) ‖ ct+tag

/// Live group-audio keying, shared (behind an `Arc`) between the outbound writer
/// (`seal_outbound`), the inbound read-track tasks (`open_inbound`), and the
/// engine loop, which `install`s keys received from the authority. Keeps the
/// current key plus the immediately-previous one so frames still sealed under
/// the old key during a rekey keep decoding through the transition.
pub struct RoomAudio {
    cur: std::sync::Mutex<Option<RoomKey>>,
    prev: std::sync::Mutex<Option<RoomKey>>,
}

impl Default for RoomAudio {
    fn default() -> Self {
        Self::new()
    }
}

impl RoomAudio {
    pub fn new() -> Self {
        RoomAudio { cur: std::sync::Mutex::new(None), prev: std::sync::Mutex::new(None) }
    }

    /// The current key generation, if a key is installed.
    pub fn generation(&self) -> Option<u32> {
        self.cur.lock().unwrap().as_ref().map(|k| k.generation())
    }

    /// A clone of the current key (for the authority to distribute), if any.
    pub fn current(&self) -> Option<RoomKey> {
        self.cur.lock().unwrap().clone()
    }

    /// Install `k` as current, demoting the old current to `prev`. Ignored
    /// unless `k` is strictly better — a newer generation, or the same
    /// generation from a smaller authority id — so duplicate/older/losing-tie
    /// distributions are idempotent.
    pub fn install(&self, k: RoomKey) {
        let mut cur = self.cur.lock().unwrap();
        if let Some(existing) = cur.as_ref() {
            // Newer generation wins; on a tie the smaller authority id wins, so
            // a split roster view converges on one key instead of partitioning.
            let better = k.generation() > existing.generation()
                || (k.generation() == existing.generation() && k.authority() < existing.authority());
            if !better {
                return;
            }
            *self.prev.lock().unwrap() = cur.take();
        }
        *cur = Some(k);
    }

    /// Frame an outbound audio payload: sealed if we hold a key, else raw.
    pub fn seal_outbound(&self, sender: &str, opus: &[u8]) -> Vec<u8> {
        if let Some(k) = self.cur.lock().unwrap().as_ref() {
            let sealed = k.seal_audio(sender, opus);
            let mut out = Vec::with_capacity(5 + sealed.len());
            out.push(AUDIO_SEALED);
            out.extend_from_slice(&k.generation().to_be_bytes());
            out.extend_from_slice(&sealed);
            out
        } else {
            let mut out = Vec::with_capacity(1 + opus.len());
            out.push(AUDIO_RAW);
            out.extend_from_slice(opus);
            out
        }
    }

    /// Parse an inbound audio payload from `sender` → the Opus bytes, or None to
    /// drop (unknown tag, or a sealed frame we can't authenticate). A raw frame
    /// is accepted even when we hold a key: the threat is passive harvest-now,
    /// and an active downgrade would still sit inside the authenticated DTLS
    /// channel, so leniency costs nothing against the modeled adversary.
    pub fn open_inbound(&self, sender: &str, wire: &[u8]) -> Option<Vec<u8>> {
        match wire.split_first() {
            Some((&AUDIO_RAW, opus)) => Some(opus.to_vec()),
            Some((&AUDIO_SEALED, rest)) if rest.len() >= 4 => {
                let gen = u32::from_be_bytes(rest[..4].try_into().ok()?);
                let body = &rest[4..];
                for slot in [&self.cur, &self.prev] {
                    if let Some(k) = slot.lock().unwrap().as_ref() {
                        if k.generation() == gen {
                            if let Some(pt) = k.open_audio(sender, body) {
                                return Some(pt);
                            }
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_key_round_trip() {
        let rk = RoomKey::generate(1, "auth".into());
        let w = rk.seal_audio("alice", b"opus-frame");
        // Same key + same sender → opens.
        let rk2 = RoomKey::from_bytes(rk.generation(), rk.authority().into(), rk.key_bytes());
        assert_eq!(rk2.open_audio("alice", &w).unwrap(), b"opus-frame");
    }

    #[test]
    fn room_key_wrong_sender_rejected() {
        let rk = RoomKey::generate(7, "auth".into());
        let w = rk.seal_audio("alice", b"x");
        assert!(rk.open_audio("bob", &w).is_none()); // AAD sender mismatch
    }

    #[test]
    fn room_key_wrong_key_rejected() {
        let a = RoomKey::generate(1, "auth".into());
        let b = RoomKey::generate(1, "auth".into());
        let w = a.seal_audio("alice", b"x");
        assert!(b.open_audio("alice", &w).is_none());
    }

    #[test]
    fn room_key_nonces_differ_per_frame() {
        let rk = RoomKey::generate(1, "auth".into());
        let w1 = rk.seal_audio("a", b"same");
        let w2 = rk.seal_audio("a", b"same");
        assert_ne!(&w1[..12], &w2[..12]); // random nonce per frame
    }

    #[test]
    fn room_audio_raw_passthrough_without_key() {
        let ra = RoomAudio::new();
        let wire = ra.seal_outbound("a", b"opus"); // no key → raw
        assert_eq!(wire[0], AUDIO_RAW);
        // A receiver (with or without a key) recovers the plaintext Opus.
        assert_eq!(ra.open_inbound("a", &wire).unwrap(), b"opus");
    }

    #[test]
    fn room_audio_sealed_round_trip() {
        let key = RoomKey::generate(1, "auth".into());
        let sender = RoomAudio::new();
        let recv = RoomAudio::new();
        sender.install(key.clone());
        recv.install(key);
        let wire = sender.seal_outbound("alice", b"voice");
        assert_eq!(wire[0], AUDIO_SEALED);
        assert_eq!(recv.open_inbound("alice", &wire).unwrap(), b"voice");
        // A receiver without the key can't open it.
        assert!(RoomAudio::new().open_inbound("alice", &wire).is_none());
    }

    #[test]
    fn room_audio_prev_key_bridges_rekey() {
        let sender = RoomAudio::new();
        let recv = RoomAudio::new();
        sender.install(RoomKey::generate(1, "auth".into()));
        recv.install(sender.current().unwrap()); // both at gen 1
        let old_frame = sender.seal_outbound("a", b"old");
        // Receiver rekeys to gen 2 before the old frame arrives.
        recv.install(RoomKey::generate(2, "auth".into()));
        // gen-1 frame still opens via the retained previous key.
        assert_eq!(recv.open_inbound("a", &old_frame).unwrap(), b"old");
    }

    #[test]
    fn room_audio_unknown_generation_dropped() {
        let recv = RoomAudio::new();
        recv.install(RoomKey::generate(5, "auth".into()));
        let other = RoomAudio::new();
        other.install(RoomKey::generate(9, "auth".into())); // gen the receiver never saw
        let wire = other.seal_outbound("a", b"x");
        assert!(recv.open_inbound("a", &wire).is_none());
    }

    #[test]
    fn room_audio_same_gen_smaller_authority_wins() {
        let ra = RoomAudio::new();
        // Two authorities mint gen 1 during a split roster view.
        let from_c = RoomKey::generate(1, "c".into());
        let from_a = RoomKey::generate(1, "a".into());
        // Install the larger-id one first, then the smaller — smaller id wins.
        ra.install(from_c.clone());
        ra.install(from_a.clone());
        assert_eq!(ra.current().unwrap().key_bytes(), from_a.key_bytes());
        // And the reverse order converges to the same key (idempotent tie-break).
        let rb = RoomAudio::new();
        rb.install(from_a.clone());
        rb.install(from_c);
        assert_eq!(rb.current().unwrap().key_bytes(), from_a.key_bytes());
    }

    #[test]
    fn room_audio_install_ignores_stale_generation() {
        let ra = RoomAudio::new();
        ra.install(RoomKey::generate(3, "auth".into()));
        ra.install(RoomKey::generate(2, "auth".into())); // older → ignored
        assert_eq!(ra.generation(), Some(3));
    }

    #[test]
    fn peer_crypto_handshake_and_seal() {
        let a = PeerCrypto::new("a".into()); // initiator (smaller id)
        let b = PeerCrypto::new("b".into()); // responder
        let hello = a.begin("b").expect("a initiates");
        assert!(b.begin("a").is_none()); // b is responder
        let reply = b.on_hello("a", &hello).expect("b responds");
        assert!(a.on_reply("b", &reply));
        assert!(a.is_secure("b") && b.is_secure("a"));
        let w = a.seal("b", Kind::Chat, b"hi").unwrap();
        assert_eq!(b.open("a", Kind::Chat, &w).unwrap(), b"hi");
    }

    fn pair() -> (Session, Session) {
        // "a" < "b" → a is initiator.
        let (init, hello) = Initiator::start("a", "b");
        let (reply, sess_b) = respond("a", "b", &hello).unwrap();
        let sess_a = init.finish(&reply).unwrap();
        (sess_a, sess_b)
    }

    #[test]
    fn round_trip_both_directions() {
        let (mut a, mut b) = pair();
        let w = a.seal(Kind::Chat, b"hello from A");
        assert_eq!(b.open(Kind::Chat, &w).unwrap(), b"hello from A");
        let w2 = b.seal(Kind::Audio, b"opus-from-B");
        assert_eq!(a.open(Kind::Audio, &w2).unwrap(), b"opus-from-B");
    }

    #[test]
    fn tamper_is_rejected() {
        let (mut a, mut b) = pair();
        let mut w = a.seal(Kind::Chat, b"secret");
        let n = w.len() - 1;
        w[n] ^= 0x01;
        assert!(b.open(Kind::Chat, &w).is_none());
    }

    #[test]
    fn wrong_kind_is_rejected() {
        let (mut a, mut b) = pair();
        let w = a.seal(Kind::Audio, b"x");
        assert!(b.open(Kind::Chat, &w).is_none()); // AAD mismatch
    }

    #[test]
    fn replay_is_rejected() {
        let (mut a, mut b) = pair();
        let w = a.seal(Kind::Chat, b"once");
        assert!(b.open(Kind::Chat, &w).is_some());
        assert!(b.open(Kind::Chat, &w).is_none()); // same counter → replay
    }

    #[test]
    fn out_of_order_within_window_ok() {
        let (mut a, mut b) = pair();
        let w0 = a.seal(Kind::Audio, b"0");
        let w1 = a.seal(Kind::Audio, b"1");
        let w2 = a.seal(Kind::Audio, b"2");
        assert!(b.open(Kind::Audio, &w2).is_some());
        assert!(b.open(Kind::Audio, &w0).is_some()); // reordered, still fresh
        assert!(b.open(Kind::Audio, &w1).is_some());
        assert!(b.open(Kind::Audio, &w1).is_none()); // now a replay
    }
}
