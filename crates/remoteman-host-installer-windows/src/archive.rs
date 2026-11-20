//! The files the installer carries, appended to its own exe:
//!
//!     [the exe] [stream 0] [stream 1] [index] [padding] [trailer]
//!
//! Each file's content is a blob, kept once however many paths have it (the Windows App Runtime
//! the pages need is the same as the one beside Remoteman Host in `app\`, so it is carried
//! once). Stream 0 has the blobs the pages need (the runtime), stream 1 the rest; each is one XZ
//! stream (LZMA2, preset 9, behind the x86 branch filter), its blobs one after the other, ordered
//! by file name so that alike files (a DLL's strings in each language) sit together. The index
//! (JSON) lists the streams with their blobs (size, SHA-256) and the two sets of paths: `ui`,
//! the runtime the pages are unpacked with, and `payload`, the install folder's files. Every
//! file unpacked is checked against its SHA-256 as it is written.
//!
//! The trailer is the last 32 bytes before a code signature, if the file is ever signed (a
//! signature is appended after everything, and the padding keeps the end 8-byte aligned so
//! signing adds nothing before it): "RMSETUP1", the index's offset and length, and the length
//! of the bare exe. The uninstaller is the exe, stream 0 and an index with the `ui` set only.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use lzma_rust2::{FilterType, XzOptions, XzReader, XzWriter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"RMSETUP1";
const TRAILER: u64 = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Index {
    pub version: String,
    pub streams: Vec<Stream>,
    pub ui: Vec<Entry>,
    pub payload: Vec<Entry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stream {
    /// From the start of the file.
    pub offset: u64,
    pub packed: u64,
    pub blobs: Vec<Blob>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Blob {
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// Relative, with backslashes.
    pub path: String,
    pub sha256: String,
}

pub struct Archive {
    pub file: PathBuf,
    pub exe_len: u64,
    pub index: Index,
}

/// A file unpacked: where, how big, and its SHA-256.
#[derive(Clone, Debug)]
pub struct Unpacked {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

fn damaged(what: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("the setup file is damaged or incomplete ({what}); download it again"))
}

impl Archive {
    pub fn open(path: &Path) -> io::Result<Archive> {
        let mut file = File::open(path)?;
        let end = archive_end(&mut file)?;
        if end < TRAILER {
            return Err(damaged("too short"));
        }
        file.seek(SeekFrom::Start(end - TRAILER))?;
        let mut trailer = [0u8; TRAILER as usize];
        file.read_exact(&mut trailer)?;
        if &trailer[..8] != MAGIC {
            return Err(damaged("no archive"));
        }
        let word = |i: usize| u64::from_le_bytes(trailer[i..i + 8].try_into().unwrap());
        let (offset, len, exe_len) = (word(8), word(16), word(24));
        if offset + len > end || exe_len > offset || len > 64 << 20 {
            return Err(damaged("bad trailer"));
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut json = vec![0u8; len as usize];
        file.read_exact(&mut json)?;
        let index: Index = serde_json::from_slice(&json).map_err(|e| damaged(format!("index: {e}")))?;
        Ok(Archive { file: path.to_path_buf(), exe_len, index })
    }

    /// Whether the files to install have Remoteman Host (`app\`).
    pub fn has_app(&self) -> bool {
        self.index.payload.iter().any(|e| e.path.eq_ignore_ascii_case(r"app\remoteman-host-app.exe"))
    }

    /// The bare exe, as `out`.
    pub fn write_exe(&self, out: &Path) -> io::Result<()> {
        let mut from = File::open(&self.file)?.take(self.exe_len);
        let mut to = BufWriter::new(File::create(out)?);
        io::copy(&mut from, &mut to)?;
        to.flush()
    }

    /// The uninstaller: the exe, the stream with the runtime, and an index with only that.
    pub fn write_uninstaller(&self, out: &Path) -> io::Result<()> {
        let mut source = File::open(&self.file)?;
        let mut to = BufWriter::new(File::create(out)?);
        io::copy(&mut (&mut source).take(self.exe_len), &mut to)?;
        let needed: std::collections::HashSet<&str> = self.index.ui.iter().map(|e| e.sha256.as_str()).collect();
        let mut streams = Vec::new();
        let mut at = self.exe_len;
        for stream in &self.index.streams {
            if !stream.blobs.iter().any(|b| needed.contains(b.sha256.as_str())) {
                continue;
            }
            source.seek(SeekFrom::Start(stream.offset))?;
            let copied = io::copy(&mut (&mut source).take(stream.packed), &mut to)?;
            if copied != stream.packed {
                return Err(damaged("a stream is cut short"));
            }
            streams.push(Stream { offset: at, packed: stream.packed, blobs: stream.blobs.clone() });
            at += stream.packed;
        }
        let index = Index { version: self.index.version.clone(), streams, ui: self.index.ui.clone(), payload: Vec::new() };
        finish(&mut to, at, &index, self.exe_len)?;
        to.flush()
    }

    /// Unpacks `entries` under `dest`, checking each against its SHA-256. `each` hears each
    /// file's path as it is written.
    pub fn unpack(&self, entries: &[Entry], dest: &Path, each: &mut dyn FnMut(&str)) -> io::Result<Vec<Unpacked>> {
        // Where each wanted blob goes.
        let mut wanted: HashMap<&str, Vec<&Entry>> = HashMap::new();
        for e in entries {
            wanted.entry(e.sha256.as_str()).or_default().push(e);
        }
        let mut done = Vec::new();
        let mut source = File::open(&self.file)?;
        for stream in &self.index.streams {
            let last = stream.blobs.iter().rposition(|b| wanted.contains_key(b.sha256.as_str()));
            let Some(last) = last else { continue };
            source.seek(SeekFrom::Start(stream.offset))?;
            let packed = BufReader::with_capacity(1 << 20, (&mut source).take(stream.packed));
            let mut xz = XzReader::new(packed, false);
            for blob in &stream.blobs[..=last] {
                let mut part = (&mut xz).take(blob.size);
                match wanted.get(blob.sha256.as_str()) {
                    None => {
                        let skipped = io::copy(&mut part, &mut io::sink())?;
                        if skipped != blob.size {
                            return Err(damaged("a stream is cut short"));
                        }
                    }
                    Some(paths) => {
                        let first = dest.join(&paths[0].path);
                        each(&paths[0].path);
                        let (size, sha) = write_checked(&mut part, &first)?;
                        if size != blob.size || sha != blob.sha256 {
                            let _ = std::fs::remove_file(&first);
                            return Err(damaged(format!("{} does not match", paths[0].path)));
                        }
                        done.push(Unpacked { path: first.clone(), size, sha256: sha.clone() });
                        for other in &paths[1..] {
                            let to = dest.join(&other.path);
                            each(&other.path);
                            if let Some(parent) = to.parent() {
                                std::fs::create_dir_all(parent)?;
                            }
                            retry(|| std::fs::copy(&first, &to).map(|_| ()), &to)?;
                            done.push(Unpacked { path: to, size, sha256: sha.clone() });
                        }
                    }
                }
            }
        }
        if done.len() != entries.len() {
            return Err(damaged(format!("{} of {} files found", done.len(), entries.len())));
        }
        Ok(done)
    }
}

/// Writes `from` to `path` (making its folder), returning its size and SHA-256.
fn write_checked(from: &mut dyn Read, path: &Path) -> io::Result<(u64, String)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = retry(|| File::create(path), path)?;
    let mut to = BufWriter::with_capacity(1 << 20, file);
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let n = from.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        to.write_all(&buffer[..n]).map_err(|e| in_path(e, path))?;
        size += n as u64;
    }
    to.flush().map_err(|e| in_path(e, path))?;
    Ok((size, hex(&hash.finalize())))
}

/// A file that is held open for a moment (an antivirus scanning it) is tried again for a few
/// seconds before the error is given, naming the file.
fn retry<T>(mut f: impl FnMut() -> io::Result<T>, path: &Path) -> io::Result<T> {
    let mut tries = 0;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if tries < 15 && matches!(e.raw_os_error(), Some(5 | 32 | 33)) => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => return Err(in_path(e, path)),
        }
    }
}

