//! How the host's owner asks a running host how things stand and changes them: a Unix socket
//! that takes one line and answers. Only the user the host runs as may use it. The host's
//! window and tray icon (`remoteman-tray`) talk to it, and so can a shell:
//!
//!     echo status | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/remoteman-agent-7791.sock
//!
//! - `status`: one line of JSON: the address, the key's fingerprint, whether pairing is open,
//!   what is allowed, the client being served, and the paired devices.
//! - `pair on|off`: let the next unknown device in (it is remembered, and pairing closes).
//! - `drop`: end the connected device's session; it stays paired.
//! - `revoke FINGERPRINT`: unpair a device (and drop it if it is the one connected).
//! - `allow clipboard|files on|off`: remembered for the next start, and it takes effect at
//!   once: the connected device is told, and what is switched off stops.

use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::sync::atomic::Ordering;

use remoteman_core::Message;
use remoteman_core::host::Host;

use crate::session::Allowed;

/// The socket of the host listening on `port`, in this user's runtime folder. The port is in
/// the name so that a second host (a test's, on another port) never takes the first one's.
pub fn default_path(port: u16) -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join(format!("remoteman-agent-{port}.sock"))
}

/// What the owner switched on, kept across restarts in the state folder.
#[derive(Default)]
pub struct Saved {
    pub files: bool,
    pub clipboard: bool,
}

impl Saved {
    pub fn load(state: &Path) -> Self {
        let text = std::fs::read_to_string(state.join("host-settings")).unwrap_or_default();
        let on = |key: &str| text.lines().any(|l| l.trim() == format!("{key}=on"));
        Self { files: on("files"), clipboard: on("clipboard") }
    }

    fn save(&self, state: &Path) -> std::io::Result<()> {
        let word = |on: bool| if on { "on" } else { "off" };
        std::fs::create_dir_all(state)?;
        std::fs::write(state.join("host-settings"), format!("files={}\nclipboard={}\n", word(self.files), word(self.clipboard)))
    }
}

/// What the host offers, given what is allowed now.
pub fn features(allowed: &Allowed) -> Vec<String> {
    let mut features: Vec<String> = ["video", "input", "cursor", "placement", "configure", "sound", "mic", "camera"].map(String::from).to_vec();
    if allowed.files.load(Ordering::Relaxed) {
        features.push("files".into());
    }
    if allowed.clipboard.load(Ordering::Relaxed) {
        features.push("clipboard".into());
    }
    features
}

/// Start answering at `path`. If something already answers there (another host of this user's
/// on the same port can't exist, but a stale name might be someone's), it is left alone and
/// this host runs without a control socket.
pub fn serve(path: &Path, host: Weak<Host>, allowed: Allowed, state: PathBuf) {
    if UnixStream::connect(path).is_ok() {
        return tracing::warn!("{} is in use: this host can't be asked how it stands", path.display());
    }
    // Nobody answers: a name left behind by a host that was killed.
    let _ = std::fs::remove_file(path);
    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => return tracing::warn!("no control socket at {}: {e}", path.display()),
    };
    let _ = std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
    let _ = std::thread::Builder::new().name("control".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            // SAFETY: a plain struct filled in by the kernel, with its size passed along.
            let uid = unsafe {
                let mut cred: libc::ucred = std::mem::zeroed();
                let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
                libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len);
                cred.uid
            };
            // SAFETY: no arguments, no state.
            if uid != unsafe { libc::geteuid() } {
                continue;
            }
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let answer = match host.upgrade() {
                Some(host) => answer(&host, &allowed, &state, line.trim()),
                None => "the host is stopping\n".into(),
            };
            let _ = (&stream).write_all(answer.as_bytes());
        }
    });
}

fn answer(host: &Host, allowed: &Allowed, state: &Path, line: &str) -> String {
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("status") | None, _, _) => {
            let status = serde_json::json!({
                "address": host.address.to_string(),
                "fingerprint": host.fingerprint,
                "pairing": host.pairing(),
                "clipboard": allowed.clipboard.load(Ordering::Relaxed),
                "files": allowed.files.load(Ordering::Relaxed),
                "client": *allowed.now.lock().unwrap(),
                "devices": host.devices(),
            });
            format!("{status}\n")
        }
        (Some("pair"), Some(word @ ("on" | "off")), _) => {
            host.set_pairing(word == "on");
            "ok\n".into()
        }
        (Some("drop"), _, _) => if host.drop_client() { "ok\n".into() } else { "no device is connected\n".into() },
        (Some("revoke"), Some(fingerprint), _) => match host.revoke(fingerprint) {
            Ok(true) => "ok\n".into(),
            Ok(false) => "no such device\n".into(),
            Err(e) => format!("the list couldn't be saved: {e:#}\n"),
        },
        (Some("allow"), Some(what @ ("clipboard" | "files")), Some(word @ ("on" | "off"))) => {
            let on = word == "on";
            if what == "clipboard" { &allowed.clipboard } else { &allowed.files }.store(on, Ordering::Relaxed);
            let saved = Saved { files: allowed.files.load(Ordering::Relaxed), clipboard: allowed.clipboard.load(Ordering::Relaxed) };
            let kept = saved.save(state);
            // The next client hears it in the hello; the connected one is told now. A
            // clipboard being shared stops as if the client had switched it off; a file
            // opened after this is refused where files are taken.
            host.set_features(features(allowed));
            if what == "clipboard" && !on {
                host.tell_handler(Message::Clipboard { on: false });
            }
            match kept {
                Ok(()) => "ok\n".into(),
                Err(e) => format!("done, but it won't be remembered: {e}\n"),
            }
        }
        _ => "status | pair on|off | drop | revoke FINGERPRINT | allow clipboard|files on|off\n".into(),
    }
}
