//! Decoding with FFmpeg's libavcodec through D3D11VA, on the window's own device.
//!
//! A decoded picture is one slice of the decoder's array texture (P010 for 10-bit 4:2:0) and
//! stays there: the draw thread samples it where it is, or, where the driver won't let a
//! decoder texture be sampled, copies it once on the GPU. What the card doesn't decode is
//! decoded on the CPU and uploaded into textures of our own (`Decoder::software`): a codec the
//! card lacks, and HEVC 4:4:4 always, because this FFmpeg's HEVC decoder offers no D3D11VA
//! path for 4:4:4 even where the driver lists the profile (an RTX 5090 lists HEVC Main 4:4:4
//! 10 into Y410; FFmpeg picks yuv444p10le on the CPU). 4:4:4 is offered unasked only where the
//! processor keeps up (`App::withholds_444`).

use std::ffi::CStr;
use std::ptr;
use std::sync::Once;

use anyhow::{Context, Result, bail};
use ffmpeg_sys_next as ffi;
use remoteman_core::Codec;
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_SHADER_RESOURCE, ID3D11Texture2D};
use windows::core::Interface;

use crate::gpu::Gpu;

/// `AVERROR(EAGAIN)` with the Microsoft C library's EAGAIN: "give me more" or "take a picture
/// out first", not a failure.
const AGAIN: i32 = -11;

/// Pictures the viewer holds on to while the decoder goes on: the one waiting to be drawn, the
/// one on screen (kept for redrawing), and one changing hands. The decoder's pool is a texture
/// array of fixed size, so it must be told.
const HELD: i32 = 4;