fn in_path(e: io::Error, path: &Path) -> io::Error {
    let what = match e.raw_os_error() {
        Some(32 | 33) => "it is in use by another program".to_string(),
        Some(5) => "access is denied".to_string(),
        _ => e.to_string(),
    };
    io::Error::new(e.kind(), format!("{}: {what}", path.display()))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A file's size and SHA-256, read back from the disk.
pub fn hash_file(path: &Path) -> io::Result<(u64, String)> {
    let mut file = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut hash = Sha256::new();
    let size = io::copy(&mut file, &mut HashWriter(&mut hash))?;
    Ok((size, hex(&hash.finalize())))
}

struct HashWriter<'a>(&'a mut Sha256);
impl Write for HashWriter<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.update(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Where the archive ends: the end of the file, or where a code signature appended to it starts.
fn archive_end(file: &mut File) -> io::Result<u64> {
    let len = file.metadata()?.len();
    let mut head = vec![0u8; 4096.min(len as usize)];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut head)?;
    let u16_at = |i: usize| head.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |i: usize| head.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let Some(pe) = u32_at(0x3c).map(|v| v as usize) else { return Ok(len) };
    if head.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Ok(len);
    }
    let optional = pe + 24;
    let directories = match u16_at(optional) {
        Some(0x20b) => optional + 112,
        Some(0x10b) => optional + 96,
        _ => return Ok(len),
    };
    // The fifth directory: the certificate table, a file offset and size.
    let (Some(offset), Some(size)) = (u32_at(directories + 4 * 8), u32_at(directories + 4 * 8 + 4)) else { return Ok(len) };
    if offset != 0 && size != 0 && offset as u64 + size as u64 == len {
        return Ok(offset as u64);
    }
    Ok(len)
}

