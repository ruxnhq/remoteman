//! What the viewer says: on standard output, and in a file when `--log` names one (a program
//! started in the desktop session by a scheduled task has no output anyone can read).

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn to_file(path: &Path) -> std::io::Result<()> {
    *FILE.lock().unwrap() = Some(File::create(path)?);
    Ok(())
}

pub fn line(text: &str) {
    println!("{text}");
    if let Some(file) = FILE.lock().unwrap().as_mut() {
        let _ = writeln!(file, "{text}");
        let _ = file.flush();
    }
}

macro_rules! say {
    ($($arg:tt)*) => { $crate::log::line(&format!($($arg)*)) };
}
