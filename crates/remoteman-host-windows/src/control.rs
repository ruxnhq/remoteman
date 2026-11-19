//! The owner's side of the host: what it is asked and told while it runs, by `remoteman-host
//! ctl WORDS` today and by a tray icon later. The words are the Linux host's (`status`, `pair
//! on|off|accept|decline`, `allow clipboard|files on|off`, `drop`, `revoke FINGERPRINT`), so
//! one interface can speak to both. `send PATH` sends a file to the connected device.
//!
//! It is a named pipe, `\\.\pipe\remoteman-host-PORT`, one line in and one answer out. The
//! pipe is made with Windows' default protection for pipes: the system, Administrators and the
//! account that made it may write to it. The host runs elevated, so `ctl` must too.

use std::ffi::c_void;
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use remoteman_core::Message;
use remoteman_core::host::Host;
use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_FLAGS_AND_ATTRIBUTES, FlushFileBuffers};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
use windows::core::HSTRING;

/// `PIPE_ACCESS_DUPLEX`.
const DUPLEX: u32 = 3;

/// The client being served, as the host's owner is shown it.
#[derive(Clone)]
pub struct Now {
    pub name: String,
    /// When it connected, in seconds since 1970.
    pub since: u64,
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    pub placement: &'static str,
    /// What its picture is encoded as, e.g. "AV1 10-bit".
    pub picture: String,
    pub sound: bool,
}

/// What a device may do besides see and control the desktop, and what the session says of
/// itself. Files and the clipboard are off unless the computer's owner said otherwise: any
/// paired device could otherwise write files here, or read whatever is copied.
#[derive(Clone, Default)]
pub struct Allowed {
    pub files: Arc<AtomicBool>,
    pub clipboard: Arc<AtomicBool>,
    /// A device this host can play a client's microphone into was found.
    pub microphone: Arc<AtomicBool>,
    /// The driver has a second monitor to give a device as its own.
    pub own_monitor: Arc<AtomicBool>,
    /// And a third, for a device's second display.
    pub second: Arc<AtomicBool>,
    pub now: Arc<Mutex<Option<Now>>>,
    /// What the device said of files sent to it: how many it answered for, and the last
    /// answer (whether it took it, and what it said: where it went, or why not).
    pub sent: Arc<Mutex<(u64, bool, String)>>,
}

/// What this host does, as its hello lists it. `mirror`: a device may ask for the monitor as
/// it is. `placement`: a device gets a monitor of its own, placed as it asks. `displays`: and a
/// second one, where the driver has a third monitor.
pub fn features(allowed: &Allowed) -> Vec<String> {
    let mut features: Vec<String> = ["video", "input", "cursor", "configure", "mirror", "sound"].map(String::from).to_vec();
    if allowed.own_monitor.load(Ordering::Relaxed) {
        features.push("placement".into());
        if allowed.second.load(Ordering::Relaxed) {
            features.push("displays".into());
        }
    }
    if allowed.microphone.load(Ordering::Relaxed) {
        features.push("mic".into());
    }
    if allowed.files.load(Ordering::Relaxed) {
        features.push("files".into());
    }
    if allowed.clipboard.load(Ordering::Relaxed) {
        features.push("clipboard".into());
    }
    features
}

fn saved_path(state: &Path) -> PathBuf {
    state.join("allowed.json")
}

/// What the owner switched on earlier: (files, clipboard).
pub fn load(state: &Path) -> (bool, bool) {
    let saved = std::fs::read_to_string(saved_path(state)).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let is = |key: &str| saved.as_ref().and_then(|v| v.get(key)).and_then(|v| v.as_bool()).unwrap_or(false);
    (is("files"), is("clipboard"))
}

fn save(state: &Path, allowed: &Allowed) -> std::io::Result<()> {
    let saved = serde_json::json!({ "files": allowed.files.load(Ordering::Relaxed), "clipboard": allowed.clipboard.load(Ordering::Relaxed) });
    std::fs::create_dir_all(state)?;
    std::fs::write(saved_path(state), saved.to_string())
}

pub fn pipe_name(port: u16) -> String {
    format!(r"\\.\pipe\remoteman-host-{port}")
}

