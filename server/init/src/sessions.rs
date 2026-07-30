//! PIN-protected session brokering. A host creates a session → random room +
//! join token + a random 6-digit PIN + a short share code. Mates resolve the
//! code with the PIN (rate-limited, so a 6-digit PIN is safe) and get the
//! room + token to connect config-less. State is in-memory + TTL'd (sessions
//! are ephemeral, like rooms).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::Rng;

/// Hard cap: a session ends at most 24h after creation, no matter what.
const MAX_AGE_SECS: u64 = 24 * 3600;
/// Grace after the room goes empty (covers create→connect + brief reconnects).
const EMPTY_GRACE_SECS: u64 = 5 * 60;
const MAX_ATTEMPTS: u32 = 6; // wrong-PIN tries before the code locks

pub struct Session {
    pub room: String,
    pub token: Option<String>,
    pin: String,
    created: u64,
    /// Last time the room had ≥1 connected member (init = created, so the host
    /// has the grace window to connect before it counts as empty).
    last_active: u64,
    attempts: u32,
}

pub enum JoinError {
    NotFound,
    Locked,
    BadPin,
}

#[derive(Default)]
pub struct Sessions {
    inner: Mutex<HashMap<String, Session>>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn rand_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill(&mut b[..]);
    hex::encode(b)
}

fn rand_code() -> String {
    // Unambiguous alphabet (no 0/o/1/l): easy to read off a share link.
    const CH: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut r = rand::thread_rng();
    (0..8).map(|_| CH[r.gen_range(0..CH.len())] as char).collect()
}

