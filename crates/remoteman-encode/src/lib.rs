//! NVENC through NVIDIA's API directly (by way of the C wrapper in `shim/`): 10-bit, 4:2:0 or
//! 4:4:4, HEVC or AV1, tuned for the lowest delay. On Linux it is fed from CUDA memory that
//! Vulkan wrote; on Windows from P010 textures of a Direct3D 11 device (4:2:0 only).
//!
//! What a host needs of any encoder is the small [`FrameEncoder`] trait, and what it does with
//! a client's loss reports is [`recovery::Recovery`]; neither knows about NVIDIA, so another
//! encoder (AMF, QSV, Direct3D 12 Video Encode, VA-API) goes behind the same two.

pub mod recovery;

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
    long_term: u32,
    short_term: u32,
}

#[cfg(not(windows))]
unsafe extern "C" {
    fn rm_cuda_open(err: *mut c_char, errlen: c_int) -> *mut c_void;
    fn rm_cuda_close(c: *mut c_void);
    fn rm_cuda_import(c: *mut c_void, fd: c_int, size: u64, devptr: *mut u64, err: *mut c_char, errlen: c_int) -> c_int;
    fn rm_nvenc_open(cuda: *mut c_void, cfg: *const Cfg, err: *mut c_char, errlen: c_int) -> *mut c_void;
    fn rm_nvenc_register(e: *mut c_void, devptr: u64, pitch: u32) -> c_int;
}

#[cfg(windows)]
unsafe extern "C" {
    fn rm_nvenc_open_d3d11(device: *mut c_void, cfg: *const Cfg, err: *mut c_char, errlen: c_int) -> *mut c_void;
    fn rm_nvenc_register_d3d11(e: *mut c_void, texture: *mut c_void) -> c_int;
}

unsafe extern "C" {
    fn rm_nvenc_error(e: *mut c_void) -> *const c_char;
    #[allow(clippy::too_many_arguments)]
    fn rm_nvenc_encode(e: *mut c_void, idx: c_int, ts: u64, force_idr: c_int, mark: c_int, refer: u32, data: *mut *const u8, size: *mut u32, key: *mut c_int, long_term: *mut u32) -> c_int;
    fn rm_nvenc_short_term(e: *mut c_void) -> u32;
    fn rm_nvenc_last_qp(e: *mut c_void) -> u32;
    fn rm_nvenc_long_term(e: *mut c_void) -> u32;
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
    /// How many long-term reference frames to keep room for: frames the encoder holds on to
    /// when told, to build a later frame from when everything recent was lost on the way.
    pub long_term: u32,
    /// How many recent frames the encoder keeps to refer to. `None`: the usual four, and for
    /// large HEVC pictures three, so the stream's level stays where decoders expect it. Said
    /// outright, both numbers are taken as they are.
    pub short_term: Option<u32>,
}

/// How to encode one frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct How {
    /// As a keyframe.
    pub idr: bool,
    /// Keep it as long-term frame number this.
    pub mark: Option<u32>,
    /// Build it from these long-term frames only (bit i: frame i), not from recent ones.
    pub refer: u32,
}

/// One encoded frame.
pub struct Made<'a> {
    pub data: &'a [u8],
    pub key: bool,
    /// The long-term frame number the encoder kept it as, if it did.
    pub marked: Option<u32>,
    /// The long-term frames it was built from (bit i: frame i).
    pub referred: u32,
    /// The frame's average quantiser as the encoder reports it (lower is finer; HEVC's scale
    /// runs to 51, AV1's to 255). For logs: it shows whether the encoder is spending its rate.
    pub qp: u32,
}

/// What a host asks of an encoder, whichever it is. Inputs are registered with the encoder in
/// its own way (CUDA memory, Direct3D textures) and named here by their index.
pub trait FrameEncoder {
    /// What the encoder said about itself when it opened, for the log.
    fn note(&self) -> &str;
    /// How many recent frames it keeps to refer to.
    fn short_term(&self) -> u32;
    /// How many long-term frames it agreed to keep.
    fn long_term(&self) -> u32;
    /// Encode input `idx` as `how` says; `timestamp` is the frame's number, by which
    /// [`Self::invalidate`] names it later.
    fn encode_how(&mut self, idx: usize, timestamp: u64, how: How) -> Result<Made<'_>>;
    /// Encode at this many bits a second from the next frame on, with no keyframe.
    fn set_bitrate(&mut self, bitrate: u32) -> Result<()>;
    /// The frame with this timestamp was lost on the way: stop referring to it.
    fn invalidate(&mut self, timestamp: u64) -> Result<()>;
}

pub struct Encoder {
    #[cfg_attr(windows, allow(dead_code))]
    cuda: *mut c_void,
    enc: *mut c_void,
    /// What the encoder said about itself when it opened.
    pub note: String,
}