/// Answer the owner's questions for as long as the host lives.
pub fn serve(port: u16, host: Weak<Host>, allowed: Allowed, state: PathBuf) {
    let name = HSTRING::from(pipe_name(port));
    let spawned = std::thread::Builder::new().name("control".into()).spawn(move || {
        let mut said = false;
        loop {
            // A pipe for each question: the one before is closed when it has been answered.
            // SAFETY: plain call; the name outlives it.
            let pipe = unsafe { CreateNamedPipeW(&name, FILE_FLAGS_AND_ATTRIBUTES(DUPLEX), PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, PIPE_UNLIMITED_INSTANCES, 65536, 65536, 0, None) };
            if pipe.is_invalid() {
                if !std::mem::replace(&mut said, true) {
                    tracing::warn!("no control pipe ({}): this host can't be asked how it stands", windows::core::Error::from_thread());
                }
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
            // SAFETY: a live pipe; waits for someone to open it.
            let connected = unsafe { ConnectNamedPipe(pipe, None) };
            // SAFETY: the handle is ours and from here on the file's, which closes it.
            let mut file = unsafe { std::fs::File::from_raw_handle(pipe.0 as *mut c_void) };
            if let Err(e) = connected {
                if e.code() != ERROR_PIPE_CONNECTED.to_hresult() {
                    continue;
                }
            }
            let mut line = Vec::new();
            let mut byte = [0u8; 1];
            while line.len() < 4096 && matches!(file.read(&mut byte), Ok(1)) && byte[0] != b'\n' {
                line.push(byte[0]);
            }
            let line = String::from_utf8_lossy(&line);
            let said = match host.upgrade() {
                Some(host) => answer(&host, &allowed, &state, line.trim()),
                None => "the host is stopping\n".into(),
            };
            let _ = file.write_all(said.as_bytes());
            // What was written is read before the pipe goes.
            // SAFETY: a live handle.
            let _ = unsafe { FlushFileBuffers(HANDLE(pipe.0)) };
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("couldn't start the control thread: {e}");
    }
}

/// `remoteman-host ctl WORDS`: ask the running host, print what it says.
pub fn ask(port: u16, words: &str) -> anyhow::Result<String> {
    let name = pipe_name(port);
    let mut last = None;
    // Between two questions the pipe is away for a moment.
    for _ in 0..40 {
        match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
            Ok(mut pipe) => {
                pipe.write_all(format!("{words}\n").as_bytes())?;
                let mut said = String::new();
                let _ = pipe.read_to_string(&mut said);
                return Ok(said);
            }
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    anyhow::bail!("no host is answering on {name}: {} (is it running, and is this prompt elevated?)", last.map(|e| e.to_string()).unwrap_or_default())
}

/// Send a file to the connected device, on a thread of its own. The device decides where it
/// goes and answers when it has it (`Allowed::sent`). As the Linux host does it.
fn send_file(link: Option<remoteman_core::host::Link>, path: &Path) -> Result<(), String> {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let link = link.ok_or("no device is connected")?;
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let header = remoteman_core::media::FileHeader { id: NEXT.fetch_add(1, Ordering::Relaxed), name, size };
    tracing::info!(bytes = size, "a file goes to the device");
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

fn answer(host: &Host, allowed: &Allowed, state: &Path, line: &str) -> String {
    // A path may have spaces in it: everything after the word.
    if let Some(path) = line.trim().strip_prefix("send ") {
        if !allowed.files.load(Ordering::Relaxed) {
            return "files are switched off on this computer\n".into();
        }
        return match send_file(host.link(), Path::new(path.trim().trim_matches('"'))) {
            Ok(()) => "ok\n".into(),
            Err(why) => format!("{why}\n"),
        };
    }
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("status") | None, _, _) => {
            let client = allowed.now.lock().unwrap().as_ref().map(|n| {
                serde_json::json!({ "name": n.name, "since": n.since, "width": n.width, "height": n.height, "refresh": n.refresh, "placement": n.placement, "picture": n.picture, "sound": n.sound })
            });
            let sent = {
                let sent = allowed.sent.lock().unwrap();
                serde_json::json!({ "answered": sent.0, "ok": sent.1, "said": sent.2 })
            };
            let status = serde_json::json!({
                "address": host.address.to_string(),
                "fingerprint": host.fingerprint,
                "pairing": host.pairing(),
                "pending": host.pending(),
                "devices": host.devices(),
                "daemon": false,
                "paused": false,
                "clipboard": allowed.clipboard.load(Ordering::Relaxed),
                "files": allowed.files.load(Ordering::Relaxed),
                "microphone": allowed.microphone.load(Ordering::Relaxed),
                "client": client,
                "sent": sent,
            });
            format!("{status}\n")
        }
        // Pairing opened here waits for its owner's word on the device that comes (the code
        // is in `status`); `--pair` on the command line trusts the first to come.
        (Some("pair"), Some(word @ ("on" | "off")), _) => {
            host.set_confirm(true);
            host.set_pairing(word == "on");
            if word == "off" {
                host.decide(false);
            }
            "ok\n".into()
        }
        (Some("pair"), Some(word @ ("accept" | "decline")), _) => {
            if host.decide(word == "accept") {
                "ok\n".into()
            } else {
                "no device is waiting\n".into()
            }
        }
        (Some("drop"), _, _) => {
            if host.drop_client() {
                "ok\n".into()
            } else {
                "no device is connected\n".into()
            }
        }
        (Some("revoke"), Some(fingerprint), _) => match host.revoke(fingerprint) {
            Ok(true) => "ok\n".into(),
            Ok(false) => "no such device\n".into(),
            Err(e) => format!("the list couldn't be saved: {e:#}\n"),
        },
        (Some("allow"), Some(what @ ("clipboard" | "files")), Some(word @ ("on" | "off"))) => {
            let on = word == "on";
            if what == "clipboard" { &allowed.clipboard } else { &allowed.files }.store(on, Ordering::Relaxed);
            let kept = save(state, allowed);
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
        _ => "status | pair on|off|accept|decline | allow clipboard|files on|off | drop | revoke FINGERPRINT | send PATH\n".into(),
    }
}
