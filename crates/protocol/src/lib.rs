//! Signaling wire format for RDOC SquadLink Lite.
//!
//! JSON over WebSocket. Tag field is `t`; variants are kebab-case
//! (`peer-joined`, `room-full`). This crate is the single source of truth —
//! the doc examples in ARCHITECTURE.md are illustrative; these types win.
//!
//! The InitConnection server is a dumb relay: it routes `offer`/`answer`/`ice`
//! by `to`, keeps the roster, enforces auth + cap, and mints TURN creds. It
//! never sees media. Glare (who offers) is decided client-side from user ids.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub user_id: String,
    pub name: String,
}

/// Text-chat payload sent over the per-peer WebRTC DataChannel (NOT through
/// the signaling server). Sender identity = the peer owning the channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMsg {
    pub text: String,
    pub ts: u64,
}

/// Control envelope carried over the per-peer WebRTC DataChannel (mesh mode).
/// The server never sees this — named channels (frequencies) are a pure
/// client-side overlay. Receivers parse `CtrlMsg` first and fall back to a bare
/// `ChatMsg` for one version's backward-compat read.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum CtrlMsg {
    /// A text-chat line (same payload as the legacy bare `ChatMsg`).
    Chat(ChatMsg),
    /// Sender announces its current channel (frequency) name. Matching is
    /// case-insensitive (trim + lowercase); peers only mix audio from senders
    /// whose announced channel equals their own.
    Channel { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnCreds {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub ttl: u32,
}

/// Client → Server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum ClientMsg {
    /// First message on a connection. `token` is the room-auth token
    /// (required when the server runs with ROOM_AUTH_SECRET).
    Join {
        room: String,
        user_id: String,
        name: String,
        #[serde(default)]
        token: Option<String>,
    },
    Offer { to: String, sdp: String },
    Answer { to: String, sdp: String },
    Ice { to: String, candidate: String },
    /// Speaking-state for the roster (optional UX).
    Ptt { active: bool },
    /// Request a room-wide key rotation: every peer tears down + re-handshakes
    /// its connections, yielding fresh DTLS-SRTP keys. Server broadcasts it.
    Rekey,
    Leave,
}

/// Server → Client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum ServerMsg {
    /// Existing peers in the room (sent to the joiner; self excluded).
    Roster { peers: Vec<PeerInfo> },
    /// Ephemeral TURN credentials for this session.
    Turn(TurnCreds),
    PeerJoined { user_id: String, name: String },
    PeerLeft { user_id: String },
    Offer { from: String, sdp: String },
    Answer { from: String, sdp: String },
    Ice { from: String, candidate: String },
    Ptt { user_id: String, active: bool },
    /// A peer requested a key rotation; all clients re-handshake. `by` = name.
    Rekey { by: String },
    /// Join refused — room at hard cap.
    RoomFull { cap: usize },
    /// Soft cap reached: client should show a quality-warning banner.
    Warn { size: usize, cap: usize },
    Error { code: String, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_msg_round_trips() {
        let chan = CtrlMsg::Channel { name: "Alpha".into() };
        let j = serde_json::to_string(&chan).unwrap();
        assert!(j.contains("\"t\":\"channel\""));
        match serde_json::from_str::<CtrlMsg>(&j).unwrap() {
            CtrlMsg::Channel { name } => assert_eq!(name, "Alpha"),
            _ => panic!("wrong variant"),
        }

        let chat = CtrlMsg::Chat(ChatMsg { text: "hi".into(), ts: 7 });
        let j = serde_json::to_string(&chat).unwrap();
        match serde_json::from_str::<CtrlMsg>(&j).unwrap() {
            CtrlMsg::Chat(c) => {
                assert_eq!(c.text, "hi");
                assert_eq!(c.ts, 7);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn legacy_bare_chatmsg_still_parses() {
        // A pre-CtrlMsg client sends a bare ChatMsg; the fallback path must read it.
        let legacy = serde_json::to_string(&ChatMsg { text: "old".into(), ts: 1 }).unwrap();
        assert!(serde_json::from_str::<CtrlMsg>(&legacy).is_err());
        let c: ChatMsg = serde_json::from_str(&legacy).unwrap();
        assert_eq!(c.text, "old");
    }
}
