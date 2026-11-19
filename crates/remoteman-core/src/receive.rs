//! Taking files the other side sends (docs/PROTOCOL.md, "Files"): each goes into one folder
//! under a cleaned-up name, never over a file that is there, and counts only once it has
//! arrived whole.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use remoteman_proto::media::{BLOB_FILE, FileHeader, safe_file_name};

use crate::blob::BlobEvent;

struct Incoming {
    /// The sender's number for the file, which the answer names.
    id: u32,
    file: std::fs::File,
    /// Where it is written while it arrives, and where it goes when whole.
    part: PathBuf,
    done: PathBuf,
    size: u64,
    got: u64,
}

fn part_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

/// Give a file that has arrived whole its name, never in place of a file that is there: a
/// plain rename would replace one that appeared under that name while this one was arriving
/// (saved by a program, or put there by its user).
pub fn settle(part: &Path, done: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(part, done) {
        Ok(()) => std::fs::remove_file(part),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        // A disk that has no links: the gap between looking and renaming is all there is.
        Err(_) if done.exists() => Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "a file of that name appeared meanwhile")),
        Err(_) => std::fs::rename(part, done),
    }
}

/// What became of a file: the sender's number for it, and where it was put or why not. It
/// is what the `file_result` message says.
pub type Outcome = (u32, Result<PathBuf, String>);

/// Files on their way in.
#[derive(Default)]
pub struct Receiver {
    files: HashMap<u32, Incoming>,
}

impl Receiver {
    /// Pass every blob event here. File blobs are taken into `folder`; an outcome comes back
    /// when one has arrived or been given up. Other blobs are left alone (`None`).
    pub fn blob(&mut self, event: &BlobEvent, folder: &Path) -> Option<Outcome> {
        match event {
            BlobEvent::Open { id, kind, header } if *kind == BLOB_FILE => {
                let h = serde_json::from_str::<FileHeader>(header).ok()?;
                let refuse = |why: String| Some((h.id, Err(why)));
                if let Err(e) = std::fs::create_dir_all(folder) {
                    return refuse(format!("{}: {e}", folder.display()));
                }
                // Never over a file that is there: "name (2).ext" and so on.
                let name = safe_file_name(&h.name);
                let (stem, ext) = match name.rsplit_once('.') {
                    Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
                    _ => (name.clone(), String::new()),
                };
                let done = (1..1000).map(|n| folder.join(if n == 1 { name.clone() } else { format!("{stem} ({n}){ext}") })).find(|p| !p.exists() && !part_of(p).exists());
                let Some(done) = done else { return refuse("too many files of that name".into()) };
                let part = part_of(&done);
                match std::fs::OpenOptions::new().write(true).create_new(true).open(&part) {
                    Ok(file) => {
                        self.files.insert(*id, Incoming { id: h.id, file, part, done, size: h.size, got: 0 });
                        None
                    }
                    Err(e) => refuse(format!("{}: {e}", part.display())),
                }
            }
            BlobEvent::Open { .. } => None,
            BlobEvent::Data { id, bytes } => {
                let f = self.files.get_mut(id)?;
                f.got += bytes.len() as u64;
                // More than it said it would be, or a disk that won't take it: give it up.
                let failed = if f.got > f.size { Some("it was longer than it said".to_owned()) } else { f.file.write_all(bytes).err().map(|e| e.to_string()) };
                let why = failed?;
                let f = self.files.remove(id)?;
                let _ = std::fs::remove_file(&f.part);
                Some((f.id, Err(why)))
            }
            BlobEvent::Close { id, complete } => {
                let f = self.files.remove(id)?;
                let whole = *complete && f.got == f.size;
                let result = if whole { f.file.sync_all().and_then(|()| settle(&f.part, &f.done)).map_err(|e| e.to_string()) } else { Err("it didn't arrive whole".to_owned()) };
                if result.is_err() {
                    let _ = std::fs::remove_file(&f.part);
                }
                Some((f.id, result.map(|()| f.done)))
            }
        }
    }
}

impl Drop for Receiver {
    /// What was still arriving is thrown away.
    fn drop(&mut self) {
        for (_, f) in self.files.drain() {
            let _ = std::fs::remove_file(&f.part);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_appeared_meanwhile_is_not_replaced() {
        let dir = std::env::temp_dir().join(format!("remoteman-receive-meanwhile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = Receiver::default();
        assert!(r.blob(&BlobEvent::Open { id: 1, kind: BLOB_FILE, header: serde_json::to_string(&FileHeader { id: 7, name: "notes.txt".into(), size: 3 }).unwrap() }, &dir).is_none());
        r.blob(&BlobEvent::Data { id: 1, bytes: b"new".to_vec() }, &dir);
        // Something else saves a file of that name while this one is on its way.
        std::fs::write(dir.join("notes.txt"), b"the user's own").unwrap();
        let (id, result) = r.blob(&BlobEvent::Close { id: 1, complete: true }, &dir).unwrap();
        assert_eq!(id, 7);
        assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"the user's own", "a file that was there was replaced");
        assert!(result.is_err());
        assert!(!dir.join("notes.txt.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn open(id: u32, name: &str, size: u64) -> BlobEvent {
        BlobEvent::Open { id, kind: BLOB_FILE, header: serde_json::to_string(&FileHeader { id: id + 100, name: name.into(), size }).unwrap() }
    }

    #[test]
    fn files_arrive_whole_under_safe_names_and_never_over_another() {
        let dir = std::env::temp_dir().join(format!("remoteman-receive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = Receiver::default();
        // Whole, in two pieces; the name loses its folders.
        assert!(r.blob(&open(1, "../../etc/notes.txt", 6), &dir).is_none());
        assert!(r.blob(&BlobEvent::Data { id: 1, bytes: b"abc".to_vec() }, &dir).is_none());
        assert!(r.blob(&BlobEvent::Data { id: 1, bytes: b"def".to_vec() }, &dir).is_none());
        let (id, at) = r.blob(&BlobEvent::Close { id: 1, complete: true }, &dir).unwrap();
        assert_eq!((id, at.clone().unwrap()), (101, dir.join("notes.txt")));
        assert_eq!(std::fs::read(at.unwrap()).unwrap(), b"abcdef");
        // The same name again goes beside it.
        r.blob(&open(2, "notes.txt", 1), &dir);
        r.blob(&BlobEvent::Data { id: 2, bytes: b"x".to_vec() }, &dir);
        assert_eq!(r.blob(&BlobEvent::Close { id: 2, complete: true }, &dir).unwrap().1.unwrap(), dir.join("notes (2).txt"));
        // Given up by the sender, short, and too long: nothing is left behind.
        r.blob(&open(3, "a.bin", 4), &dir);
        r.blob(&BlobEvent::Data { id: 3, bytes: b"ab".to_vec() }, &dir);
        assert!(r.blob(&BlobEvent::Close { id: 3, complete: false }, &dir).unwrap().1.is_err());
        r.blob(&open(4, "b.bin", 4), &dir);
        assert!(r.blob(&BlobEvent::Close { id: 4, complete: true }, &dir).unwrap().1.is_err());
        r.blob(&open(5, "c.bin", 1), &dir);
        assert!(r.blob(&BlobEvent::Data { id: 5, bytes: b"toolong".to_vec() }, &dir).unwrap().1.is_err());
        // Still arriving when the receiver goes: thrown away.
        r.blob(&open(6, "d.bin", 9), &dir);
        drop(r);
        let mut left: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["notes (2).txt", "notes.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