fn message(err: i32) -> String {
    let mut buf = [0 as std::ffi::c_char; 128];
    // SAFETY: the buffer is ours and its length is given.
    unsafe {
        ffi::av_strerror(err, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

fn init() {
    static INIT: Once = Once::new();
    // SAFETY: setting a global log level.
    INIT.call_once(|| unsafe { ffi::av_log_set_level(ffi::AV_LOG_ERROR) });
}

fn find(codec: Codec) -> Result<*const ffi::AVCodec> {
    // SAFETY: looking up static codec descriptions.
    let found = unsafe {
        match codec {
            // FFmpeg's own AV1 decoder is the one with hardware paths; a search by id may hand
            // back a software one first.
            Codec::Av1 => ffi::avcodec_find_decoder_by_name(c"av1".as_ptr()),
            Codec::Hevc => ffi::avcodec_find_decoder(ffi::AVCodecID::AV_CODEC_ID_HEVC),
        }
    };
    if found.is_null() {
        bail!("this FFmpeg has no {codec:?} decoder");
    }
    Ok(found)
}

/// Whether this FFmpeg can decode `codec` through D3D11VA at all.
pub fn has_d3d11va(codec: Codec) -> Result<()> {
    let found = find(codec)?;
    for i in 0.. {
        // SAFETY: the codec is static; its configurations end with a null.
        let config = unsafe { ffi::avcodec_get_hw_config(found, i) };
        if config.is_null() {
            break;
        }
        // SAFETY: non-null configurations are static.
        let config = unsafe { &*config };
        if config.device_type == ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA && config.pix_fmt == ffi::AVPixelFormat::AV_PIX_FMT_D3D11 {
            return Ok(());
        }
    }
    bail!("this FFmpeg has no D3D11VA path for {codec:?}")
}

/// The codecs this card doesn't decode, which are decoded on the CPU instead: the last resort.
static SOFTWARE: std::sync::Mutex<Vec<Codec>> = std::sync::Mutex::new(Vec::new());

fn find_software(codec: Codec) -> Result<*const ffi::AVCodec> {
    // SAFETY: looking up static codec descriptions.
    let found = unsafe {
        match codec {
            // dav1d: FFmpeg's own AV1 decoder has no software path.
            Codec::Av1 => ffi::avcodec_find_decoder_by_name(c"libdav1d".as_ptr()),
            Codec::Hevc => ffi::avcodec_find_decoder(ffi::AVCodecID::AV_CODEC_ID_HEVC),
        }
    };
    if found.is_null() {
        bail!("this FFmpeg has no software {codec:?} decoder");
    }
    Ok(found)
}

/// Whether this FFmpeg can decode `codec` on the CPU.
pub fn can_software(codec: Codec) -> Result<()> {
    find_software(codec).map(|_| ())
}

/// From now on `codec` is decoded on the CPU. Fails if this FFmpeg can't.
pub fn use_software(codec: Codec) -> Result<()> {
    find_software(codec)?;
    let mut list = SOFTWARE.lock().unwrap();
    if !list.contains(&codec) {
        list.push(codec);
    }
    Ok(())
}

pub fn is_software(codec: Codec) -> bool {
    SOFTWARE.lock().unwrap().contains(&codec)
}

/// Whether this mode is decoded on the CPU: what `use_software` said for its codec, and
/// 4:4:4 always (FFmpeg's HEVC decoder has no D3D11VA path for it).
pub fn mode_in_software(mode: remoteman_core::Mode) -> bool {
    mode.chroma == remoteman_core::Chroma::C444 || is_software(mode.codec)
}

/// Textures a picture decoded on the CPU is put into: one a plane (Y, Cb, Cr), of 16 bits for
/// 10-bit pictures and 8 for 8-bit (formats every card has), a few sets used in turn so that
/// the one on screen isn't written to.
struct Upload {
    gpu: Gpu,
    sets: Vec<[ID3D11Texture2D; 3]>,
    size: (u32, u32),
    chroma_size: (u32, u32),
    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
    next: usize,
    /// For `REMOTEMAN_Y410=1` (a test): 4:4:4 10-bit packed as the card's decoder would give
    /// it, in Y410 textures, so that the drawing of them can be tried with no card decoder.
    packed: Vec<ID3D11Texture2D>,
    scratch: Vec<u32>,
    /// For a decoder on NVDEC: CUDA, and the sets' textures as registered with it.
    cuda: Option<crate::cuda::Cuda>,
    regs: Vec<Vec<crate::cuda::Resource>>,
    /// With CUDA: the chroma is one interleaved texture (P416), not two.
    semi: bool,
}

impl Drop for Upload {
    fn drop(&mut self) {
        if let Some(cuda) = &self.cuda {
            for r in &self.regs {
                cuda.unregister(r);
            }
        }
    }
}

impl Upload {
    fn texture(&self, width: u32, height: u32, format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT) -> Result<ID3D11Texture2D> {
        use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT};
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let mut texture = None;
        // SAFETY: the description lives through the call.
        unsafe { self.gpu.device.CreateTexture2D(&desc, None, Some(&mut texture)).context("a texture for a picture decoded on the CPU")? };
        texture.context("no texture")
    }

    /// Pack a yuv444p10 frame into Y410 (U in bits 0-9, Y in 10-19, V in 20-29, A 30-31) in the
    /// next of a few textures of that format.
    ///
    /// SAFETY: `f` is a decoded yuv444p10le frame, its planes valid for its size.
    unsafe fn put_y410(&mut self, f: &ffi::AVFrame) -> Result<ID3D11Texture2D> {
        let (w, h) = (f.width as usize, f.height as usize);
        if self.size != (w as u32, h as u32) || self.format != windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_Y410 || self.packed.is_empty() {
            self.packed.clear();
            for _ in 0..HELD {
                self.packed.push(self.texture(w as u32, h as u32, windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_Y410)?);
            }
            self.size = (w as u32, h as u32);
            self.format = windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_Y410;
            self.scratch = vec![0; w * h];
        }
        // SAFETY: rows are read within each plane's width and height.
        unsafe {
            for y in 0..h {
                let row = |i: usize| std::slice::from_raw_parts(f.data[i].add(y * f.linesize[i] as usize) as *const u16, w);
                let (py, pu, pv) = (row(0), row(1), row(2));
                for x in 0..w {
                    self.scratch[y * w + x] = (pu[x] as u32 & 0x3ff) | ((py[x] as u32 & 0x3ff) << 10) | ((pv[x] as u32 & 0x3ff) << 20) | (3 << 30);
                }
            }
        }
        let texture = self.packed[self.next % self.packed.len()].clone();
        self.next += 1;
        // SAFETY: the scratch is w*h*4 bytes.
        unsafe { self.gpu.context.UpdateSubresource(&texture, 0, None, self.scratch.as_ptr() as *const _, (w * 4) as u32, 0) };
        Ok(texture)
    }

    /// Put a 4:4:4 picture NVDEC decoded (FFmpeg's CUDA frame, planes in the card's memory) into
    /// the next three textures, GPU to GPU. 10-bit pictures are 16 bits with the 10 at the top.
    /// Returns the textures and whether the samples are that way (true) or 8-bit (false).
    ///
    /// SAFETY: `f` is a decoded CUDA frame with its hardware frames context.
    unsafe fn put_cuda(&mut self, f: &ffi::AVFrame) -> Result<([ID3D11Texture2D; 3], bool, bool)> {
        use ffi::AVPixelFormat::*;
        use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R16G16_UNORM, DXGI_FORMAT_R16_UNORM};
        // SAFETY: a CUDA frame has its frames context.
        let sw = unsafe { (*((*f.hw_frames_ctx).data as *const ffi::AVHWFramesContext)).sw_format };
        // The luma's format, its bytes a sample, whether the 10 bits are at the top, whether
        // Cb and Cr are interleaved.
        let (format, bytes, msb, semi) = match sw {
            AV_PIX_FMT_YUV444P16LE | AV_PIX_FMT_YUV444P10MSBLE => (DXGI_FORMAT_R16_UNORM, 2, true, false),
            AV_PIX_FMT_P416LE => (DXGI_FORMAT_R16_UNORM, 2, true, true),
            AV_PIX_FMT_YUV444P => (DXGI_FORMAT_R8_UNORM, 1, false, false),
            other => bail!("NVDEC gave a picture format ({}, {}) that isn't copied", other as i32, CStr::from_ptr(ffi::av_get_pix_fmt_name(other)).to_string_lossy()),
        };
        let (w, h) = (f.width as u32, f.height as u32);
        let cuda = self.cuda.context("no CUDA")?;
        if (self.size, self.chroma_size, self.format, self.semi) != ((w, h), (w, h), format, semi) || self.sets.is_empty() {
            for r in self.regs.drain(..) {
                cuda.unregister(&r);
            }
            self.sets.clear();
            for _ in 0..HELD {
                let y = self.texture(w, h, format)?;
                let set = if semi {
                    let uv = self.texture(w, h, DXGI_FORMAT_R16G16_UNORM)?;
                    [y, uv.clone(), uv]
                } else {
                    [y, self.texture(w, h, format)?, self.texture(w, h, format)?]
                };
                let mut regs = Vec::new();
                for t in set.iter().take(if semi { 2 } else { 3 }) {
                    match cuda.register(t) {
                        Ok(r) => regs.push(r),
                        Err(e) => {
                            cuda.unregister(&regs);
                            return Err(e);
                        }
                    }
                }
                self.regs.push(regs);
                self.sets.push(set);
            }
            (self.size, self.chroma_size, self.format, self.semi) = ((w, h), (w, h), format, semi);
        }
        let n = self.next % self.sets.len();
        self.next += 1;
        let plane = |i: usize, bytes: usize| crate::cuda::Plane { ptr: f.data[i] as usize as u64, pitch: f.linesize[i] as usize, width: w as usize * bytes, height: h as usize };
        if semi {
            cuda.copy(&self.regs[n], &[plane(0, bytes), plane(1, 4)])?;
        } else {
            cuda.copy(&self.regs[n], &[plane(0, bytes), plane(1, bytes), plane(2, bytes)])?;
        }
        Ok((self.sets[n].clone(), msb, semi))
    }

    /// Put a 4:2:0 or 4:4:4 frame of 8 or 10 bits into the next three textures, each plane as
    /// it lies in memory (10 bits in the low bits of 16; the shader is told), so the processor
    /// does nothing to it on the way. 4:4:4 keeps its chroma at full size: the shader samples
    /// it with the same coordinates, so nothing is halved.
    ///
    /// SAFETY: `f` is a decoded software frame in a planar format, its planes valid for its
    /// size.
    unsafe fn put(&mut self, f: &ffi::AVFrame) -> Result<[ID3D11Texture2D; 3]> {
        use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R16_UNORM};
        let (w, h) = (f.width as u32, f.height as u32);
        use ffi::AVPixelFormat::*;
        let full = [AV_PIX_FMT_YUV444P10LE as i32, AV_PIX_FMT_YUV444P as i32].contains(&f.format);
        let (cw, ch) = if full { (w, h) } else { (w.div_ceil(2), h.div_ceil(2)) };
        let ten = [AV_PIX_FMT_YUV420P10LE as i32, AV_PIX_FMT_YUV444P10LE as i32].contains(&f.format);
        anyhow::ensure!(ten || [AV_PIX_FMT_YUV420P as i32, AV_PIX_FMT_YUV444P as i32].contains(&f.format), "the software decoder gave a picture format ({}) that isn't drawn", f.format);
        let format = if ten { DXGI_FORMAT_R16_UNORM } else { DXGI_FORMAT_R8_UNORM };
        if (self.size, self.chroma_size, self.format) != ((w, h), (cw, ch), format) {
            self.sets.clear();
            for _ in 0..HELD {
                self.sets.push([self.texture(w, h, format)?, self.texture(cw, ch, format)?, self.texture(cw, ch, format)?]);
            }
            (self.size, self.chroma_size, self.format) = ((w, h), (cw, ch), format);
        }
        let set = self.sets[self.next % self.sets.len()].clone();
        self.next += 1;
        // SAFETY: each plane is read for its rows at its own pitch, within its size.
        unsafe {
            for (i, texture) in set.iter().enumerate() {
                self.gpu.context.UpdateSubresource(texture, 0, None, f.data[i] as *const _, f.linesize[i] as u32, 0);
            }
        }
        Ok(set)
    }
}

