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
    /// What gets a paired device into this computer while it is locked (`session::UNLOCK_*`).
    pub unlock: u8,
}

impl Saved {
    pub fn load(state: &Path) -> Self {
        let text = std::fs::read_to_string(state.join("host-settings")).unwrap_or_default();
        let on = |key: &str| text.lines().any(|l| l.trim() == format!("{key}=on"));
        let unlock = match text.lines().find_map(|l| l.trim().strip_prefix("unlock=")) {
            Some("password") => crate::session::UNLOCK_WITH_PASSWORD,
            Some("key") => crate::session::UNLOCK_WITH_KEY,
            _ => crate::session::UNLOCK_OFF,
        };
        Self { files: on("files"), clipboard: on("clipboard"), unlock }
    }

    fn save(&self, state: &Path) -> std::io::Result<()> {
        let word = |on: bool| if on { "on" } else { "off" };
        std::fs::create_dir_all(state)?;
        std::fs::write(state.join("host-settings"), format!("files={}\nclipboard={}\nunlock={}\n", word(self.files), word(self.clipboard), unlock_word(self.unlock)))
    }
}

/// The setting for a locked computer, as it is written and said.
fn unlock_word(unlock: u8) -> &'static str {
    match unlock {
        crate::session::UNLOCK_WITH_PASSWORD => "password",
        crate::session::UNLOCK_WITH_KEY => "key",
        _ => "off",
    }
}

/// What the host offers, given what is allowed now.
pub fn features(allowed: &Allowed) -> Vec<String> {
    let mut features: Vec<String> = ["video", "input", "cursor", "placement", "mirror", "displays", "configure", "sound", "mic", "camera"].map(String::from).to_vec();
    // Not built for KDE yet: a client that asks then gets a display of its own.
    if remoteman_desktop::kde::likely() {
        features.retain(|f| f != "mirror");
    }
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
/// What listens for devices behind this control socket.
pub enum Behind {
    /// This program itself (the standalone host).
    Own(Weak<Host>),
    /// The system's daemon, at this socket: this program is the agent of one desktop
    /// session, and what belongs to the daemon (who is paired, pairing, the address) is
    /// asked of it.
    Daemon(PathBuf),
}

/// The same, for the length of one request.
enum Back<'a> {
    Own(std::sync::Arc<Host>),
    Daemon(&'a Path),
}

/// One request to the daemon (as `remotemand ctl` makes them), and its answer.
fn ask_daemon(socket: &Path, line: &str) -> Option<String> {
    use std::io::Read;
    let mut stream = UnixStream::connect(socket).ok()?;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    remoteman_core::relay::ToDaemon::Control(line.to_owned()).write(&mut stream).ok()?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).ok()?;
    Some(answer)
}

impl Back<'_> {
    /// Ask the daemon and say `ok` if its answer begins with one of these.
    fn daemon(socket: &Path, line: &str, good: &[&str]) -> String {
        match ask_daemon(socket, line) {
            Some(said) if good.iter().any(|g| said.starts_with(g)) => "ok\n".into(),
            Some(said) => said,
            None => "the daemon isn't answering\n".into(),
        }
    }

    fn link(&self, allowed: &Allowed) -> Option<remoteman_core::host::Link> {
        match self {
            Back::Own(host) => host.link(),
            Back::Daemon(_) => allowed.link.lock().unwrap().clone(),
        }
    }
}

pub fn serve(path: &Path, behind: Behind, allowed: Allowed, state: PathBuf) {
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
            let answer = match &behind {
                Behind::Own(host) => match host.upgrade() {
                    Some(host) => answer(&Back::Own(host), &allowed, &state, line.trim()),
                    None => "the host is stopping\n".into(),
                },
                Behind::Daemon(socket) => answer(&Back::Daemon(socket), &allowed, &state, line.trim()),
            };
            let _ = (&stream).write_all(answer.as_bytes());
        }
    });
}

