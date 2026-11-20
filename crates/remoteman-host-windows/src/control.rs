//! The owner's side of the host: what it is asked and told while it runs, by `remoteman-host
//! ctl WORDS` today and by a tray icon later. The words are the Linux host's (`status`, `pair
//! on|off|accept|decline`, `allow clipboard|files on|off`, `drop`, `revoke FINGERPRINT`), so
//! one interface can speak to both. `send PATH` sends a file to the connected device.
//!
//! It is a named pipe, `\\.\pipe\remoteman-host-PORT`, one line in and one answer out. The
//! system, Administrators and the user the host runs as may use it, that user also from a
//! prompt that isn't elevated (the owner's app, Remoteman Host, runs so); nobody else.
//!
//! `sharing off` turns every device away (the connected one is dropped, pairing closes) until
//! `sharing on`; it is remembered in the state folder, as the switches for files and the
//! clipboard are.

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
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, RevertToSelf, SECURITY_ATTRIBUTES};
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
    /// A device's monitor can be made HDR and the encoder can say so.
    pub hdr: Arc<AtomicBool>,
    /// Sharing is switched off: no device may connect.
    pub paused: Arc<AtomicBool>,
    /// The PC has screens of its own, which this host can't capture (only the driver's
    /// monitors): no `mirror`.
    pub real_screens: Arc<AtomicBool>,
    pub now: Arc<Mutex<Option<Now>>>,
    /// What the device said of files sent to it: how many it answered for, and the last
    /// answer (whether it took it, and what it said: where it went, or why not).
    pub sent: Arc<Mutex<(u64, bool, String)>>,
}

/// What this host does, as its hello lists it. `mirror`: a device may ask for the monitor as
/// it is. `placement`: a device gets a monitor of its own, placed as it asks. `displays`: and a
/// second one, where the driver has a third monitor.
pub fn features(allowed: &Allowed) -> Vec<String> {
    let mut features: Vec<String> = ["video", "input", "cursor", "configure", "sound", "pen"].map(String::from).to_vec();
    if !allowed.real_screens.load(Ordering::Relaxed) {
        features.push("mirror".into());
    }
    if allowed.own_monitor.load(Ordering::Relaxed) {
        features.push("placement".into());
        if allowed.second.load(Ordering::Relaxed) {
            features.push("displays".into());
        }
        if allowed.hdr.load(Ordering::Relaxed) {
            features.push("hdr".into());
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

/// What this computer is called, as devices are told.
fn host_name() -> String {
    NAME.get().cloned().unwrap_or_default()
}

/// The name the host gives itself, for `status`.
pub static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn saved_path(state: &Path) -> PathBuf {
    state.join("allowed.json")
}

/// What the owner switched earlier: (files, clipboard, sharing off).
pub fn load(state: &Path) -> (bool, bool, bool) {
    let saved = std::fs::read_to_string(saved_path(state)).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let is = |key: &str| saved.as_ref().and_then(|v| v.get(key)).and_then(|v| v.as_bool()).unwrap_or(false);
    (is("files"), is("clipboard"), is("paused"))
}

fn save(state: &Path, allowed: &Allowed) -> std::io::Result<()> {
    let saved = serde_json::json!({ "files": allowed.files.load(Ordering::Relaxed), "clipboard": allowed.clipboard.load(Ordering::Relaxed), "paused": allowed.paused.load(Ordering::Relaxed) });
    std::fs::create_dir_all(state)?;
    std::fs::write(saved_path(state), saved.to_string())
}

pub fn pipe_name(port: u16) -> String {
    format!(r"\\.\pipe\remoteman-host-{port}")
}

/// The host's log file, for `log` (set at the start when there is one).
pub static LOG: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Is this process SYSTEM, started by the service? Asked once.
fn system() -> bool {
    static IS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS.get_or_init(crate::desk::is_system)
}

/// The pipe's protection: the system and Administrators, and the user: the one this process
/// runs as, whether or not that user's program is elevated (an elevated program's token makes
/// Administrators, not the user, the owner of what it creates, so Windows' default would shut
/// the user's own app out); or, for a host the service started as SYSTEM, whoever is signed in
/// at the console now (nobody but the system and Administrators while nobody is). `None`:
/// Windows' default, if the user can't be named. The descriptor is the caller's to free.
fn protection() -> Option<SECURITY_ATTRIBUTES> {
    let user = if system() { crate::desk::console_user_sid() } else { Some(crate::desk::own_sid()?) };
    let sddl = HSTRING::from(match user {
        Some(sid) => format!("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;{sid})"),
        None => "D:(A;;GA;;;SY)(A;;GA;;;BA)".to_string(),
    });
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: the text outlives the call; the descriptor is LocalAlloc'd and freed by the caller.
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, 1, &mut descriptor, None) }.ok()?;
    Some(SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor.0, bInheritHandle: false.into() })
}

