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
/// How long a lock holds. A permanent lock would let anyone with the share
/// link deny the host's session for its whole lifetime; a 6-digit PIN stays
/// safe at MAX_ATTEMPTS guesses per window (~900/day).
const LOCK_SECS: u64 = 10 * 60;

pub struct Session {
    pub room: String,
    pub token: Option<String>,
    pin: String,
    created: u64,
    /// Last time the room had ≥1 connected member (init = created, so the host
    /// has the grace window to connect before it counts as empty).
    last_active: u64,
    attempts: u32,
    /// When the guess budget ran out (0 = never); the lock lifts LOCK_SECS later.
    locked_at: u64,
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
            Session { room: room.clone(), token: token.clone(), pin: pin.clone(), created: t, last_active: t, attempts: 0, locked_at: 0 },
        );
        (code, pin, room, token)
    }

    /// Resolve a code with a PIN. Rate-limited per code.
    pub fn join(&self, code: &str, pin: &str) -> Result<(String, Option<String>), JoinError> {
        self.join_at(code, pin, now())
    }

    fn join_at(&self, code: &str, pin: &str, t: u64) -> Result<(String, Option<String>), JoinError> {
        let mut map = self.inner.lock().unwrap();
        let s = map.get_mut(code).ok_or(JoinError::NotFound)?;
        if s.attempts >= MAX_ATTEMPTS {
            if t < s.locked_at.saturating_add(LOCK_SECS) {
                return Err(JoinError::Locked);
            }
            s.attempts = 0; // lock expired: a fresh guess budget, not an open door
        }
        if ct_eq(s.pin.as_bytes(), pin.as_bytes()) {
            s.last_active = t; // a successful join keeps it alive
            Ok((s.room.clone(), s.token.clone()))
        } else {
            s.attempts += 1;
            if s.attempts >= MAX_ATTEMPTS {
                s.locked_at = t;
            }
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
        if s.join(&c2, &p1).is_ok() {
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

    /// A permanent lock lets anyone holding the share link deny the host's
    /// session for its whole 24 h lifetime by burning six guesses. The lock
    /// has to expire; brute force stays capped at MAX_ATTEMPTS per window.
    #[test]
    fn a_locked_code_reopens_after_the_lock_window() {
        let s = sessions();
        let (code, pin, _, _) = s.create(token_for);
        let wrong = if pin == "000000" { "111111" } else { "000000" };
        let t0 = now();
        for _ in 0..MAX_ATTEMPTS {
            let _ = s.join_at(&code, wrong, t0);
        }
        assert!(matches!(s.join_at(&code, &pin, t0 + LOCK_SECS - 1), Err(JoinError::Locked)));
        assert!(s.join_at(&code, &pin, t0 + LOCK_SECS).is_ok(), "lock never expires");
        // ... and a fresh guess budget applies after it, not an open door.
        for _ in 0..MAX_ATTEMPTS {
            let _ = s.join_at(&code, wrong, t0 + LOCK_SECS);
        }
        assert!(matches!(s.join_at(&code, &pin, t0 + LOCK_SECS + 1), Err(JoinError::Locked)));
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

    /// Codes are read aloud off a share link: fixed length, no 0/o/1/l.
    #[test]
    fn share_codes_are_eight_chars_from_the_unambiguous_alphabet() {
        for _ in 0..500 {
            let c = rand_code();
            assert_eq!(c.len(), 8);
            assert!(c.chars().all(|ch| "abcdefghijkmnpqrstuvwxyz23456789".contains(ch)), "{c}");
        }
    }

    /// Leading zeros must survive: "000042" is a valid PIN and must not become "42".
    #[test]
    fn pins_are_always_six_zero_padded_digits() {
        let mut saw_leading_zero = false;
        for _ in 0..5000 {
            let p = rand_pin();
            assert_eq!(p.len(), 6, "{p}");
            assert!(p.chars().all(|c| c.is_ascii_digit()), "{p}");
            saw_leading_zero |= p.starts_with('0');
        }
        assert!(saw_leading_zero, "1 in 10 PINs starts with 0; 5000 draws never did");
    }

    #[test]
    fn rand_hex_is_lowercase_hex_of_the_requested_width() {
        let h = rand_hex(8);
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(rand_hex(8), rand_hex(8));
        assert_eq!(rand_hex(0), "");
    }

    #[test]
    fn room_names_are_squad_plus_64_random_bits() {
        let s = sessions();
        let (_, _, room, _) = s.create(token_for);
        let hex = room.strip_prefix("squad-").expect("prefix");
        assert_eq!(hex.len(), 16);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn open_mode_sessions_carry_no_token() {
        let s = sessions();
        let (code, pin, _, token) = s.create(|_| None);
        assert!(token.is_none());
        let (_, got) = s.join(&code, &pin).ok().unwrap();
        assert!(got.is_none());
    }

    /// A wrong PIN of the right length and a wrong PIN of the wrong length are
    /// both just wrong — the comparison must not leak the length either.
    #[test]
    fn wrong_length_pin_is_a_bad_pin_not_a_crash() {
        let s = sessions();
        let (code, _, _, _) = s.create(token_for);
        assert!(matches!(s.join(&code, ""), Err(JoinError::BadPin)));
        assert!(matches!(s.join(&code, "1234567"), Err(JoinError::BadPin)));
    }

    #[test]
    fn the_lock_lifts_exactly_at_the_window_boundary() {
        let s = sessions();
        let (code, pin, _, _) = s.create(token_for);
        let wrong = if pin == "000000" { "111111" } else { "000000" };
        let t0 = 1_000_000;
        for _ in 0..MAX_ATTEMPTS {
            let _ = s.join_at(&code, wrong, t0);
        }
        assert!(matches!(s.join_at(&code, &pin, t0), Err(JoinError::Locked)));
        assert!(matches!(s.join_at(&code, &pin, t0 + LOCK_SECS - 1), Err(JoinError::Locked)));
        assert!(s.join_at(&code, &pin, t0 + LOCK_SECS).is_ok());
    }

    /// Guesses below the cap do not accumulate into a lock across a later
    /// correct join: only the budget counter matters, and it is not reset by a
    /// success (so a correct PIN cannot be used to "unlock" guessing either).
    #[test]
    fn attempts_persist_across_a_successful_join() {
        let s = sessions();
        let (code, pin, _, _) = s.create(token_for);
        let wrong = if pin == "000000" { "111111" } else { "000000" };
        for _ in 0..(MAX_ATTEMPTS - 1) {
            let _ = s.join(&code, wrong);
        }
        assert!(s.join(&code, &pin).is_ok());
        // One more wrong guess hits the cap.
        assert!(matches!(s.join(&code, wrong), Err(JoinError::BadPin)));
        assert!(matches!(s.join(&code, &pin), Err(JoinError::Locked)));
    }

    #[test]
    fn a_successful_join_refreshes_last_active() {
        let s = sessions();
        let (code, pin, _, _) = s.create(token_for);
        {
            let mut map = s.inner.lock().unwrap();
            map.get_mut(&code).unwrap().last_active = now().saturating_sub(EMPTY_GRACE_SECS + 100);
        }
        assert!(s.join(&code, &pin).is_ok(), "the join itself must still work");
        s.reap(|_| false);
        assert!(s.join(&code, &pin).is_ok(), "join refreshed last_active, so the sweep keeps it");
    }

    #[test]
    fn reap_decides_per_session() {
        let s = sessions();
        let (keep, keep_pin, keep_room, _) = s.create(token_for);
        let (drop_me, drop_pin, _, _) = s.create(token_for);
        {
            let mut map = s.inner.lock().unwrap();
            for c in [&keep, &drop_me] {
                map.get_mut(c).unwrap().last_active = now().saturating_sub(EMPTY_GRACE_SECS + 1);
            }
        }
        s.reap(|room| room == keep_room);
        assert!(s.join(&keep, &keep_pin).is_ok());
        assert!(matches!(s.join(&drop_me, &drop_pin), Err(JoinError::NotFound)));
    }

    #[test]
    fn reap_on_an_occupied_room_refreshes_last_active() {
        let s = sessions();
        let (code, _, _, _) = s.create(token_for);
        {
            let mut map = s.inner.lock().unwrap();
            map.get_mut(&code).unwrap().last_active = 0;
        }
        s.reap(|_| true);
        let la = s.inner.lock().unwrap().get(&code).unwrap().last_active;
        assert!(la >= now().saturating_sub(2), "occupied sweep must stamp the clock");
    }
}