fn text(buf: &[c_char]) -> String {
    // SAFETY: the wrapper always terminates what it writes, and the buffer starts zeroed.
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

impl Config {
    fn raw(&self) -> Cfg {
        Cfg {
            codec: (self.codec == Codec::Av1) as u32,
            chroma444: self.chroma444 as u32,
            width: self.width,
            height: self.height,
            fps: self.fps,
            bitrate: self.bitrate,
            preset: self.preset,
            hdr_pq: 0,
            long_term: self.long_term,
            short_term: self.short_term.unwrap_or(0),
        }
    }
}

impl Encoder {
    /// An encoder fed from CUDA memory (see [`Self::add_input`]).
    #[cfg(not(windows))]
    pub fn new(cfg: &Config) -> Result<Self> {
        let mut err = [0 as c_char; 256];
        // SAFETY: the wrapper's contract; `err` is a writable buffer of the given length.
        unsafe {
            let cuda = rm_cuda_open(err.as_mut_ptr(), 256);
            if cuda.is_null() {
                bail!("{}", text(&err));
            }
            let enc = rm_nvenc_open(cuda, &cfg.raw(), err.as_mut_ptr(), 256);
            if enc.is_null() {
                rm_cuda_close(cuda);
                bail!("{}", text(&err));
            }
            Ok(Self { cuda, enc, note: text(&err) })
        }
    }

    /// An encoder fed from textures of a Direct3D 11 device (see [`Self::add_texture`]).
    /// 4:2:0 only: Direct3D has no planar 10-bit 4:4:4 texture.
    ///
    /// # Safety
    /// `device` must be a live `ID3D11Device*` with multithread protection switched on (the
    /// encoder works on it from threads of its own), and must outlive the encoder.
    #[cfg(windows)]
    pub unsafe fn new_d3d11(device: *mut c_void, cfg: &Config) -> Result<Self> {
        let mut err = [0 as c_char; 256];
        // SAFETY: the wrapper's contract; `err` is a writable buffer of the given length.
        unsafe {
            let enc = rm_nvenc_open_d3d11(device, &cfg.raw(), err.as_mut_ptr(), 256);
            if enc.is_null() {
                bail!("{}", text(&err));
            }
            Ok(Self { cuda: std::ptr::null_mut(), enc, note: text(&err) })
        }
    }

    /// Register a P010 texture of the encoder's size as an input. Returns the input's index.
    ///
    /// # Safety
    /// `texture` must be a live `ID3D11Texture2D*` of the device the encoder was opened on, and
    /// must outlive the encoder.
    #[cfg(windows)]
    pub unsafe fn add_texture(&mut self, texture: *mut c_void) -> Result<usize> {
        // SAFETY: a live encoder; the caller vouches for the texture.
        let idx = unsafe { rm_nvenc_register_d3d11(self.enc, texture) };
        if idx < 0 {
            bail!("{}", self.last_error());
        }
        Ok(idx as usize)
    }

    fn last_error(&self) -> String {
        // SAFETY: a live encoder; the string is its own buffer.
        unsafe { CStr::from_ptr(rm_nvenc_error(self.enc)) }.to_string_lossy().into_owned()
    }

    /// Share memory Vulkan exported (CUDA takes `fd` over) and register it as an input.
    /// Returns the input's index.
    #[cfg(not(windows))]
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

    /// How many recent frames the encoder keeps to refer to. A frame can be invalidated and
    /// the stream carry on without a keyframe only while an older one is still among them.
    pub fn short_term(&self) -> u32 {
        // SAFETY: a live encoder.
        unsafe { rm_nvenc_short_term(self.enc) }
    }

    /// How many long-term frames the encoder agreed to keep (at most what was asked for).
    pub fn long_term(&self) -> u32 {
        // SAFETY: a live encoder.
        unsafe { rm_nvenc_long_term(self.enc) }
    }

    /// Encode input `idx`; returns the bitstream and whether it is a keyframe.
    pub fn encode(&mut self, idx: usize, timestamp: u64, force_idr: bool) -> Result<(&[u8], bool)> {
        let made = self.encode_how(idx, timestamp, How { idr: force_idr, ..How::default() })?;
        Ok((made.data, made.key))
    }

    /// Encode input `idx` as `how` says.
    pub fn encode_how(&mut self, idx: usize, timestamp: u64, how: How) -> Result<Made<'_>> {
        let (mut data, mut size, mut key, mut long_term) = (std::ptr::null(), 0u32, 0, 0u32);
        let mark = how.mark.map_or(-1, |m| m as c_int);
        // SAFETY: the returned bytes are the encoder's own buffer, valid until the next call,
        // which the borrow of `self` enforces.
        unsafe {
            if rm_nvenc_encode(self.enc, idx as c_int, timestamp, how.idr as c_int, mark, how.refer, &mut data, &mut size, &mut key, &mut long_term) != 0 {
                bail!("{}", self.last_error());
            }
            Ok(Made {
                data: std::slice::from_raw_parts(data, size as usize),
                key: key != 0,
                marked: (long_term >> 31 != 0).then_some(long_term >> 16 & 0xff),
                referred: long_term & 0xffff,
                qp: rm_nvenc_last_qp(self.enc),
            })
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

impl FrameEncoder for Encoder {
    fn note(&self) -> &str {
        &self.note
    }

    fn short_term(&self) -> u32 {
        Encoder::short_term(self)
    }

    fn long_term(&self) -> u32 {
        Encoder::long_term(self)
    }

    fn encode_how(&mut self, idx: usize, timestamp: u64, how: How) -> Result<Made<'_>> {
        Encoder::encode_how(self, idx, timestamp, how)
    }

    fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        Encoder::set_bitrate(self, bitrate)
    }

    fn invalidate(&mut self, timestamp: u64) -> Result<()> {
        Encoder::invalidate(self, timestamp)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: ours, and not used again; the encoder goes before the context it lives in.
        unsafe {
            rm_nvenc_close(self.enc);
            #[cfg(not(windows))]
            rm_cuda_close(self.cuda);
        }
    }
}
