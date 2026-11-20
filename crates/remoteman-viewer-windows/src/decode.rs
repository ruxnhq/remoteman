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

/// Textures a picture decoded on the CPU is put into: one of 16 bits for the luma, one of two
/// 16-bit channels for the chroma (formats every card has), a few pairs used in turn so that
/// the one on screen isn't written to.
struct Upload {
    gpu: Gpu,
    pairs: Vec<(ID3D11Texture2D, ID3D11Texture2D)>,
    size: (u32, u32),
    next: usize,
    luma: Vec<u16>,
    chroma: Vec<u16>,
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

    /// Put a 4:2:0 or 4:4:4 frame of 8 or 10 bits into the next pair of textures. 4:4:4 keeps
    /// its chroma at full size: the shader samples it with the same coordinates, so nothing is
    /// halved on the way.
    ///
    /// SAFETY: `f` is a decoded software frame in a planar 4:2:0 format, its planes valid for
    /// its size.
    unsafe fn put(&mut self, f: &ffi::AVFrame) -> Result<(ID3D11Texture2D, ID3D11Texture2D)> {
        use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM};
        let (w, h) = (f.width as usize, f.height as usize);
        use ffi::AVPixelFormat::*;
        let full = [AV_PIX_FMT_YUV444P10LE as i32, AV_PIX_FMT_YUV444P as i32].contains(&f.format);
        let (cw, ch) = if full { (w, h) } else { (w.div_ceil(2), h.div_ceil(2)) };
        let ten = [AV_PIX_FMT_YUV420P10LE as i32, AV_PIX_FMT_YUV444P10LE as i32].contains(&f.format);
        anyhow::ensure!(ten || [AV_PIX_FMT_YUV420P as i32, AV_PIX_FMT_YUV444P as i32].contains(&f.format), "the software decoder gave a picture format ({}) that isn't drawn", f.format);
        if self.size != (w as u32, h as u32) || self.chroma.len() != cw * ch * 2 {
            self.pairs.clear();
            for _ in 0..HELD {
                self.pairs.push((self.texture(w as u32, h as u32, DXGI_FORMAT_R16_UNORM)?, self.texture(cw as u32, ch as u32, DXGI_FORMAT_R16G16_UNORM)?));
            }
            self.size = (w as u32, h as u32);
            self.luma = vec![0; w * h];
            self.chroma = vec![0; cw * ch * 2];
        }
        // Every sample ends up as the card's 10-bit pictures have it: the code in the top ten
        // of sixteen bits.
        // SAFETY (for the block): rows are read within each plane's width and height.
        unsafe {
            for y in 0..h {
                let to = &mut self.luma[y * w..(y + 1) * w];
                let row = f.data[0].add(y * f.linesize[0] as usize);
                if ten {
                    for (t, s) in to.iter_mut().zip(std::slice::from_raw_parts(row as *const u16, w)) {
                        *t = s << 6;
                    }
                } else {
                    for (t, s) in to.iter_mut().zip(std::slice::from_raw_parts(row, w)) {
                        *t = (*s as u16) << 8;
                    }
                }
            }
            for y in 0..ch {
                let to = &mut self.chroma[y * cw * 2..(y + 1) * cw * 2];
                let (u, v) = (f.data[1].add(y * f.linesize[1] as usize), f.data[2].add(y * f.linesize[2] as usize));
                if ten {
                    let (u, v) = (std::slice::from_raw_parts(u as *const u16, cw), std::slice::from_raw_parts(v as *const u16, cw));
                    for x in 0..cw {
                        to[2 * x] = u[x] << 6;
                        to[2 * x + 1] = v[x] << 6;
                    }
                } else {
                    let (u, v) = (std::slice::from_raw_parts(u, cw), std::slice::from_raw_parts(v, cw));
                    for x in 0..cw {
                        to[2 * x] = (u[x] as u16) << 8;
                        to[2 * x + 1] = (v[x] as u16) << 8;
                    }
                }
            }
            let pair = self.pairs[self.next % self.pairs.len()].clone();
            self.next += 1;
            self.gpu.context.UpdateSubresource(&pair.0, 0, None, self.luma.as_ptr() as *const _, (w * 2) as u32, 0);
            self.gpu.context.UpdateSubresource(&pair.1, 0, None, self.chroma.as_ptr() as *const _, (cw * 4) as u32, 0);
            Ok(pair)
        }
    }
}

/// One decoded picture, still in the decoder's texture.
pub struct Picture {
    /// Keeps the slice from being decoded into again; freed when the picture is dropped.
    hold: *mut ffi::AVFrame,
    pub texture: ID3D11Texture2D,
    /// For a picture decoded on the CPU: its chroma, in a texture of its own (`texture` is
    /// then the luma alone).
    pub chroma: Option<ID3D11Texture2D>,
    /// Which slice of the array texture.
    pub slice: u32,
    /// The visible size; the texture is the coded size, a little larger (1088 rows for 1080).
    pub width: u32,
    pub height: u32,
    /// The frame's age when its decoding was done, if the clocks have been compared.
    pub age_us: Option<i64>,
    pub decoded: std::time::Instant,
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
}

// SAFETY: used by the decode thread alone.
unsafe impl Send for Decoder {}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: all four are ours; each call takes a null in its stride.
        unsafe {
            ffi::av_frame_free(&mut self.frame);
            ffi::av_packet_free(&mut self.packet);
            ffi::avcodec_free_context(&mut self.ctx);
            ffi::av_buffer_unref(&mut self.device);
        }
    }
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
            let mut this = Self { ctx: ptr::null_mut(), device: ptr::null_mut(), packet: ptr::null_mut(), frame: ptr::null_mut(), name: String::new(), upload: None };
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
        init();
        let found = find_software(codec)?;
        // SAFETY: as in `new`.
        unsafe {
            let mut this = Self { ctx: ptr::null_mut(), device: ptr::null_mut(), packet: ptr::null_mut(), frame: ptr::null_mut(), name: String::new(), upload: None };
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
            this.upload = Some(Upload { gpu: gpu.clone(), pairs: Vec::new(), size: (0, 0), next: 0, luma: Vec::new(), chroma: Vec::new() });
            Ok(this)
        }
    }

    /// Feed one frame as the core delivers it; returns its picture.
    pub fn decode(&mut self, data: &[u8], age_us: Option<i64>) -> Result<Option<Picture>> {
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
                let put = upload.put(f);
                let (width, height) = (f.width as u32, f.height as u32);
                ffi::av_frame_unref(self.frame);
                let (texture, chroma) = put?;
                return Ok(Some(Picture { hold: ptr::null_mut(), texture, chroma: Some(chroma), slice: 0, width, height, age_us, decoded: std::time::Instant::now() }));
            }
            anyhow::ensure!(f.format == ffi::AVPixelFormat::AV_PIX_FMT_D3D11 as i32, "the decoder gave a picture that isn't on the card");
            let raw = f.data[0] as *mut std::ffi::c_void;
            let texture = ID3D11Texture2D::from_raw_borrowed(&raw).context("a picture with no texture")?.clone();
            let hold = ffi::av_frame_clone(self.frame);
            anyhow::ensure!(!hold.is_null(), "out of memory");
            let picture = Picture { hold, texture, chroma: None, slice: f.data[1] as usize as u32, width: f.width as u32, height: f.height as u32, age_us, decoded: std::time::Instant::now() };
            ffi::av_frame_unref(self.frame);
            Ok(Some(picture))
        }
    }
}
