//! VA-API, for AMD and Intel GPUs on Linux: the compositor's DMA-BUF is imported as a VA surface,
//! the driver's video processor converts it to BT.709 limited-range Y'CbCr 4:2:0 (8 or 10 bits)
//! in one pass, and FFmpeg's VA-API encoder (HEVC, or AV1 where the GPU encodes it) takes it
//! with no B-frames, no lookahead and one frame in flight (by way of the C wrapper in `shim/`).
//!
//! What it can't do that NVENC can: drop references to chosen frames, or keep long-term ones
//! (FFmpeg's VA-API encoders don't offer it, and the Radeon 610M's HEVC encoder keeps only one
//! reference anyway), so a loss is answered with a keyframe; and a new bitrate opens the encoder
//! afresh (FFmpeg hands the driver its rate settings with keyframes only), so it costs one.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};

use crate::Codec;

#[repr(C)]
struct Cfg {
    codec: u32,
    depth: u32,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    slots: u32,
}

unsafe extern "C" {
    fn rm_va_probe(node: *const c_char, caps: *mut u32, vendor: *mut c_char, vendorlen: c_int) -> c_int;
    fn rm_va_open(node: *const c_char, cfg: *const Cfg, err: *mut c_char, errlen: c_int) -> *mut c_void;
    fn rm_va_error(r: *mut c_void) -> *const c_char;
    fn rm_va_close(r: *mut c_void);
    fn rm_va_import(r: *mut c_void, fd: c_int, modifier: u64, offset: u32, stride: u32, surface: *mut u32) -> c_int;
    fn rm_va_forget(r: *mut c_void, surface: u32);
    fn rm_va_convert(r: *mut c_void, src: u32, slot: u32) -> c_int;
    fn rm_va_fill(r: *mut c_void, slot: u32, bgrx: *const u8, stride: u32) -> c_int;
    fn rm_va_encode(r: *mut c_void, slot: u32, ts: i64, idr: c_int, data: *mut *const u8, size: *mut u32, key: *mut c_int) -> c_int;
    fn rm_va_set_bitrate(r: *mut c_void, bitrate: u32) -> c_int;
}

/// What a GPU encodes through VA-API.
#[derive(Debug, Clone, Default)]
pub struct Caps {
    pub hevc8: bool,
    pub hevc10: bool,
    pub av1_8: bool,
    pub av1_10: bool,
    /// It has a video processor, which the conversion needs.
    pub convert: bool,
    /// The driver's own description of itself.
    pub vendor: String,
}

impl Caps {
    /// Whether it encodes `codec` at `depth` bits (4:2:0; VA-API has no 4:4:4 encoding on AMD,
    /// and none is asked of it here).
    pub fn encodes(&self, codec: Codec, depth: u8) -> bool {
        self.convert
            && match (codec, depth) {
                (Codec::Hevc, 8) => self.hevc8,
                (Codec::Hevc, 10) => self.hevc10,
                (Codec::Av1, 8) => self.av1_8,
                (Codec::Av1, 10) => self.av1_10,
                _ => false,
            }
    }
}

/// What the GPU behind the render node `node` encodes, or `None` if it has no VA-API.
pub fn probe(node: &Path) -> Option<Caps> {
    let path = CString::new(node.as_os_str().as_encoded_bytes()).ok()?;
    let (mut caps, mut vendor) = (0u32, [0 as c_char; 256]);
    // SAFETY: a valid C string and writable buffers of the given sizes.
    if unsafe { rm_va_probe(path.as_ptr(), &mut caps, vendor.as_mut_ptr(), 256) } != 0 {
        return None;
    }
    Some(Caps {
        hevc8: caps & 1 != 0,
        hevc10: caps & 2 != 0,
        av1_8: caps & 4 != 0,
        av1_10: caps & 8 != 0,
        convert: caps & 16 != 0,
        vendor: text(&vendor),
    })
}

/// This computer's DRM render nodes, in order.
pub fn render_nodes() -> Vec<PathBuf> {
    let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri").into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("renderD"))).collect();
    nodes.sort();
    nodes
}

