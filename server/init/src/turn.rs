//! Ephemeral TURN credentials (coturn `use-auth-secret` REST scheme).
//! username = "<unix-expiry>:<user_id>", credential = base64(HMAC-SHA1(secret, username)).
//! coturn validates the HMAC itself — no account store, no state.

use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use protocol::TurnCreds;
use sha1::Sha1;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha1 = Hmac<Sha1>;

pub struct TurnConfig {
    secret: Vec<u8>,
    urls: Vec<String>,
    ttl: u32,
}

impl TurnConfig {
    /// Enabled only when both TURN_SECRET and TURN_URLS (comma-separated) are set.
    pub fn from_env() -> Option<Self> {
        let secret = std::env::var("TURN_SECRET").ok().filter(|s| !s.is_empty())?;
        let urls = std::env::var("TURN_URLS").ok().filter(|s| !s.is_empty())?;
        Some(Self::new(secret, &urls))
    }

    /// Split out of `from_env` so the credential scheme can be exercised without
    /// touching process environment (which no parallel test can own).
    fn new(secret: String, urls: &str) -> Self {
        Self {
            secret: secret.into_bytes(),
            urls: urls.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            ttl: 3600,
        }
    }

    pub fn mint(&self, user_id: &str) -> TurnCreds {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let expiry = now + self.ttl as u64;
        let username = format!("{expiry}:{user_id}");
        let mut mac = HmacSha1::new_from_slice(&self.secret).expect("HMAC accepts any key length");
        mac.update(username.as_bytes());
        let credential = STANDARD.encode(mac.finalize().into_bytes());
        TurnCreds { urls: self.urls.clone(), username, credential, ttl: self.ttl }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The credential coturn will recompute on its side. Written out longhand
    /// here on purpose: if `mint` ever drifts from the `use-auth-secret` scheme,
    /// every relay allocation fails at the TURN server with no clue why.
    fn expected_credential(secret: &[u8], username: &str) -> String {
        let mut mac = HmacSha1::new_from_slice(secret).unwrap();
        mac.update(username.as_bytes());
        STANDARD.encode(mac.finalize().into_bytes())
    }

    #[test]
    fn credentials_follow_the_coturn_rest_scheme() {
        let cfg = TurnConfig::new("s3cret".into(), "turn:relay.example:3478");
        let c = cfg.mint("user-1");

        let (expiry, user) = c.username.split_once(':').expect("username is <expiry>:<user_id>");
        assert_eq!(user, "user-1");
        let expiry: u64 = expiry.parse().expect("expiry is a unix timestamp");

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(expiry > now, "credential must not be born expired");
        assert!(expiry <= now + c.ttl as u64, "expiry is now + ttl");
        assert_eq!(c.ttl, 3600);

        assert_eq!(c.credential, expected_credential(b"s3cret", &c.username));
    }

    #[test]
    fn the_credential_is_bound_to_the_user_and_the_secret() {
        let cfg = TurnConfig::new("s3cret".into(), "turn:relay.example:3478");
        let a = cfg.mint("user-a");
        let b = cfg.mint("user-b");
        assert_ne!(a.credential, b.credential, "one user's creds must not work for another");

        let other = TurnConfig::new("different".into(), "turn:relay.example:3478");
        assert_ne!(other.mint("user-a").credential, expected_credential(b"s3cret", &a.username));
    }

    #[test]
    fn url_list_is_split_and_trimmed() {
        let cfg = TurnConfig::new("s".into(), " turn:a:3478 , turns:b:5349 ,, ");
        assert_eq!(cfg.mint("u").urls, vec!["turn:a:3478", "turns:b:5349"]);
    }
}
