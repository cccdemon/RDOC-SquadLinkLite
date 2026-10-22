//! Room-auth. Each room's join token = HMAC-SHA256(secret, room) as hex.
//! Whoever holds the server secret + room name can mint the invite token
//! (see the `mint` subcommand). Expiry/rotation: later (§15.4).

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub enum AuthConfig {
    /// Dev mode: no token required. Loudly warned at startup.
    Open,
    Hmac(Vec<u8>),
}

impl AuthConfig {
    /// Build from env, FAIL-CLOSED: Open mode is only allowed when explicitly
    /// opted into via `ALLOW_OPEN_AUTH=1` (dev). Otherwise a missing
    /// `ROOM_AUTH_SECRET` is a hard error so production never silently runs open.
    pub fn from_env() -> Result<Self, String> {
        Self::from_values(std::env::var("ROOM_AUTH_SECRET").ok(), std::env::var("ALLOW_OPEN_AUTH").ok())
    }

    /// The decision behind `from_env`, on plain values: `secret` is
    /// `ROOM_AUTH_SECRET`, `allow_open` is `ALLOW_OPEN_AUTH`. Split out so the
    /// fail-closed rule can be tested without touching process environment.
    fn from_values(secret: Option<String>, allow_open: Option<String>) -> Result<Self, String> {
        match secret {
            Some(s) if !s.is_empty() => Ok(Self::Hmac(s.into_bytes())),
            _ => {
                let allow_open = allow_open
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                if allow_open {
                    Ok(Self::Open)
                } else {
                    Err("ROOM_AUTH_SECRET is not set — refusing to start in OPEN auth mode. \
Set ROOM_AUTH_SECRET=<secret> for production, or ALLOW_OPEN_AUTH=1 for local dev only."
                        .into())
                }
            }
        }
    }

    /// The valid join token for a room (None in Open mode).
    pub fn token_for(&self, room: &str) -> Option<String> {
        match self {
            Self::Open => None,
            Self::Hmac(key) => Some(hmac_hex(key, room.as_bytes())),
        }
    }

    /// Validate a presented token for a room (constant-time).
    pub fn check(&self, room: &str, token: Option<&str>) -> bool {
        match self {
            Self::Open => true,
            Self::Hmac(key) => {
                let expected = hmac_hex(key, room.as_bytes());
                token.map(|t| ct_eq(t.as_bytes(), expected.as_bytes())).unwrap_or(false)
            }
        }
    }
}

