//! Signaling wire format for subraum.
//!
//! JSON over WebSocket. Tag field is `t`; variants are kebab-case
//! (`peer-joined`, `room-full`). This crate is the single source of truth —
//! the doc examples in ARCHITECTURE.md are illustrative; these types win.
//!
//! The InitConnection server is a dumb relay: it routes `offer`/`answer`/`ice`
//! by `to`, keeps the roster, enforces auth + cap, and mints TURN creds. It
//! never sees media. Glare (who offers) is decided client-side from user ids.
//!
//! Compatibility policy: **none across protocol versions.** Every `Join`
//! carries [`PROTOCOL_VERSION`]; the server refuses any other value with the
//! [`ERR_PROTOCOL_MISMATCH`] error and closes the socket. Any change to the
//! wire format (new/removed/retyped field, new variant a peer must understand)
//! bumps the constant — there are no `#[serde(default)]` escape hatches for
//! older builds.

use serde::{Deserialize, Serialize};

/// Wire-format generation. Bump on any incompatible change; the server
/// refuses a `Join` whose `proto` differs.
///
/// - 1: implicit — every build before 0.4.0 (no `proto` field, optional
///   `secret`, bare `ChatMsg` fallback on the DataChannel).
/// - 2: 0.4.0 — `proto` + `secret` mandatory on `Join`; DataChannel frames
///   are `CtrlMsg` only.
pub const PROTOCOL_VERSION: u32 = 2;

/// `ServerMsg::Error.code` for a `Join` from a different protocol generation.
/// Terminal for the client: reconnecting will not help, only an update.
pub const ERR_PROTOCOL_MISMATCH: &str = "protocol-mismatch";