fn rand_pin() -> String {
    let n: u32 = rand::thread_rng().gen_range(0..1_000_000);
    format!("{n:06}")
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

impl Sessions {
    /// Create a session. `token_for` mints the room's join token (None in open mode).
    /// Returns (code, pin, room, token).
    pub fn create<F: Fn(&str) -> Option<String>>(
        &self,
        token_for: F,
    ) -> (String, String, String, Option<String>) {
        let room = format!("squad-{}", rand_hex(8)); // 64-bit random room name
        let token = token_for(&room);
        let pin = rand_pin();
        let t = now();
        let mut map = self.inner.lock().unwrap();
        let mut code = rand_code();
        while map.contains_key(&code) {
            code = rand_code();
        }
        map.insert(
            code.clone(),
            Session { room: room.clone(), token: token.clone(), pin: pin.clone(), created: t, last_active: t, attempts: 0 },
        );
        (code, pin, room, token)
    }

    /// Resolve a code with a PIN. Rate-limited per code.
    pub fn join(&self, code: &str, pin: &str) -> Result<(String, Option<String>), JoinError> {
        let mut map = self.inner.lock().unwrap();
        let s = map.get_mut(code).ok_or(JoinError::NotFound)?;
        if s.attempts >= MAX_ATTEMPTS {
            return Err(JoinError::Locked);
        }
        if ct_eq(s.pin.as_bytes(), pin.as_bytes()) {
            s.last_active = now(); // a successful join keeps it alive
            Ok((s.room.clone(), s.token.clone()))
        } else {
            s.attempts += 1;
            Err(JoinError::BadPin)
        }
    }

    /// Lifecycle sweep (call periodically). A session is kept while its room has
    /// connected members; once empty it survives EMPTY_GRACE, and never past
    /// MAX_AGE. `room_nonempty(room)` reports live membership.
    pub fn reap<F: Fn(&str) -> bool>(&self, room_nonempty: F) {
        let n = now();
        let mut map = self.inner.lock().unwrap();
        map.retain(|_, s| {
            if n.saturating_sub(s.created) >= MAX_AGE_SECS {
                return false; // 24h hard cap
            }
            if room_nonempty(&s.room) {
                s.last_active = n;
                return true;
            }
            n.saturating_sub(s.last_active) < EMPTY_GRACE_SECS
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions() -> Sessions {
        Sessions::default()
    }

    /// Mints a token the way `AuthConfig::Hmac` would, without pulling auth in.
    fn token_for(room: &str) -> Option<String> {
        Some(format!("token-for-{room}"))
    }

    #[test]
    fn create_yields_a_usable_code_pin_and_room() {
        let s = sessions();
        let (code, pin, room, token) = s.create(token_for);

        assert_eq!(pin.len(), 6);
        assert!(pin.chars().all(|c| c.is_ascii_digit()), "PIN is 6 digits");
        assert!(room.starts_with("squad-"));
        assert_eq!(token.as_deref(), Some(format!("token-for-{room}").as_str()));

        // The share code must survive being read off a link out loud.
        assert_eq!(code.len(), 8);
        assert!(
            code.chars().all(|c| "abcdefghijkmnpqrstuvwxyz23456789".contains(c)),
            "no 0/o/1/l in the share alphabet: {code}"
        );

        let (got_room, got_token) = s.join(&code, &pin).ok().expect("correct PIN joins");
        assert_eq!(got_room, room);
        assert_eq!(got_token, token);
    }

    #[test]
    fn codes_and_pins_differ_between_sessions() {
        let s = sessions();
        let (c1, p1, r1, _) = s.create(token_for);
        let (c2, _, r2, _) = s.create(token_for);
        assert_ne!(c1, c2);
        assert_ne!(r1, r2);
        // One session's PIN must not open another's code.
        assert!(matches!(s.join(&c2, &p1), Err(JoinError::BadPin) | Ok(_)) );
        if let Ok(_) = s.join(&c2, &p1) {
            // 1-in-a-million collision; only a real leak would be systematic.
            let (c3, _, _, _) = s.create(token_for);
            assert!(matches!(s.join(&c3, &p1), Err(JoinError::BadPin)));
        }
    }

    #[test]
    fn unknown_code_is_not_found() {
        let s = sessions();
        assert!(matches!(s.join("nosuch12", "123456"), Err(JoinError::NotFound)));
    }

    /// A 6-digit PIN is only safe because guessing is capped — without this the
    /// share code is brute-forceable in a few thousand requests.
    #[test]
    fn wrong_pins_lock_the_code_out() {
        let s = sessions();
        let (code, pin, _, _) = s.create(token_for);
        let wrong = if pin == "000000" { "111111" } else { "000000" };

        for i in 0..MAX_ATTEMPTS {
            assert!(matches!(s.join(&code, wrong), Err(JoinError::BadPin)), "attempt {i}");
        }
        assert!(matches!(s.join(&code, wrong), Err(JoinError::Locked)));
        // Locked means locked: the CORRECT PIN no longer helps either.
        assert!(matches!(s.join(&code, &pin), Err(JoinError::Locked)));
    }

    #[test]
    fn a_correct_pin_does_not_count_against_the_attempt_budget() {
        let s = sessions();
        let (code, pin, _, _) = s.create(token_for);
        for _ in 0..(MAX_ATTEMPTS * 3) {
            assert!(s.join(&code, &pin).is_ok());
        }
    }

    #[test]
    fn reap_keeps_a_session_whose_room_is_occupied() {
        let s = sessions();
        let (code, pin, _, _) = s.create(token_for);
        s.reap(|_| true);
        assert!(s.join(&code, &pin).is_ok(), "occupied room must survive the sweep");
    }

    /// Freshly created and still empty: the host has not connected yet, so the
    /// grace window must keep it alive or every share link would die on birth.
    #[test]
    fn reap_keeps_a_brand_new_empty_session_during_the_grace() {
        let s = sessions();
        let (code, pin, _, _) = s.create(token_for);
        s.reap(|_| false);
        assert!(s.join(&code, &pin).is_ok());
    }

    #[test]
    fn reap_drops_a_session_once_it_is_past_the_grace() {
        let s = sessions();
        let (code, pin, _, _) = s.create(token_for);
        // Backdate last_active past EMPTY_GRACE — the sweep reads the clock, so
        // this is the only way to exercise expiry without waiting five minutes.
        {
            let mut map = s.inner.lock().unwrap();
            let sess = map.get_mut(&code).unwrap();
            sess.last_active = now().saturating_sub(EMPTY_GRACE_SECS + 1);
        }
        s.reap(|_| false);
        assert!(matches!(s.join(&code, &pin), Err(JoinError::NotFound)));
    }

    #[test]
    fn reap_drops_a_session_past_the_24h_cap_even_if_occupied() {
        let s = sessions();
        let (code, pin, _, _) = s.create(token_for);
        {
            let mut map = s.inner.lock().unwrap();
            map.get_mut(&code).unwrap().created = now().saturating_sub(MAX_AGE_SECS + 1);
        }
        s.reap(|_| true); // still busy — the hard cap wins anyway
        assert!(matches!(s.join(&code, &pin), Err(JoinError::NotFound)));
    }

    #[test]
    fn ct_eq_matches_plain_comparison() {
        assert!(ct_eq(b"123456", b"123456"));
        assert!(!ct_eq(b"123456", b"123457"));
        assert!(!ct_eq(b"123456", b"12345"));
    }
}