/// One decoded picture, still in the decoder's texture.
pub struct Picture {
    /// Keeps the slice from being decoded into again; freed when the picture is dropped.
    hold: *mut ffi::AVFrame,
    pub texture: ID3D11Texture2D,
    /// For a picture decoded on the CPU: its Cb and Cr, in textures of their own (`texture` is
    /// then the luma alone), each sample as the decoder gave it (10 bits in the low bits).
    pub chroma: Option<(ID3D11Texture2D, ID3D11Texture2D)>,
    /// With `chroma`: the 10 bits sit at the top of the 16 (as NVDEC leaves them, like P010),
    /// not at the bottom (as the CPU decoder does).
    pub msb: bool,
    /// With `chroma`: the chroma is one texture of Cb and Cr interleaved (R16G16, as NVDEC's
    /// P416 is); the second texture of `chroma` is then the same one and is not used.
    pub semi: bool,
    /// Which slice of the array texture.
    pub slice: u32,
    /// The visible size; the texture is the coded size, a little larger (1088 rows for 1080).
    pub width: u32,
    pub height: u32,
    /// The frame's age when its decoding was done, if the clocks have been compared.
    pub age_us: Option<i64>,
    pub decoded: std::time::Instant,
    /// When its frame came to this program (for measuring: the draw thread times the way from
    /// there to the screen).
    pub arrived: Option<std::time::Instant>,
}