/// The index, the padding and the trailer, after the streams (written up to `at`).
fn finish(to: &mut dyn Write, at: u64, index: &Index, exe_len: u64) -> io::Result<()> {
    let json = serde_json::to_vec(index).map_err(io::Error::other)?;
    to.write_all(&json)?;
    let end = at + json.len() as u64;
    let pad = (8 - (end + TRAILER) % 8) % 8;
    to.write_all(&vec![0u8; pad as usize])?;
    to.write_all(MAGIC)?;
    to.write_all(&at.to_le_bytes())?;
    to.write_all(&(json.len() as u64).to_le_bytes())?;
    to.write_all(&exe_len.to_le_bytes())?;
    Ok(())
}

// ---- Packing (the build) ----

/// Every file under `root`, relative, with backslashes.
fn files_under(root: &Path) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('/', "\\");
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(out)
}

pub struct PackReport {
    pub exe: u64,
    /// Per stream: files, bytes before, bytes packed.
    pub streams: Vec<(usize, u64, u64)>,
    pub total: u64,
    pub ui_files: usize,
    pub payload_files: usize,
    pub payload_bytes: u64,
}

/// Packs `runtime` (the pages' Windows App Runtime) and `payload` (the install folder's files)
/// onto `exe`, as `out`.
/// A blob to pack: its size, a file that has it, and that file's name.
type Source = (u64, PathBuf, String);

