//! Decoding with FFmpeg's libavcodec on the GPU (NVDEC, VA-API or Vulkan Video), falling back to
//! the CPU. Pictures come back as planes in CPU memory for GTK to upload; GTK converts them to
//! RGB on its own GPU pass. (Handing GTK the GPU surface itself, with no download, is later work.)

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

/// One decoded picture: its planes one after another in `data`.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub layout: Layout,
    pub data: Vec<u8>,
    /// Where each plane starts in `data`, and its bytes per row.
    pub planes: Vec<(usize, usize)>,
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
    /// e.g. "av1 (cuda)".
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
        Ok(Self { decoder, _hw: hw.map(|(d, _)| d), frame: Video::empty(), download: Video::empty(), name })
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
        Ok(Some(Picture { width: frame.width(), height: frame.height(), layout, data, planes }))
    }
}