// SAFETY: the frame is reference counted and may be freed from any thread; the texture belongs
// to a device with multithread protection on.
unsafe impl Send for Picture {}

impl Drop for Picture {
    fn drop(&mut self) {
        // SAFETY: we own this frame.
        unsafe { ffi::av_frame_free(&mut self.hold) };
    }
}

pub struct Decoder {
    ctx: *mut ffi::AVCodecContext,
    device: *mut ffi::AVBufferRef,
    packet: *mut ffi::AVPacket,
    frame: *mut ffi::AVFrame,
    /// e.g. "av1 (d3d11va)".
    pub name: String,
    /// Set for a decoder on the CPU: where its pictures go.
    upload: Option<Upload>,
    /// For a decoder on NVDEC that has not yet given a picture: the codec to decode on the CPU
    /// instead if it can't (no driver, or a format it doesn't copy).
    fallback: Option<(Gpu, Codec)>,
}

/// Whether 4:4:4 is decoded by the card (NVDEC through CUDA) here, as far as is known: set
/// where the card is probed (`Gpu::decodes`), cleared if a decoder on NVDEC then fails.
static NVDEC_444: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_nvdec_444(on: bool) {
    NVDEC_444.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether this mode is decoded on the processor: in software, and not 4:4:4 on NVDEC.
pub fn on_processor(mode: remoteman_core::Mode) -> bool {
    mode_in_software(mode) && !(mode.chroma == remoteman_core::Chroma::C444 && mode.codec == Codec::Hevc && NVDEC_444.load(std::sync::atomic::Ordering::Relaxed))
}

// SAFETY: used by the decode thread alone.
unsafe impl Send for Decoder {}

impl Drop for Decoder {
    fn drop(&mut self) {
        // Its CUDA registrations go while the context still lives.
        self.upload = None;
        // SAFETY: all four are ours; each call takes a null in its stride.
        unsafe {
            ffi::av_frame_free(&mut self.frame);
            ffi::av_packet_free(&mut self.packet);
            ffi::avcodec_free_context(&mut self.ctx);
            ffi::av_buffer_unref(&mut self.device);
        }
    }
}

/// CUDA pictures or none.
unsafe extern "C" fn pick_cuda(_ctx: *mut ffi::AVCodecContext, mut offered: *const ffi::AVPixelFormat) -> ffi::AVPixelFormat {
    // SAFETY: the list ends with AV_PIX_FMT_NONE.
    unsafe {
        while *offered != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if *offered == ffi::AVPixelFormat::AV_PIX_FMT_CUDA {
                return *offered;
            }
            offered = offered.add(1);
        }
    }
    ffi::AVPixelFormat::AV_PIX_FMT_NONE
}

