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
    if Path::new(cert_path).exists() && Path::new(key_path).exists() {
        let cert_pem = std::fs::read_to_string(cert_path)?;
        let key_pem = std::fs::read_to_string(key_path)?;
        let fingerprint = fingerprint_of(&cert_pem)?;
        return Ok(Tls { cert_pem, key_pem, fingerprint });
    }

    let mut sans = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    if let Ok(extra) = std::env::var("TLS_SAN") {
        for s in extra.split(',') {
            let s = s.trim();
            if !s.is_empty() {
                sans.push(s.to_string());
            }
        }
    }
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

#[cfg(all(test, unix))]
mod perm_tests {
    use std::os::unix::fs::PermissionsExt;

    /// The private key is written next to the cert on first run. With the
    /// default umask that is 0644 — world-readable on a shared host.
    #[test]
    fn private_key_is_not_world_readable() {
        let dir = std::env::temp_dir().join(format!("subraum-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cert = dir.join("c.pem");
        let key = dir.join("k.pem");
        super::ensure(cert.to_str().unwrap(), key.to_str().unwrap()).unwrap();
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(mode, 0o600, "key mode is {mode:o}");
    }
}
