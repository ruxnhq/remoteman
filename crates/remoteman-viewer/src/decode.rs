//! Decoding with FFmpeg's libavcodec on the GPU (Vulkan Video, VA-API or NVDEC), falling back to
//! the CPU.
//!
//! A picture never comes to CPU memory unless there is no other way:
//! - NVDEC pictures stay in CUDA memory; the window transfers them on the GPU into OpenGL
//!   textures and draws them (`shim/glview.c`).
//! - Vulkan and VA-API surfaces are handed out as DMA-BUFs, which GTK takes as textures as they
//!   are.
//! - Only when neither works (a software decoder, or the GPU path failing) is the picture copied
//!   down into planes for GTK to upload, which is far slower and is reported as such.

use std::ffi::CStr;
use std::ptr;
use std::sync::Once;

use anyhow::{Context, Result, bail};
use ff::codec::{self, threading};
use ff::ffi;
use ff::format::Pixel;
use ff::util::frame::Video;
use ffmpeg_next as ff;
use remoteman_core::Codec;

/// How a picture's samples are laid out, in GTK's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// 8-bit 4:2:0, Y then interleaved CbCr.
    Nv12,
    /// 10-bit 4:2:0 in 16-bit words (high bits), Y then interleaved CbCr.
    P010,
    /// 4:4:4 in 16-bit words (high bits), three planes.
    Yuv444High16,
    /// 10-bit 4:4:4 in the low bits of 16-bit words, three planes.
    Yuv444Low10,
}

/// One decoded picture.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
}

pub enum Pixels {
    /// In CPU memory: the planes one after another in `data`.
    Memory {
        layout: Layout,
        data: Vec<u8>,
        /// Where each plane starts in `data`, and its bytes per row.
        planes: Vec<(usize, usize)>,
    },
    /// Still on the GPU, in CUDA memory (NVDEC).
    Cuda {
        /// 0: NV12, 1: P010, 2: three 16-bit planes (4:4:4). As `shim/glview.c` numbers them.
        layout: i32,
        /// The decoder's CUDA context, as an address.
        context: usize,
        /// Each plane's CUDA pointer and bytes per row.
        data: [u64; 3],
        pitch: [i32; 3],
        /// Keeps the surface (and the decoder's device) alive.
        hold: Surface,
    },
    /// Still on the GPU, as a DMA-BUF.
    Dmabuf {
        /// The DRM format code of the whole picture (e.g. `P010`).
        fourcc: u32,
        modifier: u64,
        /// Each plane's descriptor, offset and bytes per row.
        planes: Vec<(i32, u32, u32)>,
        /// Keeps the surface and its descriptors alive; drop it when the picture is no longer shown.
        hold: Surface,
    },
}

/// A decoded surface mapped out as a DMA-BUF.
pub struct Surface(*mut ffi::AVFrame);

// SAFETY: an AVFrame holding references; freeing it from another thread is fine.
unsafe impl Send for Surface {}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: we own this frame.
        unsafe { ffi::av_frame_free(&mut self.0) };
    }
}

const fn fourcc(c: &[u8; 4]) -> u32 {
    c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16 | (c[3] as u32) << 24
}

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = ff::init();
        ff::util::log::set_level(ff::util::log::Level::Fatal);
    });
}

/// GPU decoders to try, best first. `REMOTEMAN_HWDEC=cuda|vaapi|vulkan|none` picks one.
fn hw_candidates() -> Vec<ffi::AVHWDeviceType> {
    use ffi::AVHWDeviceType::*;
    match std::env::var("REMOTEMAN_HWDEC").as_deref() {
        Ok("none") => vec![],
        Ok("cuda") => vec![AV_HWDEVICE_TYPE_CUDA],
        Ok("vaapi") => vec![AV_HWDEVICE_TYPE_VAAPI],
        Ok("vulkan") => vec![AV_HWDEVICE_TYPE_VULKAN],
        // NVDEC where there is an NVIDIA GPU: its Vulkan decoder's pictures can't leave as real
        // DMA-BUFs (seen on driver 620: no modifier, no plane layout), and copying those down is
        // slower than copying NVDEC's. Elsewhere VA-API, whose surfaces go to GTK without a copy.
        _ => vec![AV_HWDEVICE_TYPE_CUDA, AV_HWDEVICE_TYPE_VAAPI, AV_HWDEVICE_TYPE_VULKAN],
    }
}

fn codec_has_hw(codec: &ff::Codec, kind: ffi::AVHWDeviceType) -> bool {
    for i in 0.. {
        // SAFETY: the codec is a static AVCodec; its configs end with a null.
        let config = unsafe { ffi::avcodec_get_hw_config(codec.as_ptr(), i) };
        if config.is_null() {
            return false;
        }
        // SAFETY: non-null configs are static.
        let config = unsafe { &*config };
        if config.device_type == kind && config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0 {
            return true;
        }
    }
    false
}

struct HwDevice(*mut ffi::AVBufferRef);