/// D3D11VA pictures or none: FFmpeg would otherwise quietly decode on the CPU.
unsafe extern "C" fn pick_format(_ctx: *mut ffi::AVCodecContext, mut offered: *const ffi::AVPixelFormat) -> ffi::AVPixelFormat {
    // SAFETY: the list ends with AV_PIX_FMT_NONE.
    unsafe {
        while *offered != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if *offered == ffi::AVPixelFormat::AV_PIX_FMT_D3D11 {
                return *offered;
            }
            offered = offered.add(1);
        }
    }
    ffi::AVPixelFormat::AV_PIX_FMT_NONE
}

impl Decoder {
    /// `sampled`: ask for decoder textures the pixel shader may read as well. Not every driver
    /// allows that; if the first frame then fails, the caller starts again without.
    pub fn new(gpu: &Gpu, codec: Codec, sampled: bool) -> Result<Self> {
        init();
        let found = find(codec)?;
        // SAFETY: FFmpeg's own allocation and set-up calls, each checked; what is allocated is
        // owned by `this` from the start, so an early return frees it.
        unsafe {
            let mut this = Self { ctx: ptr::null_mut(), device: ptr::null_mut(), packet: ptr::null_mut(), frame: ptr::null_mut(), name: String::new(), upload: None, fallback: None };
            this.device = ffi::av_hwdevice_ctx_alloc(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
            anyhow::ensure!(!this.device.is_null(), "out of memory");
            let hw = (*((*this.device).data as *mut ffi::AVHWDeviceContext)).hwctx as *mut ffi::AVD3D11VADeviceContext;
            // FFmpeg releases the device when its context goes, so it gets a reference of its
            // own. It finds the device context and the video interfaces itself.
            (*hw).device = gpu.device.clone().into_raw() as *mut _;
            (*hw).BindFlags = if sampled { D3D11_BIND_SHADER_RESOURCE.0 as u32 } else { 0 };
            let err = ffi::av_hwdevice_ctx_init(this.device);
            anyhow::ensure!(err >= 0, "give FFmpeg the device: {}", message(err));

            this.ctx = ffi::avcodec_alloc_context3(found);
            anyhow::ensure!(!this.ctx.is_null(), "out of memory");
            (*this.ctx).hw_device_ctx = ffi::av_buffer_ref(this.device);
            (*this.ctx).get_format = Some(pick_format);
            (*this.ctx).extra_hw_frames = HELD;
            // The card does the work; frame threads would only hold every picture back.
            (*this.ctx).thread_count = 1;
            (*this.ctx).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            let err = ffi::avcodec_open2(this.ctx, found, ptr::null_mut());
            anyhow::ensure!(err >= 0, "open the decoder: {}", message(err));
            this.packet = ffi::av_packet_alloc();
            this.frame = ffi::av_frame_alloc();
            anyhow::ensure!(!this.packet.is_null() && !this.frame.is_null(), "out of memory");
            let name = CStr::from_ptr((*found).name).to_string_lossy();
            this.name = format!("{name} (d3d11va{})", if sampled { "" } else { ", copied" });
            Ok(this)
        }
    }

    /// A decoder on the CPU, for a card that has none for `codec`. Its pictures are put into
    /// textures one by one, which costs what the card's own decoder doesn't.
    pub fn software(gpu: &Gpu, codec: Codec) -> Result<Self> {
        if codec == Codec::Hevc && std::env::var("REMOTEMAN_CUDA").as_deref() != Ok("0") && std::env::var("REMOTEMAN_SOFTWARE").as_deref() != Ok("1") {
            match Self::nvdec(gpu) {
                Ok(this) => return Ok(this),
                Err(e) => {
                    set_nvdec_444(false);
                    say!("decoder: no NVDEC through CUDA here ({e:#}); on the CPU");
                }
            }
        }
        Self::cpu(gpu, codec)
    }

    /// HEVC on NVDEC through FFmpeg's CUDA device (the card the Direct3D device is on): the
    /// pictures are copied GPU to GPU into our textures (`Upload::put_cuda`). No frame threads
    /// and low delay, so a picture comes out for every frame sent.
    fn nvdec(gpu: &Gpu) -> Result<Self> {
        init();
        let found = find_software(Codec::Hevc)?;
        let ordinal = std::ffi::CString::new(crate::cuda::ordinal(gpu)?)?;
        // SAFETY: as in `new`.
        unsafe {
            let mut this = Self { ctx: ptr::null_mut(), device: ptr::null_mut(), packet: ptr::null_mut(), frame: ptr::null_mut(), name: String::new(), upload: None, fallback: Some((gpu.clone(), Codec::Hevc)) };
            let err = ffi::av_hwdevice_ctx_create(&mut this.device, ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA, ordinal.as_ptr(), ptr::null_mut(), 0);
            anyhow::ensure!(err >= 0, "FFmpeg's CUDA device: {}", message(err));
            let hw = (*((*this.device).data as *mut ffi::AVHWDeviceContext)).hwctx as *const crate::cuda::AvCudaDevice;
            let cuda = crate::cuda::Cuda::new(hw)?;
            this.ctx = ffi::avcodec_alloc_context3(found);
            anyhow::ensure!(!this.ctx.is_null(), "out of memory");
            (*this.ctx).hw_device_ctx = ffi::av_buffer_ref(this.device);
            (*this.ctx).get_format = Some(pick_cuda);
            (*this.ctx).thread_count = 1;
            (*this.ctx).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            let err = ffi::avcodec_open2(this.ctx, found, ptr::null_mut());
            anyhow::ensure!(err >= 0, "open the decoder: {}", message(err));
            this.packet = ffi::av_packet_alloc();
            this.frame = ffi::av_frame_alloc();
            anyhow::ensure!(!this.packet.is_null() && !this.frame.is_null(), "out of memory");
            this.name = format!("{} (nvdec, cuda)", CStr::from_ptr((*found).name).to_string_lossy());
            this.upload = Some(Upload { gpu: gpu.clone(), sets: Vec::new(), size: (0, 0), chroma_size: (0, 0), format: Default::default(), next: 0, packed: Vec::new(), scratch: Vec::new(), cuda: Some(cuda), regs: Vec::new(), semi: false });
            Ok(this)
        }
    }

    fn cpu(gpu: &Gpu, codec: Codec) -> Result<Self> {
        init();
        let found = find_software(codec)?;
        // SAFETY: as in `new`.
        unsafe {
            let mut this = Self { ctx: ptr::null_mut(), device: ptr::null_mut(), packet: ptr::null_mut(), frame: ptr::null_mut(), name: String::new(), upload: None, fallback: None };
            this.ctx = ffi::avcodec_alloc_context3(found);
            anyhow::ensure!(!this.ctx.is_null(), "out of memory");
            // As many threads as there are cores, but none that holds a picture back to
            // decode the next beside it.
            (*this.ctx).thread_count = 0;
            (*this.ctx).thread_type = ffi::FF_THREAD_SLICE as i32;
            (*this.ctx).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            let err = ffi::avcodec_open2(this.ctx, found, ptr::null_mut());
            anyhow::ensure!(err >= 0, "open the decoder: {}", message(err));
            this.packet = ffi::av_packet_alloc();
            this.frame = ffi::av_frame_alloc();
            anyhow::ensure!(!this.packet.is_null() && !this.frame.is_null(), "out of memory");
            this.name = format!("{} (on the CPU)", CStr::from_ptr((*found).name).to_string_lossy());
            this.upload = Some(Upload { gpu: gpu.clone(), sets: Vec::new(), size: (0, 0), chroma_size: (0, 0), format: Default::default(), next: 0, packed: Vec::new(), scratch: Vec::new(), cuda: None, regs: Vec::new(), semi: false });
            Ok(this)
        }
    }

    /// Feed one frame as the core delivers it; returns its picture.
    pub fn decode(&mut self, data: &[u8], age_us: Option<i64>) -> Result<Option<Picture>> {
        let first = self.decode_now(data, age_us);
        if let Some((gpu, codec)) = self.fallback.take() {
            match &first {
                // NVDEC works: it stays.
                Ok(Some(_)) => {}
                Ok(None) => self.fallback = Some((gpu, codec)),
                Err(e) => {
                    // The card, driver or format wouldn't do: this and the next frames on the CPU.
                    set_nvdec_444(false);
                    say!("decoder: NVDEC through CUDA failed on its first frame ({e:#}); on the CPU");
                    *self = Self::cpu(&gpu, codec)?;
                    return self.decode_now(data, age_us);
                }
            }
        }
        first
    }

    fn decode_now(&mut self, data: &[u8], age_us: Option<i64>) -> Result<Option<Picture>> {
        // SAFETY: the packet is filled with a copy of `data` (FFmpeg pads it) and unreferenced
        // again; the frame is ours; a D3D11 frame's data[0] is its texture and data[1] the slice.
        unsafe {
            let err = ffi::av_new_packet(self.packet, data.len() as i32);
            anyhow::ensure!(err >= 0, "out of memory");
            ptr::copy_nonoverlapping(data.as_ptr(), (*self.packet).data, data.len());
            let err = ffi::avcodec_send_packet(self.ctx, self.packet);
            ffi::av_packet_unref(self.packet);
            if err < 0 && err != AGAIN {
                bail!("decode: {}", message(err));
            }
            let err = ffi::avcodec_receive_frame(self.ctx, self.frame);
            if err == AGAIN {
                return Ok(None);
            }
            anyhow::ensure!(err >= 0, "decode: {}", message(err));
            let f = &*self.frame;
            if let Some(upload) = self.upload.as_mut() {
                if f.format == ffi::AVPixelFormat::AV_PIX_FMT_CUDA as i32 {
                    let put = upload.put_cuda(f);
                    let (width, height) = (f.width as u32, f.height as u32);
                    ffi::av_frame_unref(self.frame);
                    let ([texture, cb, cr], msb, semi) = put?;
                    return Ok(Some(Picture { hold: ptr::null_mut(), texture, chroma: Some((cb, cr)), msb, semi, slice: 0, width, height, age_us, decoded: std::time::Instant::now(), arrived: None }));
                }
                if f.format == ffi::AVPixelFormat::AV_PIX_FMT_YUV444P10LE as i32 && std::env::var("REMOTEMAN_Y410").as_deref() == Ok("1") {
                let put = upload.put_y410(f);
                let (width, height) = (f.width as u32, f.height as u32);
                ffi::av_frame_unref(self.frame);
                return Ok(Some(Picture { hold: ptr::null_mut(), texture: put?, chroma: None, msb: false, semi: false, slice: 0, width, height, age_us, decoded: std::time::Instant::now(), arrived: None }));
            }
            let put = upload.put(f);
                let (width, height) = (f.width as u32, f.height as u32);
                ffi::av_frame_unref(self.frame);
                let [texture, cb, cr] = put?;
                return Ok(Some(Picture { hold: ptr::null_mut(), texture, chroma: Some((cb, cr)), msb: false, semi: false, slice: 0, width, height, age_us, decoded: std::time::Instant::now(), arrived: None }));
            }
            anyhow::ensure!(f.format == ffi::AVPixelFormat::AV_PIX_FMT_D3D11 as i32, "the decoder gave a picture that isn't on the card");
            let raw = f.data[0] as *mut std::ffi::c_void;
            let texture = ID3D11Texture2D::from_raw_borrowed(&raw).context("a picture with no texture")?.clone();
            let hold = ffi::av_frame_clone(self.frame);
            anyhow::ensure!(!hold.is_null(), "out of memory");
            let picture = Picture { hold, texture, chroma: None, msb: false, semi: false, slice: f.data[1] as usize as u32, width: f.width as u32, height: f.height as u32, age_us, decoded: std::time::Instant::now(), arrived: None };
            ffi::av_frame_unref(self.frame);
            Ok(Some(picture))
        }
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// Not a check: decodes the clips in `REMOTEMAN_CLIP_DIR` (length-prefixed frames, as the host's
    /// `write_clips` test writes them) through the software decoder and says how long a picture takes.
    #[test]
    fn software_444_clips() {
        let Some(dir) = std::env::var_os("REMOTEMAN_CLIP_DIR") else { return };
        let Ok(gpu) = Gpu::open() else { return eprintln!("no GPU: skipped") };
        let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "rm")).collect();
        names.sort();
        for path in names {
            let bytes = std::fs::read(&path).unwrap();
            let mut frames = Vec::new();
            let mut i = 0;
            while i + 4 <= bytes.len() {
                let n = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
                frames.push(&bytes[i + 4..i + 4 + n]);
                i += 4 + n;
            }
            let mut d = Decoder::software(&gpu, Codec::Hevc).unwrap();
            eprintln!("DECODER {}", d.name);
            let mut times = Vec::new();
            for f in &frames {
                let t = std::time::Instant::now();
                let p = d.decode(f, None).unwrap();
                times.push((t.elapsed().as_secs_f64() * 1000.0, p.is_some()));
            }
            let mut ms: Vec<f64> = times.iter().skip(5).map(|t| t.0).collect();
            ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let avg = ms.iter().sum::<f64>() / ms.len() as f64;
            eprintln!("DECODE {}: {} frames, {} pictures, avg {avg:.1} ms, median {:.1} ms, max {:.1} ms", path.file_name().unwrap().to_string_lossy(), frames.len(), times.iter().filter(|t| t.1).count(), ms[ms.len() / 2], ms[ms.len() - 1]);
        }
    }
}