/// Send a file to the connected device, on a thread of its own. The device decides where it
/// goes and answers when it has it (`Allowed::sent`).
fn send_file(link: Option<remoteman_core::host::Link>, path: &Path) -> Result<(), String> {
    use std::io::Read;
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let link = link.ok_or("no device is connected")?;
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let header = remoteman_core::media::FileHeader { id: NEXT.fetch_add(1, Ordering::Relaxed), name, size };
    let blob = link.open_blob(remoteman_core::media::BLOB_FILE, serde_json::to_string(&header).unwrap_or_default());
    std::thread::Builder::new()
        .name("file".into())
        .spawn(move || {
            let mut piece = vec![0u8; 256 * 1024];
            loop {
                match file.read(&mut piece) {
                    Ok(0) => {
                        let _ = blob.finish();
                        return;
                    }
                    Ok(n) if blob.write(&piece[..n]) => {}
                    // The connection went, or the file can't be read on: given up (dropping
                    // the blob tells the device).
                    _ => return,
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn answer(back: &Back, allowed: &Allowed, state: &Path, line: &str) -> String {
    // A path may have spaces in it: everything after the word.
    if let Some(path) = line.trim().strip_prefix("send ") {
        if !allowed.files.load(Ordering::Relaxed) {
            return "files are switched off on this computer\n".into();
        }
        return match send_file(back.link(allowed), Path::new(path.trim())) {
            Ok(()) => "ok\n".into(),
            Err(why) => format!("{why}\n"),
        };
    }
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("status") | None, _, _) => {
            let sent = {
                let sent = allowed.sent.lock().unwrap();
                serde_json::json!({ "answered": sent.0, "ok": sent.1, "said": sent.2 })
            };
            // What is the listening side's: this program's own, or asked of the daemon.
            let mut status = match back {
                Back::Own(host) => serde_json::json!({
                    "address": host.address.to_string(),
                    "fingerprint": host.fingerprint,
                    "pairing": host.pairing(),
                    "pending": host.pending(),
                    "devices": host.devices(),
                    "daemon": false,
                    "paused": false,
                }),
                Back::Daemon(socket) => match ask_daemon(socket, "owner").and_then(|said| serde_json::from_str::<serde_json::Value>(said.trim()).ok()).filter(|v| v.is_object()) {
                    Some(mut of_daemon) => {
                        of_daemon["daemon"] = true.into();
                        of_daemon
                    }
                    None => return "the daemon isn't answering\n".into(),
                },
            };
            status["clipboard"] = allowed.clipboard.load(Ordering::Relaxed).into();
            status["files"] = allowed.files.load(Ordering::Relaxed).into();
            status["unlock"] = unlock_word(allowed.unlock.load(Ordering::Relaxed)).into();
            status["client"] = serde_json::to_value(&*allowed.now.lock().unwrap()).unwrap_or_default();
            status["sent"] = sent;
            format!("{status}\n")
        }
        // Pairing opened here waits for its owner's word on the device that comes (the
        // code is in `status`); `--pair` on the command line trusts the first to come.
        (Some("pair"), Some(word @ ("on" | "off")), _) => match back {
            Back::Own(host) => {
                host.set_confirm(true);
                host.set_pairing(word == "on");
                if word == "off" {
                    host.decide(false);
                }
                "ok\n".into()
            }
            Back::Daemon(socket) => Back::daemon(socket, if word == "on" { "pair ask" } else { "pair off" }, &["ok", "pairing is"]),
        },
        (Some("pair"), Some(word @ ("accept" | "decline")), _) => match back {
            Back::Own(host) => if host.decide(word == "accept") { "ok\n".into() } else { "no device is waiting\n".into() },
            Back::Daemon(socket) => Back::daemon(socket, &format!("pair {word}"), &["ok"]),
        },
        (Some("drop"), _, _) => match back {
            Back::Own(host) => if host.drop_client() { "ok\n".into() } else { "no device is connected\n".into() },
            Back::Daemon(socket) => Back::daemon(socket, "drop", &["the client was dropped"]),
        },
        (Some("revoke"), Some(fingerprint), _) => match back {
            Back::Own(host) => match host.revoke(fingerprint) {
                Ok(true) => "ok\n".into(),
                Ok(false) => "no such device\n".into(),
                Err(e) => format!("the list couldn't be saved: {e:#}\n"),
            },
            Back::Daemon(socket) => match ask_daemon(socket, &format!("revoke {fingerprint}")) {
                Some(said) if said.contains("is no longer trusted") => "ok\n".into(),
                Some(said) => said,
                None => "the daemon isn't answering\n".into(),
            },
        },
        // With the daemon, sharing is switched off and on there (it turns every device away
        // meanwhile and keeps running); the standalone host is its own service to stop.
        (Some("sharing"), Some(word @ ("on" | "off")), _) => match back {
            Back::Own(_) => "this host is switched off by stopping it\n".into(),
            Back::Daemon(socket) => Back::daemon(socket, &format!("sharing {word}"), &["ok"]),
        },
        // What gets a paired device in while this computer is locked.
        (Some("allow"), Some("unlock"), Some(word @ ("off" | "password" | "key"))) => {
            let unlock = match word {
                "password" => crate::session::UNLOCK_WITH_PASSWORD,
                "key" => crate::session::UNLOCK_WITH_KEY,
                _ => crate::session::UNLOCK_OFF,
            };
            allowed.unlock.store(unlock, Ordering::Relaxed);
            let saved = Saved { files: allowed.files.load(Ordering::Relaxed), clipboard: allowed.clipboard.load(Ordering::Relaxed), unlock };
            match saved.save(state) {
                Ok(()) => "ok\n".into(),
                Err(e) => format!("done, but it won't be remembered: {e}\n"),
            }
        }
        (Some("allow"), Some(what @ ("clipboard" | "files")), Some(word @ ("on" | "off"))) => {
            let on = word == "on";
            if what == "clipboard" { &allowed.clipboard } else { &allowed.files }.store(on, Ordering::Relaxed);
            let saved = Saved { files: allowed.files.load(Ordering::Relaxed), clipboard: allowed.clipboard.load(Ordering::Relaxed), unlock: allowed.unlock.load(Ordering::Relaxed) };
            let kept = saved.save(state);
            // The next client hears it in the hello; the connected one is told now. A
            // clipboard being shared stops as if the client had switched it off; a file
            // opened after this is refused where files are taken.
            match back {
                Back::Own(host) => {
                    host.set_features(features(allowed));
                    if what == "clipboard" && !on {
                        host.tell_handler(Message::Clipboard { on: false });
                    }
                }
                // (The daemon's hello lists everything an agent might allow; the connected
                // device is told what this session allows now. A clipboard being shared
                // stops by itself: the session sees the switch, session.rs,
                // `clipboard_switched_off`.)
                Back::Daemon(_) => {
                    if let Some(link) = back.link(allowed) {
                        link.send_message(Message::Features { features: features(allowed) });
                    }
                }
            }
            match kept {
                Ok(()) => "ok\n".into(),
                Err(e) => format!("done, but it won't be remembered: {e}\n"),
            }
        }
        _ => "status | sharing on|off | pair on|off|accept|decline | drop | revoke FINGERPRINT | allow clipboard|files on|off | allow unlock off|password|key | send PATH\n".into(),
    }
}
