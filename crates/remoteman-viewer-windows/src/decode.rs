//! Decoding with FFmpeg's libavcodec through D3D11VA, on the window's own device.
//!
//! A decoded picture is one slice of the decoder's array texture (P010 for 10-bit 4:2:0) and
//! stays there: the draw thread samples it where it is, or, where the driver won't let a
//! decoder texture be sampled, copies it once on the GPU. There is no software fallback: a
//! picture decoded on the CPU would have to be uploaded, and the host is only offered what the
//! card decodes (`Gpu::decodes`).

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

/// One decoded picture, still in the decoder's texture.
pub struct Picture {
    /// Keeps the slice from being decoded into again; freed when the picture is dropped.
    hold: *mut ffi::AVFrame,
    pub texture: ID3D11Texture2D,
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
            let mut this = Self { ctx: ptr::null_mut(), device: ptr::null_mut(), packet: ptr::null_mut(), frame: ptr::null_mut(), name: String::new() };
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
            anyhow::ensure!(f.format == ffi::AVPixelFormat::AV_PIX_FMT_D3D11 as i32, "the decoder gave a picture that isn't on the card");
            let raw = f.data[0] as *mut std::ffi::c_void;
            let texture = ID3D11Texture2D::from_raw_borrowed(&raw).context("a picture with no texture")?.clone();
            let hold = ffi::av_frame_clone(self.frame);
            anyhow::ensure!(!hold.is_null(), "out of memory");
            let picture = Picture { hold, texture, slice: f.data[1] as usize as u32, width: f.width as u32, height: f.height as u32, age_us, decoded: std::time::Instant::now() };
            ffi::av_frame_unref(self.frame);
            Ok(Some(picture))
        }
    }
}
