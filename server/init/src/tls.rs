//! Self-signed TLS for the signaling endpoint. Generates + persists a cert on
//! first run (so the pinned fingerprint stays stable across restarts) and
//! reports its SHA-256 so clients can pin it. PEM-pin, no CA needed.

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

pub struct Tls {
    pub cert_pem: String,
    pub key_pem: String,
    /// SHA-256 of the cert DER, hex — the value clients pass as CERT_SHA256.
    pub fingerprint: String,
}

/// Load existing cert/key, or generate a self-signed pair and persist it.
/// SANs: localhost + 127.0.0.1, plus any comma-separated TLS_SAN entries.
pub fn ensure(cert_path: &str, key_path: &str) -> Result<Tls> {
    ensure_with_sans(cert_path, key_path, &parse_sans(std::env::var("TLS_SAN").ok().as_deref()))
}

/// Extra SANs from the comma-separated `TLS_SAN` value: trimmed, empties dropped.
fn parse_sans(env: Option<&str>) -> Vec<String> {
    env.map(|extra| extra.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// `ensure` with the extra SANs passed in (no environment access), so the
/// generate-once-then-reload behaviour can be tested in a temp dir.
fn ensure_with_sans(cert_path: &str, key_path: &str, extra_sans: &[String]) -> Result<Tls> {
    if Path::new(cert_path).exists() && Path::new(key_path).exists() {
        let cert_pem = std::fs::read_to_string(cert_path)?;
        let key_pem = std::fs::read_to_string(key_path)?;
        let fingerprint = fingerprint_of(&cert_pem)?;
        return Ok(Tls { cert_pem, key_pem, fingerprint });
    }

    let mut sans = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    sans.extend(extra_sans.iter().cloned());
    let key = rcgen::generate_simple_self_signed(sans)?;
    let cert_pem = key.cert.pem();
    let key_pem = key.key_pair.serialize_pem();
    std::fs::write(cert_path, &cert_pem)?;
    write_private(key_path, &key_pem)?;
    let fingerprint = fingerprint_of(&cert_pem)?;
    Ok(Tls { cert_pem, key_pem, fingerprint })
}

/// Write the key readable by the owner only. `fs::write` inherits the umask
/// (0644 by default), which leaves the private key world-readable on a host
/// shared with other services.
fn write_private(path: &str, pem: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(pem.as_bytes())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, pem)?;
        Ok(())
    }
}