/// `ServerMsg::Error.code` when the `user_id` is held by a connection that
/// presented a different `secret`. Terminal: pick another id.
pub const ERR_ID_IN_USE: &str = "id-in-use";

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
/// client-side overlay. Every DataChannel frame is a `CtrlMsg`; anything that
/// does not parse as one is dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum CtrlMsg {
    /// A text-chat line.
    Chat(ChatMsg),
    /// Sender announces its current channel (frequency) name. Matching is
    /// case-insensitive (trim + lowercase); peers only mix audio from senders
    /// whose announced channel equals their own.
    Channel { name: String },
    /// Sender shares the full set of channel (frequency) names it knows — the
    /// shared directory. Union-merged on receipt so a channel created by any
    /// peer becomes selectable for everyone, even before anyone tunes to it and
    /// even for peers that join after it was created. Sent on DataChannel open
    /// (so late joiners catch up) and whenever the directory grows.
    Channels { names: Vec<String> },
    /// Sender deleted an (empty) channel from the shared directory. Receivers
    /// drop it and tombstone the name so a later directory broadcast can't
    /// resurrect it. Ignored for a channel someone is currently tuned to.
    ChannelRemoved { name: String },
    /// PQC handshake, initiator → responder: hybrid ML-KEM-768 encapsulation key
    /// + X25519 public key (both base64). Sent plaintext (public values).
    KemHello { kem_pk: String, x_pk: String },
    /// PQC handshake, responder → initiator: X25519 public key + ML-KEM
    /// ciphertext (both base64).
    KemReply { x_pk: String, kem_ct: String },
    /// An AEAD-sealed inner `CtrlMsg` (base64) once the PQC session is up —
    /// quantum-safe confidentiality layered inside DTLS.
    Enc { data: String },
    /// Authority → member: the current group-audio room key (32 bytes, base64)
    /// at generation `gen`. Only ever sent SEALED (wrapped in `Enc` over the
    /// pairwise PQC session), so the room key never leaves the post-quantum
    /// channel AND the sender is a DTLS+PQC-authenticated peer. The authority
    /// identity is that authenticated sender — it is NOT carried in the message,
    /// so a member can't spoof which authority minted the key. Recipients accept
    /// it only from the elected authority (smallest room user_id).
    RoomKey { gen: u32, key: String },
    /// Member → authority: "the room is already at generation `gen`". Sent SEALED
    /// (like `RoomKey`) by any peer that holds a room key to the peer it considers
    /// the elected authority, as soon as that pairwise session comes up.
    ///
    /// Needed because the authority is the smallest user_id in the room and ids
    /// are random: a peer that joins LATER can be the new authority while
    /// starting from an empty key state. Without this it would mint generation 1,
    /// which every existing member rejects (their staged generation is higher) —
    /// leaving the newcomer sealing under a key nobody else holds and unable to
    /// open anyone else's. Carries no key material, only the generation counter,
    /// so it discloses nothing an on-path DTLS peer couldn't already count.
    RoomGen { gen: u32 },
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
        /// Must equal [`PROTOCOL_VERSION`] or the server refuses the join.
        proto: u32,
        room: String,
        user_id: String,
        name: String,
        /// Room-auth token; `None` only works against an open-auth (dev) server.
        token: Option<String>,
        /// Random per-connection value. The server lets a later connection
        /// supersede this user_id only when it presents the same secret.
        secret: String,
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
    fn room_gen_round_trips() {
        let j = serde_json::to_string(&CtrlMsg::RoomGen { gen: 7 }).unwrap();
        assert!(j.contains("\"t\":\"room-gen\""));
        match serde_json::from_str::<CtrlMsg>(&j).unwrap() {
            CtrlMsg::RoomGen { gen } => assert_eq!(gen, 7),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn bare_chatmsg_is_not_a_ctrl_msg() {
        // Pre-0.4.0 clients sent a bare ChatMsg on the DataChannel. That frame
        // must not parse as CtrlMsg — receivers drop it instead of reading it.
        let bare = serde_json::to_string(&ChatMsg { text: "old".into(), ts: 1 }).unwrap();
        assert!(serde_json::from_str::<CtrlMsg>(&bare).is_err());
    }

    #[test]
    fn join_round_trips_with_proto_and_secret() {
        let j = serde_json::to_string(&ClientMsg::Join {
            proto: PROTOCOL_VERSION,
            room: "r".into(),
            user_id: "u".into(),
            name: "n".into(),
            token: Some("t".into()),
            secret: "s".into(),
        })
        .unwrap();
        assert!(j.contains("\"t\":\"join\""));
        assert!(j.contains(&format!("\"proto\":{PROTOCOL_VERSION}")));
        match serde_json::from_str::<ClientMsg>(&j).unwrap() {
            ClientMsg::Join { proto, room, user_id, name, token, secret } => {
                assert_eq!(proto, PROTOCOL_VERSION);
                assert_eq!((room.as_str(), user_id.as_str(), name.as_str()), ("r", "u", "n"));
                assert_eq!(token.as_deref(), Some("t"));
                assert_eq!(secret, "s");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn join_without_token_still_parses() {
        // Open-auth dev servers take a join with no token; `null` and absent both work.
        for j in [
            r#"{"t":"join","proto":2,"room":"r","user_id":"u","name":"n","token":null,"secret":"s"}"#,
            r#"{"t":"join","proto":2,"room":"r","user_id":"u","name":"n","secret":"s"}"#,
        ] {
            match serde_json::from_str::<ClientMsg>(j).unwrap() {
                ClientMsg::Join { token, .. } => assert!(token.is_none()),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn pre_v2_join_does_not_deserialize() {
        // A 0.3.x join (no proto, no secret) is a parse error, not a lenient default.
        let old = r#"{"t":"join","room":"r","user_id":"u","name":"n"}"#;
        assert!(serde_json::from_str::<ClientMsg>(old).is_err());
        let no_secret = r#"{"t":"join","proto":2,"room":"r","user_id":"u","name":"n"}"#;
        assert!(serde_json::from_str::<ClientMsg>(no_secret).is_err());
        let no_proto = r#"{"t":"join","room":"r","user_id":"u","name":"n","secret":"s"}"#;
        assert!(serde_json::from_str::<ClientMsg>(no_proto).is_err());
    }

    #[test]
    fn client_and_server_tags_are_kebab_case() {
        let j = serde_json::to_string(&ServerMsg::RoomFull { cap: 16 }).unwrap();
        assert!(j.contains("\"t\":\"room-full\""));
        let j = serde_json::to_string(&ServerMsg::PeerJoined { user_id: "u".into(), name: "n".into() }).unwrap();
        assert!(j.contains("\"t\":\"peer-joined\""));
        let j = serde_json::to_string(&ClientMsg::Rekey).unwrap();
        assert_eq!(j, r#"{"t":"rekey"}"#);
        let j = serde_json::to_string(&CtrlMsg::KemHello { kem_pk: "a".into(), x_pk: "b".into() }).unwrap();
        assert!(j.contains("\"t\":\"kem-hello\""));
    }

    #[test]
    fn error_codes_are_stable_strings() {
        // Client and server compare these literally; a rename is a protocol change.
        assert_eq!(ERR_PROTOCOL_MISMATCH, "protocol-mismatch");
        assert_eq!(ERR_ID_IN_USE, "id-in-use");
    }

    #[test]
    fn protocol_version_is_two() {
        // Bumping this is a deliberate break; the test makes it a conscious edit.
        assert_eq!(PROTOCOL_VERSION, 2);
    }

    fn rt<T: Serialize + for<'a> Deserialize<'a>>(v: &T) -> (String, T) {
        let j = serde_json::to_string(v).unwrap();
        let back = serde_json::from_str(&j).unwrap();
        (j, back)
    }

    #[test]
    fn every_ctrl_msg_variant_round_trips_with_its_kebab_tag() {
        let cases: Vec<(CtrlMsg, &str)> = vec![
            (CtrlMsg::Chat(ChatMsg { text: "hi".into(), ts: 1 }), "chat"),
            (CtrlMsg::Channel { name: "Alpha".into() }, "channel"),
            (CtrlMsg::Channels { names: vec!["a".into(), "b".into()] }, "channels"),
            (CtrlMsg::ChannelRemoved { name: "b".into() }, "channel-removed"),
            (CtrlMsg::KemHello { kem_pk: "k".into(), x_pk: "x".into() }, "kem-hello"),
            (CtrlMsg::KemReply { x_pk: "x".into(), kem_ct: "c".into() }, "kem-reply"),
            (CtrlMsg::Enc { data: "ZZ==".into() }, "enc"),
            (CtrlMsg::RoomKey { gen: 3, key: "kk".into() }, "room-key"),
            (CtrlMsg::RoomGen { gen: 9 }, "room-gen"),
        ];
        for (msg, tag) in cases {
            let (j, back) = rt(&msg);
            assert!(j.contains(&format!("\"t\":\"{tag}\"")), "{j}");
            // Re-serialising the parsed value must give the identical frame.
            assert_eq!(serde_json::to_string(&back).unwrap(), j, "{tag}");
        }
        // Field-level checks for the variants with several fields.
        match serde_json::from_str::<CtrlMsg>(r#"{"t":"room-key","gen":7,"key":"abc"}"#).unwrap() {
            CtrlMsg::RoomKey { gen, key } => assert_eq!((gen, key.as_str()), (7, "abc")),
            other => panic!("{other:?}"),
        }
        match serde_json::from_str::<CtrlMsg>(r#"{"t":"channels","names":[]}"#).unwrap() {
            CtrlMsg::Channels { names } => assert!(names.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn chat_payload_is_inlined_into_the_ctrl_frame() {
        // Newtype variant with an internal tag: `text`/`ts` sit beside `t`,
        // not under a nested object.
        let j = serde_json::to_string(&CtrlMsg::Chat(ChatMsg { text: "x".into(), ts: 5 })).unwrap();
        assert_eq!(j, r#"{"t":"chat","text":"x","ts":5}"#);
    }

    #[test]
    fn unknown_or_missing_tags_are_rejected() {
        assert!(serde_json::from_str::<CtrlMsg>(r#"{"t":"bogus"}"#).is_err());
        assert!(serde_json::from_str::<CtrlMsg>(r#"{"name":"Alpha"}"#).is_err());
        assert!(serde_json::from_str::<CtrlMsg>(r#"{"t":"Channel","name":"Alpha"}"#).is_err(), "tags are case-sensitive");
        assert!(serde_json::from_str::<ClientMsg>(r#"{"t":"roster","peers":[]}"#).is_err(), "a server tag is not a client message");
        assert!(serde_json::from_str::<ServerMsg>(r#"{"t":"join"}"#).is_err());
        assert!(serde_json::from_str::<ClientMsg>(r#"{"t":"offer","to":"b"}"#).is_err(), "missing field");
    }

    #[test]
    fn unknown_fields_are_ignored_on_known_variants() {
        // serde default: extra keys do not fail the parse, so a newer peer may
        // add fields without a version bump as long as the meaning is unchanged.
        match serde_json::from_str::<CtrlMsg>(r#"{"t":"channel","name":"a","extra":1}"#).unwrap() {
            CtrlMsg::Channel { name } => assert_eq!(name, "a"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn client_messages_round_trip() {
        let cases: Vec<(ClientMsg, &str)> = vec![
            (ClientMsg::Offer { to: "b".into(), sdp: "v=0".into() }, r#"{"t":"offer","to":"b","sdp":"v=0"}"#),
            (ClientMsg::Answer { to: "b".into(), sdp: "v=0".into() }, r#"{"t":"answer","to":"b","sdp":"v=0"}"#),
            (ClientMsg::Ice { to: "b".into(), candidate: "c".into() }, r#"{"t":"ice","to":"b","candidate":"c"}"#),
            (ClientMsg::Ptt { active: true }, r#"{"t":"ptt","active":true}"#),
            (ClientMsg::Rekey, r#"{"t":"rekey"}"#),
            (ClientMsg::Leave, r#"{"t":"leave"}"#),
        ];
        for (msg, expect) in cases {
            let (j, back) = rt(&msg);
            assert_eq!(j, expect);
            assert_eq!(serde_json::to_string(&back).unwrap(), expect);
        }
    }

    #[test]
    fn server_messages_round_trip() {
        let peers = vec![PeerInfo { user_id: "b".into(), name: "Wolf".into() }];
        let cases: Vec<(ServerMsg, &str)> = vec![
            (ServerMsg::Roster { peers }, r#"{"t":"roster","peers":[{"user_id":"b","name":"Wolf"}]}"#),
            (ServerMsg::PeerJoined { user_id: "c".into(), name: "Hawk".into() }, r#"{"t":"peer-joined","user_id":"c","name":"Hawk"}"#),
            (ServerMsg::PeerLeft { user_id: "c".into() }, r#"{"t":"peer-left","user_id":"c"}"#),
            (ServerMsg::Offer { from: "a".into(), sdp: "s".into() }, r#"{"t":"offer","from":"a","sdp":"s"}"#),
            (ServerMsg::Answer { from: "a".into(), sdp: "s".into() }, r#"{"t":"answer","from":"a","sdp":"s"}"#),
            (ServerMsg::Ice { from: "a".into(), candidate: "c".into() }, r#"{"t":"ice","from":"a","candidate":"c"}"#),
            (ServerMsg::Ptt { user_id: "a".into(), active: false }, r#"{"t":"ptt","user_id":"a","active":false}"#),
            (ServerMsg::Rekey { by: "Falcon".into() }, r#"{"t":"rekey","by":"Falcon"}"#),
            (ServerMsg::RoomFull { cap: 16 }, r#"{"t":"room-full","cap":16}"#),
            (ServerMsg::Warn { size: 12, cap: 12 }, r#"{"t":"warn","size":12,"cap":12}"#),
            (ServerMsg::Error { code: "x".into(), message: "m".into() }, r#"{"t":"error","code":"x","message":"m"}"#),
        ];
        for (msg, expect) in cases {
            let (j, back) = rt(&msg);
            assert_eq!(j, expect);
            assert_eq!(serde_json::to_string(&back).unwrap(), expect);
        }
    }

    #[test]
    fn turn_creds_are_inlined_into_the_turn_frame() {
        let creds = TurnCreds {
            urls: vec!["turn:r:3478".into()],
            username: "1700000000:a".into(),
            credential: "c".into(),
            ttl: 3600,
        };
        let (j, back) = rt(&ServerMsg::Turn(creds));
        assert_eq!(j, r#"{"t":"turn","urls":["turn:r:3478"],"username":"1700000000:a","credential":"c","ttl":3600}"#);
        match back {
            ServerMsg::Turn(c) => {
                assert_eq!(c.urls, vec!["turn:r:3478"]);
                assert_eq!(c.ttl, 3600);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn peer_info_and_chat_msg_round_trip_standalone() {
        let (j, p) = rt(&PeerInfo { user_id: "u".into(), name: "n".into() });
        assert_eq!(j, r#"{"user_id":"u","name":"n"}"#);
        assert_eq!((p.user_id.as_str(), p.name.as_str()), ("u", "n"));
        let (j, c) = rt(&ChatMsg { text: "ü".into(), ts: u64::MAX });
        assert_eq!(j, r#"{"text":"ü","ts":18446744073709551615}"#);
        assert_eq!(c.ts, u64::MAX);
    }

    #[test]
    fn join_proto_must_be_an_unsigned_integer() {
        for bad in [
            r#"{"t":"join","proto":"2","room":"r","user_id":"u","name":"n","secret":"s"}"#,
            r#"{"t":"join","proto":-1,"room":"r","user_id":"u","name":"n","secret":"s"}"#,
            r#"{"t":"join","proto":2.5,"room":"r","user_id":"u","name":"n","secret":"s"}"#,
            r#"{"t":"join","proto":null,"room":"r","user_id":"u","name":"n","secret":"s"}"#,
        ] {
            assert!(serde_json::from_str::<ClientMsg>(bad).is_err(), "{bad}");
        }
    }
}
