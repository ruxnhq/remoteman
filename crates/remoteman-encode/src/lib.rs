//! NVENC through NVIDIA's API directly (by way of the C wrapper in `shim/`), fed from CUDA
//! memory that Vulkan wrote: 10-bit, 4:2:0 or 4:4:4, HEVC or AV1, tuned for the lowest delay.

use std::ffi::{CStr, c_char, c_int, c_void};

use anyhow::{Result, bail};

#[repr(C)]
struct Cfg {
    codec: u32,
    chroma444: u32,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    preset: u32,
    hdr_pq: u32,
}

unsafe extern "C" {
    fn rm_cuda_open(err: *mut c_char, errlen: c_int) -> *mut c_void;
    fn rm_cuda_import(c: *mut c_void, fd: c_int, size: u64, devptr: *mut u64, err: *mut c_char, errlen: c_int) -> c_int;
    fn rm_nvenc_open(cuda: *mut c_void, cfg: *const Cfg, err: *mut c_char, errlen: c_int) -> *mut c_void;
    fn rm_nvenc_error(e: *mut c_void) -> *const c_char;
    fn rm_nvenc_register(e: *mut c_void, devptr: u64, pitch: u32) -> c_int;
    fn rm_nvenc_encode(e: *mut c_void, idx: c_int, ts: u64, force_idr: c_int, data: *mut *const u8, size: *mut u32, key: *mut c_int) -> c_int;
    fn rm_nvenc_set_bitrate(e: *mut c_void, bitrate: u32) -> c_int;
    fn rm_nvenc_invalidate(e: *mut c_void, ts: u64) -> c_int;
    fn rm_nvenc_close(e: *mut c_void);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Hevc,
    Av1,
}

pub struct Config {
    pub codec: Codec,
    pub chroma444: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub preset: u32,
}

pub struct Encoder {
    cuda: *mut c_void,
    enc: *mut c_void,
    /// What the encoder said about itself when it opened.
    pub note: String,
}

fn text(buf: &[c_char]) -> String {
    // SAFETY: the wrapper always terminates what it writes, and the buffer starts zeroed.
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

impl Encoder {
    pub fn new(cfg: &Config) -> Result<Self> {
        let mut err = [0 as c_char; 256];
        // SAFETY: the wrapper's contract; `err` is a writable buffer of the given length.
        unsafe {
            let cuda = rm_cuda_open(err.as_mut_ptr(), 256);
            if cuda.is_null() {
                bail!("{}", text(&err));
            }
            let c = Cfg {
                codec: (cfg.codec == Codec::Av1) as u32,
                chroma444: cfg.chroma444 as u32,
                width: cfg.width,
                height: cfg.height,
                fps: cfg.fps,
                bitrate: cfg.bitrate,
                preset: cfg.preset,
                hdr_pq: 0,
            };
            let enc = rm_nvenc_open(cuda, &c, err.as_mut_ptr(), 256);
            if enc.is_null() {
                bail!("{}", text(&err));
            }
            Ok(Self { cuda, enc, note: text(&err) })
        }
    }

    fn last_error(&self) -> String {
        // SAFETY: a live encoder; the string is its own buffer.
        unsafe { CStr::from_ptr(rm_nvenc_error(self.enc)) }.to_string_lossy().into_owned()
    }

    /// Share memory Vulkan exported (CUDA takes `fd` over) and register it as an input.
    /// Returns the input's index.
    pub fn add_input(&mut self, fd: i32, size: u64, pitch: u32) -> Result<usize> {
        let mut err = [0 as c_char; 256];
        let mut ptr = 0u64;
        // SAFETY: as above.
        unsafe {
            if rm_cuda_import(self.cuda, fd, size, &mut ptr, err.as_mut_ptr(), 256) != 0 {
                bail!("{}", text(&err));
            }
            let idx = rm_nvenc_register(self.enc, ptr, pitch);
            if idx < 0 {
                bail!("{}", self.last_error());
            }
            Ok(idx as usize)
        }
    }

    /// Encode input `idx`; returns the bitstream and whether it is a keyframe.
    pub fn encode(&mut self, idx: usize, timestamp: u64, force_idr: bool) -> Result<(&[u8], bool)> {
        let (mut data, mut size, mut key) = (std::ptr::null(), 0u32, 0);
        // SAFETY: the returned bytes are the encoder's own buffer, valid until the next call,
        // which the borrow of `self` enforces.
        unsafe {
            if rm_nvenc_encode(self.enc, idx as c_int, timestamp, force_idr as c_int, &mut data, &mut size, &mut key) != 0 {
                bail!("{}", self.last_error());
            }
            Ok((std::slice::from_raw_parts(data, size as usize), key != 0))
        }
    }

    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        // SAFETY: a live encoder.
        if unsafe { rm_nvenc_set_bitrate(self.enc, bitrate) } != 0 {
            bail!("{}", self.last_error());
        }
        Ok(())
    }

    /// The frame with this timestamp was lost on the way: stop referring to it.
    pub fn invalidate(&mut self, timestamp: u64) -> Result<()> {
        // SAFETY: a live encoder.
        if unsafe { rm_nvenc_invalidate(self.enc, timestamp) } != 0 {
            bail!("{}", self.last_error());
        }
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: ours, and not used again. (The CUDA context is left to process exit.)
        unsafe { rm_nvenc_close(self.enc) };
    }
}