fn hmac_hex(key: &[u8], msg: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(msg);
    hex::encode(mac.finalize().into_bytes())
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hmac() -> AuthConfig {
        AuthConfig::Hmac(b"test-secret".to_vec())
    }

    #[test]
    fn minted_token_is_the_one_that_validates() {
        let a = hmac();
        let t = a.token_for("squad-abc").expect("hmac mode mints a token");
        assert!(a.check("squad-abc", Some(&t)));
        // Stable across calls — the `mint` subcommand and the running server
        // must agree without sharing state.
        assert_eq!(a.token_for("squad-abc").as_deref(), Some(t.as_str()));
        assert_eq!(t.len(), 64, "HMAC-SHA256 as hex");
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_token_is_bound_to_its_room() {
        let a = hmac();
        let t = a.token_for("squad-abc").unwrap();
        // The whole point: holding one room's invite must not open another.
        assert!(!a.check("squad-def", Some(&t)));
    }

    #[test]
    fn a_different_secret_yields_a_different_token() {
        let other = AuthConfig::Hmac(b"other-secret".to_vec());
        let t = hmac().token_for("squad-abc").unwrap();
        assert!(!other.check("squad-abc", Some(&t)));
    }

    #[test]
    fn missing_or_malformed_tokens_are_refused() {
        let a = hmac();
        let t = a.token_for("squad-abc").unwrap();
        assert!(!a.check("squad-abc", None), "no token at all");
        assert!(!a.check("squad-abc", Some("")), "empty");
        assert!(!a.check("squad-abc", Some(&t[..63])), "truncated");
        assert!(!a.check("squad-abc", Some(&format!("{t}00"))), "over-long");
        assert!(!a.check("squad-abc", Some(&t.to_uppercase())), "hex case must match");

        // Flip the last nibble — a near-miss must fail like any other.
        let mut bad = t.clone();
        let last = bad.pop().unwrap();
        bad.push(if last == '0' { '1' } else { '0' });
        assert!(!a.check("squad-abc", Some(&bad)));
    }

    #[test]
    fn open_mode_mints_nothing_and_accepts_anything() {
        // Dev-only mode, reachable solely via ALLOW_OPEN_AUTH=1.
        let a = AuthConfig::Open;
        assert_eq!(a.token_for("squad-abc"), None);
        assert!(a.check("squad-abc", None));
        assert!(a.check("squad-abc", Some("garbage")));
    }

    #[test]
    fn ct_eq_matches_plain_comparison() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab")); // length mismatch short-circuits
        assert!(ct_eq(b"", b""));
    }

    /// The token IS HMAC-SHA256(secret, room) as lowercase hex — the `mint`
    /// subcommand, FleetPlanner and anyone scripting invites rely on exactly
    /// that formula, so pin it independently of `hmac_hex`.
    #[test]
    fn token_is_hmac_sha256_of_the_room_name() {
        let mut mac = HmacSha256::new_from_slice(b"test-secret").unwrap();
        mac.update(b"squad-abc");
        let expected = hex::encode(mac.finalize().into_bytes());
        assert_eq!(hmac().token_for("squad-abc").unwrap(), expected);
        assert_eq!(expected, expected.to_lowercase());
    }

    #[test]
    fn a_set_secret_means_hmac_mode_regardless_of_allow_open() {
        for allow in [None, Some("1".to_string()), Some("0".to_string())] {
            let a = AuthConfig::from_values(Some("s".into()), allow).unwrap();
            assert!(matches!(a, AuthConfig::Hmac(ref k) if k == b"s"));
        }
    }

    /// Fail-closed: no secret and no explicit opt-in is a startup error, never
    /// a silently open server.
    #[test]
    fn missing_secret_without_allow_open_refuses_to_start() {
        for secret in [None, Some(String::new())] {
            for allow in [None, Some("0".to_string()), Some("yes".to_string()), Some(String::new())] {
                let err = AuthConfig::from_values(secret.clone(), allow.clone()).err().expect("must refuse");
                assert!(err.contains("ROOM_AUTH_SECRET"), "{err}");
                assert!(err.contains("ALLOW_OPEN_AUTH"), "{err}");
            }
        }
    }

    #[test]
    fn allow_open_one_or_true_opens_only_without_a_secret() {
        for allow in ["1", "true", "TRUE", "True"] {
            let a = AuthConfig::from_values(None, Some(allow.into())).unwrap();
            assert!(matches!(a, AuthConfig::Open), "{allow}");
            let a = AuthConfig::from_values(Some(String::new()), Some(allow.into())).unwrap();
            assert!(matches!(a, AuthConfig::Open), "empty secret counts as unset");
        }
        // Whitespace is not trimmed here: " true" stays off (unlike main's flag_on).
        assert!(AuthConfig::from_values(None, Some(" true".into())).is_err());
    }

    #[test]
    fn hmac_accepts_any_key_length() {
        // Short and long keys both work — the HMAC spec pads/hashes them.
        let short = AuthConfig::Hmac(b"k".to_vec());
        let long = AuthConfig::Hmac(vec![b'x'; 300]);
        let ts = short.token_for("r").unwrap();
        let tl = long.token_for("r").unwrap();
        assert_eq!(ts.len(), 64);
        assert_eq!(tl.len(), 64);
        assert_ne!(ts, tl);
        assert!(short.check("r", Some(&ts)));
        assert!(long.check("r", Some(&tl)));
    }

    #[test]
    fn room_names_with_unicode_and_empty_string_mint_distinct_tokens() {
        let a = hmac();
        let empty = a.token_for("").unwrap();
        let uni = a.token_for("Übung-ß").unwrap();
        assert_ne!(empty, uni);
        assert!(a.check("", Some(&empty)));
        assert!(a.check("Übung-ß", Some(&uni)));
        assert!(!a.check("Übung-ss", Some(&uni)));
    }
}