// SAFETY: FFmpeg device contexts are reference counted and thread safe.
unsafe impl Send for HwDevice {}

impl Drop for HwDevice {
    fn drop(&mut self) {
        // SAFETY: we own this reference.
        unsafe { ffi::av_buffer_unref(&mut self.0) };
    }
}

fn open_hw(codec: &ff::Codec) -> Option<(HwDevice, String)> {
    for kind in hw_candidates() {
        if !codec_has_hw(codec, kind) {
            continue;
        }
        let mut dev = ptr::null_mut();
        // SAFETY: on success `dev` receives a new reference we then own.
        let err = unsafe { ffi::av_hwdevice_ctx_create(&mut dev, kind, ptr::null(), ptr::null_mut(), 0) };
        if err >= 0 && !dev.is_null() {
            // SAFETY: a static string for every known type.
            let name = unsafe { CStr::from_ptr(ffi::av_hwdevice_get_type_name(kind)) }.to_string_lossy().into_owned();
            return Some((HwDevice(dev), name));
        }
    }
    None
}

pub struct Decoder {
    decoder: ff::decoder::Video,
    _hw: Option<HwDevice>,
    frame: Video,
    download: Video,
    /// Hand surfaces out as DMA-BUFs (until that fails once).
    export: bool,
    /// Leave NVDEC pictures in CUDA memory for the window to draw (until that fails once).
    gpu: bool,
    /// e.g. "av1 (vulkan)".
    pub name: String,
}

impl Decoder {
    pub fn new(codec: Codec) -> Result<Self> {
        init();
        // FFmpeg's own AV1 decoder is the one that decodes on the GPU; `find` may hand back a
        // software one first.
        let found = match codec {
            Codec::Av1 => ff::decoder::find_by_name("av1").or_else(|| ff::decoder::find(codec::Id::AV1)),
            Codec::Hevc => ff::decoder::find(codec::Id::HEVC),
        }
        .with_context(|| format!("FFmpeg has no {codec:?} decoder"))?;
        let mut ctx = codec::Context::new_with_codec(found);
        // Slice threads add no delay; frame threads would hold every picture back.
        ctx.set_threading(threading::Config { kind: threading::Type::Slice, count: 0 });
        ctx.set_flags(codec::Flags::LOW_DELAY);
        let hw = open_hw(&found);
        let name = match &hw {
            Some((dev, kind)) => {
                // SAFETY: the context takes its own reference to the device.
                unsafe { (*ctx.as_mut_ptr()).hw_device_ctx = ffi::av_buffer_ref(dev.0) };
                format!("{} ({kind})", found.name())
            }
            None => format!("{} (cpu)", found.name()),
        };
        let decoder = ctx.decoder().video().context("open decoder")?;
        Ok(Self { decoder, _hw: hw.map(|(d, _)| d), frame: Video::empty(), download: Video::empty(), export: std::env::var("REMOTEMAN_DMABUF").as_deref() != Ok("0"), gpu: std::env::var("REMOTEMAN_GL").as_deref() != Ok("0"), name })
    }

    /// Stop handing out GPU pictures (the window couldn't take one) and copy them down from now on.
    pub fn stop_exporting(&mut self) {
        self.export = false;
        self.gpu = false;
    }

    /// The NVDEC picture in `self.frame`, left where it is.
    fn cuda_surface(&self) -> Option<Pixels> {
        // SAFETY: `self.frame` is a valid CUDA frame, so its frames and device contexts exist;
        // a CUDA device context starts with its CUcontext.
        unsafe {
            let src = self.frame.as_ptr();
            if (*src).hw_frames_ctx.is_null() {
                return None;
            }
            let frames = (*(*src).hw_frames_ctx).data as *const ffi::AVHWFramesContext;
            let layout = match (*frames).sw_format {
                ffi::AVPixelFormat::AV_PIX_FMT_NV12 => 0,
                ffi::AVPixelFormat::AV_PIX_FMT_P010LE => 1,
                ffi::AVPixelFormat::AV_PIX_FMT_YUV444P16LE | ffi::AVPixelFormat::AV_PIX_FMT_YUV444P10MSBLE => 2,
                _ => return None,
            };
            let context = *((*(*frames).device_ctx).hwctx as *const usize);
            let hold = ffi::av_frame_clone(src);
            if hold.is_null() {
                return None;
            }
            let planes = if layout == 2 { 3 } else { 2 };
            let (mut data, mut pitch) = ([0u64; 3], [0i32; 3]);
            for i in 0..planes {
                data[i] = (*src).data[i] as u64;
                pitch[i] = (*src).linesize[i];
            }
            Some(Pixels::Cuda { layout, context, data, pitch, hold: Surface(hold) })
        }
    }

