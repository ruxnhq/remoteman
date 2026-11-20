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
            this.upload = Some(Upload { gpu: gpu.clone(), sets: Vec::new(), size: (0, 0), chroma_size: (0, 0), format: Default::default(), next: 0 });
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
                let [texture, cb, cr] = put?;
                return Ok(Some(Picture { hold: ptr::null_mut(), texture, chroma: Some((cb, cr)), slice: 0, width, height, age_us, decoded: std::time::Instant::now() }));
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
