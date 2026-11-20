//! The app's own lines, to the file given with `--log` (and nowhere else: the app has no
//! console).

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use windows::Win32::System::SystemInformation::GetLocalTime;

static FILE: OnceLock<Option<PathBuf>> = OnceLock::new();
static GATE: Mutex<()> = Mutex::new(());

pub fn to(file: Option<PathBuf>) {
    let _ = FILE.set(file);
}

pub fn say(line: &str) {
    let Some(Some(path)) = FILE.get() else { return };
    // SAFETY: plain call.
    let t = unsafe { GetLocalTime() };
    let text = format!("{:02}:{:02}:{:02}.{:03} app: {line}\n", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds);
    let _gate = GATE.lock();
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}