    /// The surface in `self.frame` as a DMA-BUF, if this GPU and format allow it.
    fn export_surface(&self) -> Option<Pixels> {
        // SAFETY: `self.frame` is a valid hardware frame; the mapped frame is ours until it is
        // wrapped in `Surface`, which frees it. The descriptor lives as long as the mapped frame.
        unsafe {
            let src = self.frame.as_ptr();
            if (*src).hw_frames_ctx.is_null() {
                return None;
            }
            let sw = (*((*(*src).hw_frames_ctx).data as *const ffi::AVHWFramesContext)).sw_format;
            let mut mapped = ffi::av_frame_alloc();
            (*mapped).format = ffi::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            if ffi::av_hwframe_map(mapped, src, ffi::AV_HWFRAME_MAP_READ as i32) < 0 {
                ffi::av_frame_free(&mut mapped);
                return None;
            }
            let hold = Surface(mapped);
            let d = &*((*mapped).data[0] as *const ffi::AVDRMFrameDescriptor);
            let mut planes = Vec::new();
            for layer in &d.layers[..d.nb_layers as usize] {
                for plane in &layer.planes[..layer.nb_planes as usize] {
                    planes.push((d.objects[plane.object_index as usize].fd, plane.offset as u32, plane.pitch as u32));
                }
            }
            // One layer describes the whole picture; several describe a plane each, and then the
            // picture's own code comes from the pixel format.
            let code = if d.nb_layers == 1 {
                d.layers[0].format
            } else {
                match sw {
                    ffi::AVPixelFormat::AV_PIX_FMT_NV12 => fourcc(b"NV12"),
                    ffi::AVPixelFormat::AV_PIX_FMT_P010LE => fourcc(b"P010"),
                    _ => return None,
                }
            };
            if std::env::var_os("REMOTEMAN_DEBUG").is_some() {
                let c = code.to_le_bytes();
                eprintln!(
                    "dmabuf: {} layers, {} objects, code {} modifier {:#x}, planes {:?}",
                    d.nb_layers,
                    d.nb_objects,
                    String::from_utf8_lossy(&c),
                    d.objects[0].format_modifier,
                    planes
                );
            }
            // A buffer with no layout to speak of can't be handed to anyone else.
            const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
            if planes.is_empty() || planes.len() > 4 || d.objects[0].format_modifier == MOD_INVALID {
                return None;
            }
            Some(Pixels::Dmabuf { fourcc: code, modifier: d.objects[0].format_modifier, planes, hold })
        }
    }

    /// Feed one frame as the core delivers it; returns its picture.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>> {
        let packet = ff::Packet::copy(data);
        match self.decoder.send_packet(&packet) {
            Ok(()) => {}
            Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => {}
            Err(e) => return Err(e).context("decode"),
        }
        match self.decoder.receive_frame(&mut self.frame) {
            Ok(()) => {}
            Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => return Ok(None),
            Err(e) => return Err(e).context("decode"),
        }
        let mut frame = &self.frame;
        if self.export && matches!(frame.format(), Pixel::VAAPI | Pixel::VULKAN) {
            match self.export_surface() {
                Some(pixels) => return Ok(Some(Picture { width: frame.width(), height: frame.height(), pixels })),
                None => self.export = false,
            }
        }
        if self.gpu && frame.format() == Pixel::CUDA {
            match self.cuda_surface() {
                Some(pixels) => return Ok(Some(Picture { width: frame.width(), height: frame.height(), pixels })),
                None => self.gpu = false,
            }
        }
        if matches!(frame.format(), Pixel::CUDA | Pixel::VAAPI | Pixel::VULKAN) {
            // SAFETY: both frames are valid; the copy lands in a fresh CPU frame.
            unsafe {
                ffi::av_frame_unref(self.download.as_mut_ptr());
                let err = ffi::av_hwframe_transfer_data(self.download.as_mut_ptr(), frame.as_ptr(), 0);
                anyhow::ensure!(err >= 0, "copy the picture from the GPU: {}", ff::Error::from(err));
            }
            frame = &self.download;
        }
        let (layout, plane_count, chroma_rows) = match frame.format() {
            Pixel::NV12 => (Layout::Nv12, 2, frame.height().div_ceil(2)),
            Pixel::P010LE => (Layout::P010, 2, frame.height().div_ceil(2)),
            Pixel::YUV444P16LE | Pixel::YUV444P10MSBLE | Pixel::YUV444P10MSB => (Layout::Yuv444High16, 3, frame.height()),
            Pixel::YUV444P10LE => (Layout::Yuv444Low10, 3, frame.height()),
            other => bail!("the decoder gave {other:?} pictures, which the viewer can't show yet"),
        };
        let mut data = Vec::new();
        let mut planes = Vec::with_capacity(plane_count);
        for p in 0..plane_count {
            let rows = if p == 0 { frame.height() } else { chroma_rows } as usize;
            let stride = frame.stride(p);
            planes.push((data.len(), stride));
            data.extend_from_slice(&frame.data(p)[..stride * rows]);
        }
        Ok(Some(Picture { width: frame.width(), height: frame.height(), pixels: Pixels::Memory { layout, data, planes } }))
    }
}