fn fingerprint_of(cert_pem: &str) -> Result<String> {
    let mut rd = std::io::Cursor::new(cert_pem.as_bytes());
    let der = rustls_pemfile::certs(&mut rd)
        .next()
        .ok_or_else(|| anyhow!("no CERTIFICATE in PEM"))??;
    let mut h = Sha256::new();
    h.update(der.as_ref());
    Ok(hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh scratch dir per test (tests run in parallel; one shared dir
    /// would make "file exists" assertions race).
    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("subraum-tls-{tag}-{}-{}", std::process::id(), rand_tag()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn paths(&self) -> (String, String) {
            (
                self.0.join("c.pem").to_str().unwrap().to_string(),
                self.0.join("k.pem").to_str().unwrap().to_string(),
            )
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn rand_tag() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
    }

    /// The private key is written next to the cert on first run. With the
    /// default umask that is 0644 — world-readable on a shared host.
    #[cfg(unix)]
    #[test]
    fn private_key_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let s = Scratch::new("perm");
        let (cert, key) = s.paths();
        ensure_with_sans(&cert, &key, &[]).unwrap();
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key mode is {mode:o}");
    }

    #[test]
    fn first_run_writes_cert_and_key_as_pem() {
        let s = Scratch::new("first");
        let (cert, key) = s.paths();
        assert!(!Path::new(&cert).exists());
        let t = ensure_with_sans(&cert, &key, &[]).unwrap();
        assert!(Path::new(&cert).exists() && Path::new(&key).exists());
        assert!(t.cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(t.key_pem.contains("PRIVATE KEY-----"));
        assert_eq!(std::fs::read_to_string(&cert).unwrap(), t.cert_pem);
        assert_eq!(std::fs::read_to_string(&key).unwrap(), t.key_pem);
    }

    /// Clients pin the fingerprint as CERT_SHA256. A restart must therefore
    /// reload the persisted pair instead of minting a new one.
    #[test]
    fn second_run_reloads_the_same_cert_and_fingerprint() {
        let s = Scratch::new("reload");
        let (cert, key) = s.paths();
        let a = ensure_with_sans(&cert, &key, &[]).unwrap();
        let b = ensure_with_sans(&cert, &key, &[]).unwrap();
        assert_eq!(a.cert_pem, b.cert_pem);
        assert_eq!(a.key_pem, b.key_pem);
        assert_eq!(a.fingerprint, b.fingerprint);
        // Two independent first runs do NOT agree — the stability comes from the
        // files, not from determinism.
        let s2 = Scratch::new("other");
        let (c2, k2) = s2.paths();
        let c = ensure_with_sans(&c2, &k2, &[]).unwrap();
        assert_ne!(a.fingerprint, c.fingerprint);
    }

    /// Only one of the two files present means a half-written pair: regenerate
    /// both rather than pairing a stale cert with a fresh key.
    #[test]
    fn a_lone_cert_or_key_file_is_regenerated_as_a_pair() {
        let s = Scratch::new("half");
        let (cert, key) = s.paths();
        let first = ensure_with_sans(&cert, &key, &[]).unwrap();
        std::fs::remove_file(&key).unwrap();
        let second = ensure_with_sans(&cert, &key, &[]).unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);
        assert!(Path::new(&key).exists());
        assert_eq!(std::fs::read_to_string(&cert).unwrap(), second.cert_pem);
    }

    #[test]
    fn fingerprint_is_sha256_of_the_der_as_lowercase_hex() {
        let s = Scratch::new("fp");
        let (cert, key) = s.paths();
        let t = ensure_with_sans(&cert, &key, &[]).unwrap();
        assert_eq!(t.fingerprint.len(), 64);
        assert!(t.fingerprint.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Recompute independently from the PEM's DER payload.
        let mut rd = std::io::Cursor::new(t.cert_pem.as_bytes());
        let der = rustls_pemfile::certs(&mut rd).next().unwrap().unwrap();
        let mut h = Sha256::new();
        h.update(der.as_ref());
        assert_eq!(t.fingerprint, hex::encode(h.finalize()));
    }

    #[test]
    fn fingerprint_of_rejects_pem_without_a_certificate() {
        assert!(fingerprint_of("").is_err());
        assert!(fingerprint_of("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n").is_err());
        assert!(fingerprint_of("garbage").is_err());
    }

    #[test]
    fn extra_sans_end_up_in_the_certificate() {
        let s = Scratch::new("san");
        let (cert, key) = s.paths();
        let t = ensure_with_sans(&cert, &key, &["init.example.org".to_string()]).unwrap();
        let mut rd = std::io::Cursor::new(t.cert_pem.as_bytes());
        let der = rustls_pemfile::certs(&mut rd).next().unwrap().unwrap();
        // dNSName SANs are IA5String: the ASCII bytes appear verbatim in the DER.
        let has = |needle: &[u8]| der.as_ref().windows(needle.len()).any(|w| w == needle);
        assert!(has(b"init.example.org"), "extra SAN missing");
        assert!(has(b"localhost"), "default SAN missing");
    }

    #[test]
    fn tls_san_env_value_is_split_and_trimmed() {
        assert!(parse_sans(None).is_empty());
        assert!(parse_sans(Some("")).is_empty());
        assert!(parse_sans(Some(" , ,")).is_empty());
        assert_eq!(parse_sans(Some(" a.example , b.example,,c")), vec!["a.example", "b.example", "c"]);
    }
}
