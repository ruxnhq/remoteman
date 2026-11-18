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
