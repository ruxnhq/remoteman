//! Files from a device: written into the user's Downloads folder under a name of this host's
//! choosing, and answered with where each went. As the Linux host does it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use remoteman_core::host::Link;
use remoteman_core::media::{FileHeader, safe_file_name};
use remoteman_core::{BlobEvent, Message};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

/// A file on its way in.
struct Incoming {
    /// The sender's number for it, to answer with.
    id: u32,
    file: std::fs::File,
    /// Written under this name first, so a half-arrived file is never taken for a whole one.
    part: PathBuf,
    done: PathBuf,
    size: u64,
    got: u64,
}

/// The files one device is sending.
#[derive(Default)]
pub struct Files {
    incoming: BTreeMap<u32, Incoming>,
}

/// The name a file is written under until it is whole.
fn part_of(done: &Path) -> PathBuf {
    let mut name = done.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    done.with_file_name(name)
}

/// The user's Downloads folder, as Windows names it: for a host the service started as SYSTEM,
/// the one of whoever is signed in at the console (`None` while nobody is: not SYSTEM's own).
fn downloads() -> Option<PathBuf> {
    let user = if crate::desk::is_system() { Some(crate::desk::console_user()?) } else { None };
    // SAFETY: the path comes back in memory of the shell's, which is freed here; the token
    // lives until after the call.
    let known = unsafe {
        SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, user.as_ref().map(|u| u.0)).ok().and_then(|path| {
            let text = path.to_string().ok();
            CoTaskMemFree(Some(path.0 as *const _));
            text
        })
    };
    let known = known.map(PathBuf::from).filter(|p| p.is_absolute());
    // (USERPROFILE is SYSTEM's own for a host the service started: never that.)
    if user.is_some() { known } else { Some(known.unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Downloads"))) }
}

impl Files {
    /// Is this blob one of the files being received?
    pub fn has(&self, id: u32) -> bool {
        self.incoming.contains_key(&id)
    }

    /// A file blob's next event. `allowed`: the owner lets devices put files here.
    pub fn event(&mut self, event: &BlobEvent, allowed: bool, link: &Link) {
        match event {
            BlobEvent::Open { id, header, .. } => {
                let Ok(h) = serde_json::from_str::<FileHeader>(header) else { return };
                let refuse = |why: &str| link.send_message(Message::FileResult { id: h.id, ok: false, message: why.into() });
                if !allowed {
                    return refuse("this computer doesn't take files from this device");
                }
                let Some(folder) = downloads() else {
                    return refuse("nobody is signed in on this computer to take the file");
                };
                if let Err(e) = std::fs::create_dir_all(&folder) {
                    return refuse(&format!("{}: {e}", folder.display()));
                }
                // Never over a file that is there: "name (2).ext" and so on.
                let name = safe_file_name(&h.name);
                let (stem, ext) = match name.rsplit_once('.') {
                    Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
                    _ => (name.clone(), String::new()),
                };
                let done = (1..1000).map(|n| folder.join(if n == 1 { name.clone() } else { format!("{stem} ({n}){ext}") })).find(|p| !p.exists() && !part_of(p).exists());
                let Some(done) = done else { return refuse("too many files of that name") };
                let part = part_of(&done);
                match std::fs::OpenOptions::new().write(true).create_new(true).open(&part) {
                    Ok(file) => {
                        self.incoming.insert(*id, Incoming { id: h.id, file, part, done, size: h.size, got: 0 });
                    }
                    Err(e) => refuse(&format!("{}: {e}", part.display())),
                }
            }
            BlobEvent::Data { id, bytes } => {
                let Some(f) = self.incoming.get_mut(id) else { return };
                f.got += bytes.len() as u64;
                // More than it said it would be, or a disk that won't take it: give it up.
                let failed = if f.got > f.size { Some("it was longer than it said".to_owned()) } else { std::io::Write::write_all(&mut f.file, bytes).err().map(|e| e.to_string()) };
                if let Some(why) = failed {
                    let f = self.incoming.remove(id).unwrap();
                    let (part, id) = (f.part.clone(), f.id);
                    drop(f);
                    let _ = std::fs::remove_file(&part);
                    link.send_message(Message::FileResult { id, ok: false, message: why });
                }
            }
            BlobEvent::Close { id, complete } => {
                let Some(f) = self.incoming.remove(id) else { return };
                let whole = *complete && f.got == f.size;
                let Incoming { id, file, part, done, got, .. } = f;
                let synced = if whole { file.sync_all().map_err(|e| e.to_string()) } else { Err("it didn't arrive whole".to_owned()) };
                // Windows renames no file that is open.
                drop(file);
                match synced.and_then(|()| std::fs::rename(&part, &done).map_err(|e| e.to_string())) {
                    Ok(()) => {
                        tracing::info!(file = %done.display(), bytes = got, "a file from the device");
                        link.send_message(Message::FileResult { id, ok: true, message: done.display().to_string() });
                    }
                    Err(why) => {
                        let _ = std::fs::remove_file(&part);
                        link.send_message(Message::FileResult { id, ok: false, message: why });
                    }
                }
            }
        }
    }
}

impl Drop for Files {
    /// Files half received are thrown away.
    fn drop(&mut self) {
        for (_, f) in std::mem::take(&mut self.incoming) {
            let part = f.part.clone();
            drop(f);
            let _ = std::fs::remove_file(&part);
        }
    }
}