pub fn pack(exe: &Path, runtime: &Path, payload: &Path, version: &str, out: &Path) -> io::Result<PackReport> {
    let mut blobs: BTreeMap<String, Source> = BTreeMap::new(); // by SHA-256
    let mut collect = |root: &Path| -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for rel in files_under(root)? {
            let path = root.join(&rel);
            let (size, sha) = hash_file(&path)?;
            let name = rel.rsplit('\\').next().unwrap_or(&rel).to_ascii_lowercase();
            blobs.entry(sha.clone()).or_insert((size, path, name));
            entries.push(Entry { path: rel, sha256: sha });
        }
        Ok(entries)
    };
    let ui = collect(runtime)?;
    let payload_entries = collect(payload)?;
    if !payload_entries.iter().any(|e| e.path.eq_ignore_ascii_case("setup.ps1")) {
        return Err(io::Error::other(format!("{} has no setup.ps1", payload.display())));
    }
    let in_ui: std::collections::HashSet<&str> = ui.iter().map(|e| e.sha256.as_str()).collect();
    // Alike files together: by name, then by where they come from.
    let mut order: Vec<(&String, &Source)> = blobs.iter().collect();
    order.sort_by(|a, b| (&a.1.2, &a.1.1).cmp(&(&b.1.2, &b.1.1)));
    let groups: [Vec<(&String, &Source)>; 2] = [
        order.iter().filter(|(sha, _)| in_ui.contains(sha.as_str())).cloned().collect(),
        order.iter().filter(|(sha, _)| !in_ui.contains(sha.as_str())).cloned().collect(),
    ];

    let exe_bytes = std::fs::read(exe)?;
    // An exe that already carries an archive is cut back to the bare exe.
    let exe_len = match Archive::open(exe) {
        Ok(a) => a.exe_len,
        Err(_) => exe_bytes.len() as u64,
    };
    let mut to = BufWriter::new(File::create(out)?);
    to.write_all(&exe_bytes[..exe_len as usize])?;
    let mut at = exe_len;
    let mut streams = Vec::new();
    let mut report = Vec::new();
    for group in groups {
        if group.is_empty() {
            continue;
        }
        let total: u64 = group.iter().map(|(_, b)| b.0).sum();
        let mut options = XzOptions::with_preset(9);
        options.prepend_pre_filter(FilterType::BcjX86, 0);
        let mut counter = Counter { inner: &mut to, count: 0 };
        let mut xz = XzWriter::new(&mut counter, options)?;
        let mut list = Vec::new();
        for (sha, (size, path, _)) in &group {
            let copied = io::copy(&mut BufReader::new(File::open(path)?), &mut xz)?;
            if copied != *size {
                return Err(io::Error::other(format!("{} changed while it was packed", path.display())));
            }
            list.push(Blob { sha256: (*sha).clone(), size: *size });
        }
        xz.finish()?;
        let packed = counter.count;
        streams.push(Stream { offset: at, packed, blobs: list });
        report.push((group.len(), total, packed));
        at += packed;
    }
    let index = Index { version: version.to_string(), streams, ui: ui.clone(), payload: payload_entries.clone() };
    finish(&mut to, at, &index, exe_len)?;
    to.flush()?;
    drop(to);
    // Read back, as the installer will.
    let check = Archive::open(out)?;
    if check.index.payload.len() != payload_entries.len() {
        return Err(io::Error::other("the packed file doesn't read back"));
    }
    let payload_bytes = payload_entries.iter().map(|e| blobs[&e.sha256].0).sum();
    Ok(PackReport {
        exe: exe_len,
        streams: report,
        total: std::fs::metadata(out)?.len(),
        ui_files: ui.len(),
        payload_files: payload_entries.len(),
        payload_bytes,
    })
}

struct Counter<'a, W: Write> {
    inner: &'a mut W,
    count: u64,
}
impl<W: Write> Write for Counter<'_, W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(b)?;
        self.count += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// `--pack`: the build's command. Prints what went where.