/// Answer the owner's questions for as long as the host lives.
pub fn serve(port: u16, host: Weak<Host>, allowed: Allowed, state: PathBuf) {
    let name = HSTRING::from(pipe_name(port));
    let spawned = std::thread::Builder::new().name("control".into()).spawn(move || {
        let mut said = false;
        let mut warned = false;
        loop {
            // A pipe for each question: the one before is closed when it has been answered.
            // Its protection is made anew each time, as the user at the console may change.
            let protection = protection();
            if protection.is_none() && !std::mem::replace(&mut warned, true) {
                tracing::warn!("the control pipe has Windows' default protection: only an elevated prompt can speak to this host");
            }
            // SAFETY: plain call; the name and the protection outlive it.
            let pipe = unsafe { CreateNamedPipeW(&name, FILE_FLAGS_AND_ATTRIBUTES(DUPLEX), PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, PIPE_UNLIMITED_INSTANCES, 65536, 65536, 0, protection.as_ref().map(|p| p as *const _)) };
            if let Some(p) = &protection {
                // SAFETY: LocalAlloc'd by `protection`; the pipe keeps its own copy.
                let _ = unsafe { windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(p.lpSecurityDescriptor))) };
            }
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
            // A host running as SYSTEM opens a file to send with the asking user's rights, so
            // that nobody gets a file sent that they couldn't read themselves.
            let as_asker = system() && line.trim().starts_with("send ");
            // SAFETY: a connected pipe; undone below on this thread.
            if as_asker && unsafe { ImpersonateNamedPipeClient(pipe) }.is_err() {
                let _ = file.write_all(b"couldn't take on the asking user's rights to read the file\n");
                continue;
            }
            let said = match host.upgrade() {
                Some(host) => answer(&host, &allowed, &state, line.trim()),
                None => "the host is stopping\n".into(),
            };
            if as_asker {
                // SAFETY: back to this process's own rights.
                let _ = unsafe { RevertToSelf() };
            }
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
        // The end of the host's log (its last N lines, 200 unless said), for the owner's app
        // and for a prompt that isn't elevated (the log's folder is Administrators' only).
        (Some("log"), n, _) => {
            let Some(path) = LOG.get() else { return "this host keeps no log (it wasn't started with --log)\n".into() };
            let n = n.and_then(|n| n.parse::<usize>().ok()).unwrap_or(200).clamp(1, 5000);
            match std::fs::read(path) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let lines: Vec<&str> = text.lines().collect();
                    lines[lines.len().saturating_sub(n)..].iter().map(|l| format!("{l}\n")).collect()
                }
                Err(e) => format!("{}: {e}\n", path.display()),
            }
        }
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
                "paused": allowed.paused.load(Ordering::Relaxed),
                "name": host_name(),
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
        // Sharing off: every device is turned away until it is on again.
        (Some("sharing"), Some(word @ ("on" | "off")), _) => {
            let off = word == "off";
            allowed.paused.store(off, Ordering::Relaxed);
            if off {
                host.set_pairing(false);
                host.decide(false);
                host.drop_client();
            }
            tracing::info!(on = !off, "sharing switched by the owner");
            match save(state, allowed) {
                Ok(()) => "ok\n".into(),
                Err(e) => format!("done, but it won't be remembered: {e}\n"),
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
        _ => "status | pair on|off|accept|decline | allow clipboard|files on|off | drop | revoke FINGERPRINT | send PATH | sharing on|off\n".into(),
    }
}
