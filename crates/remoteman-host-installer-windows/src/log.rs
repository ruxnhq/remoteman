//! The installer's lines: to its own log (`%TEMP%\remoteman-host-setup.log`, or `--log`), from
//! each of its processes, and, once the install folder is there, to `setup.log` in it too, where
//! setup.ps1 writes, so that one file tells the whole install.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use windows::Win32::System::SystemInformation::GetLocalTime;

static FILE: OnceLock<PathBuf> = OnceLock::new();
static ALSO: Mutex<Option<PathBuf>> = Mutex::new(None);
static GATE: Mutex<()> = Mutex::new(());

pub fn init(file: Option<PathBuf>) {
    let _ = FILE.set(file.unwrap_or_else(default));
}

/// `%TEMP%\remoteman-host-setup.log` (`remoteman-viewer-setup.log` for the viewer's).
fn default() -> PathBuf {
    std::env::temp_dir().join(format!("{}.log", crate::product().setup_name()))
}

/// The installer's own log.
pub fn path() -> PathBuf {
    FILE.get().cloned().unwrap_or_else(default)
}

/// Also write to this file (the install folder's setup.log), or stop.
pub fn also(file: Option<PathBuf>) {
    *ALSO.lock().unwrap() = file;
}

/// The time as setup.ps1 writes it (`Get-Date -Format s`).
pub fn stamp() -> String {
    // SAFETY: plain call.
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

pub fn say(line: &str) {
    let _gate = GATE.lock();
    let pid = std::process::id();
    append(&path(), &format!("{} [{pid}] {line}\n", stamp()));
    if let Some(also) = ALSO.lock().unwrap().clone() {
        append(&also, &format!("{} setup.exe: {line}\r\n", stamp()));
    }
}

fn append(path: &Path, text: &str) {
    // Opened and closed for each line: setup.ps1 appends to the same file between them.
    for _ in 0..5 {
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(mut file) => {
                let _ = file.write_all(text.as_bytes());
                return;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
}