pub fn pack_command(args: &crate::Args) -> i32 {
    crate::system::attach_console();
    let (Some(runtime), Some(payload), Some(version), Some(out)) = (&args.runtime, &args.payload, &args.version, &args.out) else {
        println!("usage: remoteman-host-setup --pack --runtime DIR --payload DIR --version V --out FILE");
        return 2;
    };
    let exe = std::env::current_exe().expect("this exe");
    let started = std::time::Instant::now();
    match pack(&exe, runtime, payload, version, out) {
        Ok(r) => {
            let mb = |b: u64| b as f64 / 1_048_576.0;
            println!("packed {} in {:.0} s: {:.1} MB ({} bytes)", out.display(), started.elapsed().as_secs_f64(), mb(r.total), r.total);
            println!("  the program: {:.1} MB", mb(r.exe));
            let names = ["the Windows App Runtime (the pages', and the app's, carried once)", "the rest of the files to install"];
            for (i, (files, bytes, packed)) in r.streams.iter().enumerate() {
                println!("  {}: {files} files, {:.1} MB packed into {:.1} MB", names.get(i).unwrap_or(&"more"), mb(*bytes), mb(*packed));
            }
            println!("  {} runtime files for the pages; {} files to install, {:.1} MB", r.ui_files, r.payload_files, mb(r.payload_bytes));
            0
        }
        Err(e) => {
            println!("packing failed: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(root: &Path, files: &[(&str, &[u8])]) {
        for (path, bytes) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
    }

    #[test]
    fn packs_unpacks_and_carries_shared_files_once() {
        let dir = std::env::temp_dir().join(format!("rm-archive-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let shared: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        tree(&dir.join("runtime"), &[("a.dll", &shared), (r"en-us\a.dll.mui", b"strings")]);
        tree(&dir.join("payload"), &[("setup.ps1", b"param()"), ("host.exe", b"host"), (r"app\a.dll", &shared), (r"app\remoteman-host-app.exe", b"app")]);
        tree(&dir, &[("stub.exe", b"MZ not really an exe")]);
        let out = dir.join("setup.exe");
        let report = pack(&dir.join("stub.exe"), &dir.join("runtime"), &dir.join("payload"), "9.9.9", &out).unwrap();
        // The shared DLL is in the runtime's stream only.
        assert_eq!(report.streams.len(), 2);
        assert_eq!(report.streams[0].0, 2);
        assert_eq!(report.streams[1].0, 3);
        assert_eq!(std::fs::metadata(&out).unwrap().len() % 8, 0);

        let archive = Archive::open(&out).unwrap();
        assert_eq!(archive.index.version, "9.9.9");
        assert!(archive.has_app());
        assert_eq!(archive.exe_len, b"MZ not really an exe".len() as u64);
        let unpacked = archive.unpack(&archive.index.payload, &dir.join("into"), &mut |_| {}).unwrap();
        assert_eq!(unpacked.len(), 4);
        assert_eq!(std::fs::read(dir.join(r"into\app\a.dll")).unwrap(), shared);
        assert_eq!(std::fs::read(dir.join(r"into\setup.ps1")).unwrap(), b"param()");

        let uninstaller = dir.join("uninstall.exe");
        archive.write_uninstaller(&uninstaller).unwrap();
        let small = Archive::open(&uninstaller).unwrap();
        assert!(small.index.payload.is_empty());
        assert_eq!(small.index.streams.len(), 1);
        small.unpack(&small.index.ui, &dir.join("ui"), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(dir.join(r"ui\en-us\a.dll.mui")).unwrap(), b"strings");
        archive.write_exe(&dir.join("bare.exe")).unwrap();
        assert_eq!(std::fs::read(dir.join("bare.exe")).unwrap(), b"MZ not really an exe");

        // A byte changed in a stream is found.
        let mut bytes = std::fs::read(&out).unwrap();
        let at = archive.index.streams[1].offset as usize + 40;
        bytes[at] ^= 0x55;
        std::fs::write(&out, &bytes).unwrap();
        let damaged = Archive::open(&out).unwrap();
        assert!(damaged.unpack(&damaged.index.payload, &dir.join("into2"), &mut |_| {}).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