fn text(buf: &[c_char]) -> String {
    // SAFETY: the wrapper always terminates what it writes, and the buffer starts zeroed.
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

pub struct Config {
    pub codec: Codec,
    /// 8 or 10.
    pub depth: u8,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    /// Pictures the converter writes into and the encoder reads from (at most 8).
    pub slots: usize,
}

struct Raw(*mut c_void);

// SAFETY: the wrapper's two halves are used from one thread each (conversion on the capture
// thread, encoding on the encoder thread); what they share is the VA display, which libva
// guards itself.
unsafe impl Send for Raw {}
unsafe impl Sync for Raw {}

impl Raw {
    fn error(&self) -> String {
        // SAFETY: a live wrapper; the string is its own buffer.
        unsafe { CStr::from_ptr(rm_va_error(self.0)) }.to_string_lossy().into_owned()
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        // SAFETY: ours; both halves are gone.
        unsafe { rm_va_close(self.0) };
    }
}

/// Open the GPU behind `node`: returns the converter half (for the capture thread) and the
/// encoder half (for the encoder thread).
pub fn open(node: &Path, cfg: &Config) -> Result<(Converter, Encoder)> {
    let path = CString::new(node.as_os_str().as_encoded_bytes())?;
    let c = Cfg { codec: (cfg.codec == Codec::Av1) as u32, depth: cfg.depth as u32, width: cfg.width, height: cfg.height, fps: cfg.fps, bitrate: cfg.bitrate, slots: cfg.slots as u32 };
    let mut err = [0 as c_char; 256];
    // SAFETY: the wrapper's contract; `err` is a writable buffer of the given length.
    let raw = unsafe { rm_va_open(path.as_ptr(), &c, err.as_mut_ptr(), 256) };
    if raw.is_null() {
        bail!("{}", text(&err));
    }
    let raw = Arc::new(Raw(raw));
    let note = text(&err);
    Ok((Converter { raw: raw.clone(), surfaces: HashMap::new(), width: cfg.width, height: cfg.height }, Encoder { raw, note }))
}

/// The capture thread's half: the compositor's buffers, imported once each, converted into
/// the encoder's pictures.
pub struct Converter {
    raw: Arc<Raw>,
    /// The VA surface made for each of the compositor's buffers, by descriptor.
    surfaces: HashMap<i32, u32>,
    pub width: u32,
    pub height: u32,
}

impl Converter {
    /// Convert the DMA-BUF `fd` into slot `slot`, returning once the GPU has finished.
    pub fn convert(&mut self, fd: i32, modifier: u64, offset: u32, stride: u32, slot: usize) -> Result<()> {
        let surface = match self.surfaces.get(&fd) {
            Some(&s) => s,
            None => {
                let mut s = 0u32;
                // SAFETY: a live wrapper; `fd` is a DMA-BUF the caller keeps open meanwhile.
                if unsafe { rm_va_import(self.raw.0, fd, modifier, offset, stride, &mut s) } != 0 {
                    bail!("{}", self.raw.error());
                }
                self.surfaces.insert(fd, s);
                s
            }
        };
        // SAFETY: a live wrapper and a surface it made.
        if unsafe { rm_va_convert(self.raw.0, surface, slot as u32) } != 0 {
            bail!("{}", self.raw.error());
        }
        Ok(())
    }

    /// Forget imported buffers (the stream renegotiated, so its buffers are new).
    pub fn reset_images(&mut self) {
        for (_, s) in self.surfaces.drain() {
            // SAFETY: a surface the wrapper made and nothing is converting from.
            unsafe { rm_va_forget(self.raw.0, s) };
        }
    }

    /// For tests: a picture from the CPU (BGRx rows `stride` bytes apart) through the same
    /// conversion into slot `slot`.
    pub fn fill(&mut self, slot: usize, bgrx: &[u8], stride: u32) -> Result<()> {
        anyhow::ensure!(bgrx.len() >= stride as usize * self.height as usize, "picture too small");
        // SAFETY: a live wrapper and a buffer at least as large as it reads.
        if unsafe { rm_va_fill(self.raw.0, slot as u32, bgrx.as_ptr(), stride) } != 0 {
            bail!("{}", self.raw.error());
        }
        Ok(())
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        self.reset_images();
    }
}

/// The encoder thread's half.
pub struct Encoder {
    raw: Arc<Raw>,
    /// What the encoder said about itself when it opened.
    pub note: String,
}

impl Encoder {
    /// Encode slot `slot`; returns the bitstream and whether it is a keyframe.
    pub fn encode(&mut self, slot: usize, timestamp: u64, idr: bool) -> Result<(&[u8], bool)> {
        let (mut data, mut size, mut key) = (std::ptr::null(), 0u32, 0);
        // SAFETY: the bytes are the wrapper's packet, valid until its next call, which the
        // borrow of `self` enforces.
        unsafe {
            if rm_va_encode(self.raw.0, slot as u32, timestamp as i64, idr as c_int, &mut data, &mut size, &mut key) != 0 {
                bail!("{}", self.raw.error());
            }
            Ok((if size == 0 { &[] } else { std::slice::from_raw_parts(data, size as usize) }, key != 0))
        }
    }

    /// Encode at this many bits a second from the next frame on, which is then a keyframe.
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        // SAFETY: a live wrapper.
        if unsafe { rm_va_set_bitrate(self.raw.0, bitrate) } != 0 {
            bail!("{}", self.raw.error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first render node whose GPU encodes HEVC through VA-API, if this computer has one.
    fn gpu() -> Option<(PathBuf, Caps)> {
        let found = render_nodes().into_iter().filter_map(|n| probe(&n).map(|c| (n, c))).find(|(_, c)| c.encodes(Codec::Hevc, 10) || c.encodes(Codec::Hevc, 8));
        if found.is_none() {
            eprintln!("no GPU here encodes HEVC through VA-API: nothing tested");
        }
        found
    }

    /// Bars of colour, moved along by `t` pixels.
    fn picture(w: u32, h: u32, t: u32) -> Vec<u8> {
        let bars = [[255u8, 255, 255], [0, 255, 255], [255, 255, 0], [0, 255, 0], [255, 0, 255], [0, 0, 255], [255, 0, 0], [0, 0, 0]];
        let mut p = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = bars[(((x + t) * 8 / w) % 8) as usize];
                let i = ((y * w + x) * 4) as usize;
                p[i..i + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        p
    }

    /// The NAL unit types in an HEVC packet (Annex B).
    fn nal_types(data: &[u8]) -> Vec<u8> {
        let mut types = Vec::new();
        let mut i = 0;
        while i + 3 < data.len() {
            if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                types.push((data[i + 3] >> 1) & 0x3f);
                i += 3;
            } else {
                i += 1;
            }
        }
        types
    }

    #[test]
    fn keyframes_first_on_request_and_after_a_new_bitrate() {
        let Some((node, caps)) = gpu() else { return };
        for depth in [8, 10].into_iter().filter(|&d| caps.encodes(Codec::Hevc, d)) {
            keyframes_at(&node, depth);
        }
    }

    fn keyframes_at(node: &Path, depth: u8) {
        let (w, h) = (1920, 1080);
        let (mut conv, mut enc) = open(node, &Config { codec: Codec::Hevc, depth, width: w, height: h, fps: 60, bitrate: 20_000_000, slots: 4 }).unwrap();
        eprintln!("{}", enc.note);
        // HEVC keeps to a constant rate; only AV1 is opened at a variable one.
        assert!(enc.note.contains("constant rate"), "{}", enc.note);
        let mut stream = Vec::new();
        let mut keys = Vec::new();
        for n in 0..12u32 {
            let slot = (n % 4) as usize;
            conv.fill(slot, &picture(w, h, n * 16), w * 4).unwrap();
            if n == 8 {
                enc.set_bitrate(10_000_000).unwrap();
            }
            let (data, key) = enc.encode(slot, n as u64, n == 4).unwrap();
            assert!(!data.is_empty(), "frame {n} came back empty");
            if key {
                // Parameter sets in front of every keyframe: VPS, SPS, PPS.
                let t = nal_types(data);
                assert!(t.starts_with(&[32, 33, 34]), "keyframe {n} starts with {t:?}");
            }
            keys.push(key);
            stream.extend_from_slice(data);
        }
        assert_eq!(keys.iter().positions(), vec![0, 4, 8], "keyframes at {keys:?}");
        if let Ok(path) = std::env::var("RM_VA_DUMP") {
            std::fs::write(format!("{path}.{depth}"), &stream).unwrap();
        }
    }

    /// How long encoding takes, frame by frame: `RM_VA_BENCH=WxH cargo test --release -p
    /// remoteman-encode -- --ignored --nocapture bench`. Pictures change every frame; the
    /// upload from the CPU isn't timed, only the encoder's call.
    #[test]
    #[ignore]
    fn bench() {
        let Some((node, _)) = gpu() else { return };
        let size = std::env::var("RM_VA_BENCH").unwrap_or("1920x1080".into());
        let (w, h) = size.split_once('x').map(|(w, h)| (w.parse().unwrap(), h.parse().unwrap())).unwrap();
        let (mut conv, mut enc) = open(&node, &Config { codec: Codec::Hevc, depth: 10, width: w, height: h, fps: 120, bitrate: 40_000_000, slots: 4 }).unwrap();
        let pictures: Vec<Vec<u8>> = (0..8).map(|i| picture(w, h, i * 37)).collect();
        let mut times = Vec::new();
        for n in 0..300u32 {
            let slot = (n % 4) as usize;
            conv.fill(slot, &pictures[n as usize % 8], w * 4).unwrap();
            let t = std::time::Instant::now();
            enc.encode(slot, n as u64, false).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        times.drain(..10);
        times.sort_by(f64::total_cmp);
        let p = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
        eprintln!("{size}: encode ms median {:.2} p99 {:.2} max {:.2}", p(0.5), p(0.99), p(1.0));
    }

    trait Positions {
        fn positions(&self) -> Vec<usize>;
    }
    impl Positions for std::slice::Iter<'_, bool> {
        fn positions(&self) -> Vec<usize> {
            self.clone().enumerate().filter(|(_, k)| **k).map(|(i, _)| i).collect()
        }
    }
}
