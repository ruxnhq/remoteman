//! A device's identity (one long-lived self-signed certificate) and the list of fingerprints it
//! trusts.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

/// The SHA-256 of a certificate's DER bytes, as 64 hex digits.
/// The six digits a host and a device both show while they pair, for their owner to compare:
/// made from both keys' fingerprints, so anything standing between the two, which would have
/// to show each a key of its own, makes the two codes differ.
pub fn pair_code(host: &str, device: &str) -> String {
    let digest = Sha256::digest(format!("remoteman pairing\n{}\n{}", host.trim().to_ascii_lowercase(), device.trim().to_ascii_lowercase()));
    let number = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{:03} {:03}", number / 1000, number % 1000)
}

pub fn fingerprint(cert: &[u8]) -> String {
    hex::encode(Sha256::digest(cert))
}

pub struct Identity {
    pub cert: CertificateDer<'static>,
    key: Vec<u8>,
    pub fingerprint: String,
}

impl Identity {
    /// A fresh identity, kept only in memory.
    pub fn generate() -> Result<Self> {
        let made = rcgen::generate_simple_self_signed(vec!["remoteman".to_owned()])?;
        Ok(Self::from_parts(made.cert.der().to_vec(), made.key_pair.serialize_der()))
    }

    fn from_parts(cert: Vec<u8>, key: Vec<u8>) -> Self {
        Self { fingerprint: fingerprint(&cert), cert: CertificateDer::from(cert), key }
    }

    /// An identity given rather than kept: a certificate and its key (PKCS #8), both DER, as
    /// `cert.der` and `key.der` hold them. Checked here, so a key that isn't one, or isn't the
    /// certificate's, is found before anything connects.
    pub fn from_der(cert: Vec<u8>, key: Vec<u8>) -> Result<Self> {
        let id = Self::from_parts(cert, key);
        let provider = rustls::crypto::ring::default_provider();
        let signing = provider.key_provider.load_private_key(id.key()).map_err(|e| anyhow::anyhow!("the key isn't a key: {e}"))?;
        rustls::sign::CertifiedKey::new(vec![id.cert.clone()], signing).keys_match().map_err(|e| anyhow::anyhow!("the key isn't the certificate's: {e}"))?;
        Ok(id)
    }

    /// The identity kept in `dir`, made on first use. The key file is readable by its owner only.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let (cert_path, key_path) = (dir.join("cert.der"), dir.join("key.der"));
        if let (Ok(cert), Ok(key)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            return Ok(Self::from_parts(cert, key));
        }
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let id = Self::generate()?;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        opts.open(&key_path)?.write_all(&id.key)?;
        std::fs::write(&cert_path, &id.cert)?;
        Ok(id)
    }

    pub(crate) fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_given_identity_is_checked() {
        let one = Identity::generate().unwrap();
        let other = Identity::generate().unwrap();
        let given = Identity::from_der(one.cert.to_vec(), one.key.clone()).unwrap();
        assert_eq!(given.fingerprint, one.fingerprint);
        let crossed = Identity::from_der(one.cert.to_vec(), other.key.clone()).err().unwrap().to_string();
        assert!(crossed.contains("isn't the certificate's"), "{crossed}");
        let junk = Identity::from_der(one.cert.to_vec(), vec![1, 2, 3]).err().unwrap().to_string();
        assert!(junk.contains("isn't a key"), "{junk}");
        assert!(Identity::from_der(vec![1, 2, 3], one.key.clone()).is_err(), "a certificate that isn't one");
    }
}

/// The fingerprints a host accepts, one per line as `fingerprint name`.
#[derive(Debug)]
pub struct Trust {
    path: Option<PathBuf>,
    entries: Mutex<Vec<(String, String)>>,
    pairing: AtomicBool,
}

impl Trust {
    /// A list kept only in memory.
    pub fn in_memory() -> Self {
        Self { path: None, entries: Mutex::default(), pairing: AtomicBool::new(false) }
    }

    /// The list in `path` (which need not exist yet).
    pub fn load(path: &Path) -> Self {
        let entries = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (fp, name) = l.trim().split_once(' ').unwrap_or((l.trim(), ""));
                (fp.len() == 64).then(|| (fp.to_owned(), name.to_owned()))
            })
            .collect();
        Self { path: Some(path.to_owned()), entries: Mutex::new(entries), pairing: AtomicBool::new(false) }
    }

    pub fn contains(&self, fp: &str) -> bool {
        self.entries.lock().unwrap().iter().any(|(f, _)| f == fp)
    }

    /// While pairing, a client nobody knows gets past the handshake.
    pub fn set_pairing(&self, on: bool) {
        self.pairing.store(on, Ordering::Relaxed);
    }

    pub fn pairing(&self) -> bool {
        self.pairing.load(Ordering::Relaxed)
    }

    /// Trust `fp` from now on, under `name`.
    pub fn add(&self, fp: &str, name: &str) -> Result<()> {
        let mut e = self.entries.lock().unwrap();
        if !e.iter().any(|(f, _)| f == fp) {
            e.push((fp.to_owned(), name.replace('\n', " ")));
        }
        self.save(&e)
    }

    /// Every trusted fingerprint with the name it is kept under, oldest first.
    pub fn list(&self) -> Vec<(String, String)> {
        self.entries.lock().unwrap().clone()
    }

    /// Keep `fp` under `name` from now on. Says whether `fp` is trusted at all.
    pub fn rename(&self, fp: &str, name: &str) -> Result<bool> {
        let mut e = self.entries.lock().unwrap();
        let Some(entry) = e.iter_mut().find(|(f, _)| f == fp) else { return Ok(false) };
        let name = name.replace('\n', " ");
        if entry.1 != name {
            entry.1 = name;
            self.save(&e)?;
        }
        Ok(true)
    }

    /// Stop trusting `fp`: its next handshake is refused. Says whether it was trusted.
    pub fn remove(&self, fp: &str) -> Result<bool> {
        let mut e = self.entries.lock().unwrap();
        let before = e.len();
        e.retain(|(f, _)| f != fp);
        if e.len() == before {
            return Ok(false);
        }
        self.save(&e)?;
        Ok(true)
    }

    fn save(&self, entries: &[(String, String)]) -> Result<()> {
        if let Some(path) = &self.path {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let text: String = entries.iter().map(|(f, n)| format!("{f} {n}\n")).collect();
            std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
        }
        Ok(())
    }
}
