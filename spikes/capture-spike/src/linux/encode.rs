//! Hardware video encoding through FFmpeg's NVENC encoders, tuned for mirroring: no B-frames,
//! no lookahead, one-frame-ish rate control buffer. NVENC takes the captured BGRx/RGBx frames
//! as they are and converts them to YUV on the GPU: straight from CUDA memory for zero-copy
//! captures, or uploaded from CPU memory otherwise.

use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use ffmpeg_next as ff;
use ff::Dictionary;
use ff::ffi;
use ff::format::Pixel;
use ff::software::scaling;
use ff::util::frame::Video;

use super::capture::{Frame, FrameData, PixelFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone, Copy)]
pub struct EncoderConfig {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

/// One encoded access unit in Annex-B form.
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub time_100ns: i64,
}

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = ff::init();
        ff::util::log::set_level(ff::util::log::Level::Error);
    });
}

fn pixel(f: PixelFormat) -> Pixel {
    match f {
        PixelFormat::Bgrx => Pixel::BGRZ,
        PixelFormat::Bgra => Pixel::BGRA,
        PixelFormat::Rgbx => Pixel::RGBZ,
        PixelFormat::Rgba => Pixel::RGBA,
    }
}

pub struct Encoder {
    encoder: ff::encoder::Video,
    cfg: EncoderConfig,
    /// The pixel layout the encoder was opened for (that of the first frame).
    input: Pixel,
    /// Opened for CUDA frames (zero-copy capture) rather than CPU memory.
    gpu: bool,
    frame: Video,
    /// Resizing, for frames that don't match the encoder's size.
    scaler: Option<(scaling::Context, (u32, u32, Pixel))>,
    keyframe: AtomicBool,
    /// Time spent per stage, in microseconds: (copying in, submitting, collecting output, calls).
    stages: (u128, u128, u128, u32),
    pub name: String,
}

impl Encoder {
    /// Open an encoder for frames like `first` (same memory kind and pixel layout).
    pub fn new(cfg: EncoderConfig, first: &Frame) -> Result<Self> {
        init();
        let name = match cfg.codec {
            VideoCodec::Hevc => "hevc_nvenc",
            VideoCodec::H264 => "h264_nvenc",
            VideoCodec::Av1 => "av1_nvenc",
        };
        let codec = ff::encoder::find_by_name(name).with_context(|| format!("FFmpeg has no {name} encoder"))?;
        let mut enc = ff::codec::Context::new_with_codec(codec).encoder().video()?;
        let input = pixel(first.format);
        enc.set_width(cfg.width);
        enc.set_height(cfg.height);
        let gpu = match &first.data {
            FrameData::Gpu(g) => {
                anyhow::ensure!(
                    (first.width, first.height) == (cfg.width, cfg.height),
                    "GPU capture can't be resized yet: encode at the screen's {}x{} (or set ESPALIER_CAPTURE=shm)",
                    first.width,
                    first.height
                );
                enc.set_format(Pixel::CUDA);
                // SAFETY: the encoder takes its own reference to the capture's CUDA frame pool.
                unsafe { (*enc.as_mut_ptr()).hw_frames_ctx = ffi::av_buffer_ref((*g.as_ptr()).hw_frames_ctx) };
                true
            }
            FrameData::Cpu { .. } => {
                enc.set_format(input);
                false
            }
        };
        enc.set_time_base((1, 10_000_000)); // 100 ns ticks, as on Windows
        enc.set_frame_rate(Some((cfg.fps as i32, 1)));
        enc.set_bit_rate(cfg.bitrate as usize);
        enc.set_max_bit_rate(cfg.bitrate as usize);
        enc.set_max_b_frames(0);
        // Keyframes only on request (and at the start): mirroring recovers with forced IDRs.
        enc.set_gop(u32::MAX >> 1);
        // The colour matrix isn't ours to pick: NVENC converts RGB input with BT.601, and FFmpeg
        // tags the stream to match.
        enc.set_color_range(ff::util::color::Range::MPEG);
        // SAFETY: plain field writes on an encoder context that isn't open yet.
        unsafe {
            let ctx = enc.as_mut_ptr();
            // A VBV of about two frames keeps frame sizes even, which keeps latency low.
            (*ctx).rc_buffer_size = (cfg.bitrate as u64 * 2 / cfg.fps.max(1) as u64) as i32;
            (*ctx).color_primaries = ffi::AVColorPrimaries::AVCOL_PRI_BT709;
            (*ctx).color_trc = ffi::AVColorTransferCharacteristic::AVCOL_TRC_BT709;
        }
        let preset = std::env::var("RM_PRESET").unwrap_or_else(|_| "p1".into());
        let mut opts = Dictionary::new();
        for (k, v) in [
            ("preset", preset.as_str()),
            ("tune", "ull"),
            ("zerolatency", "1"),
            ("delay", "0"),
            ("rc", "cbr"),
            ("forced-idr", "1"),
            ("rc-lookahead", "0"),
        ] {
            opts.set(k, v);
        }
        let encoder = enc.open_as_with(codec, opts).with_context(|| format!("open {name} (is the NVIDIA driver loaded?)"))?;
        Ok(Self {
            encoder,
            cfg,
            input,
            gpu,
            frame: Video::empty(),
            scaler: None,
            keyframe: AtomicBool::new(true),
            stages: (0, 0, 0, 0),
            name: name.into(),
        })
    }

    /// Copy (and if needed resize) a CPU frame into the encoder's input frame.
    fn fill(&mut self, src: &Frame, src_stride: usize, data: &[u8], format: Pixel) -> Result<()> {
        let (w, h) = (self.cfg.width, self.cfg.height);
        if self.frame.width() != w || self.frame.height() != h {
            self.frame = Video::new(self.input, w, h);
        }
        // SAFETY: the encoder may still hold a reference to the last frame's buffer.
        unsafe {
            let err = ffi::av_frame_make_writable(self.frame.as_mut_ptr());
            anyhow::ensure!(err >= 0, "frame buffer: {}", ff::Error::from(err));
        }
        let row = src.width as usize * 4;
        if (src.width, src.height) == (w, h) {
            let dst_stride = self.frame.stride(0);
            let dst = self.frame.data_mut(0);
            for y in 0..h as usize {
                dst[y * dst_stride..y * dst_stride + row].copy_from_slice(&data[y * src_stride..y * src_stride + row]);
            }
        } else {
            // Different size: scale on the CPU. Slow at 4K; match the encoder to the screen.
            let key = (src.width, src.height, format);
            if self.scaler.as_ref().is_none_or(|(_, k)| *k != key) {
                let ctx = scaling::Context::get(format, src.width, src.height, self.input, w, h, scaling::Flags::BILINEAR)?;
                self.scaler = Some((ctx, key));
            }
            let mut tmp = Video::new(format, src.width, src.height);
            let stride = tmp.stride(0);
            let buf = tmp.data_mut(0);
            for y in 0..src.height as usize {
                buf[y * stride..y * stride + row].copy_from_slice(&data[y * src_stride..y * src_stride + row]);
            }
            self.scaler.as_mut().unwrap().0.run(&tmp, &mut self.frame)?;
        }
        Ok(())
    }

    /// Average milliseconds per call spent copying the frame in, submitting it (which uploads it
    /// to the GPU), and collecting the output, since the last call to this.
    pub fn take_stage_ms(&mut self) -> (f64, f64, f64) {
        let (c, s, r, n) = std::mem::take(&mut self.stages);
        let avg = |us: u128| us as f64 / 1000.0 / n.max(1) as f64;
        (avg(c), avg(s), avg(r))
    }

    /// Make the next encoded frame an IDR (for a receiver that lost sync).
    pub fn force_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }

    /// Encode one frame; returns whatever access units the encoder finished.
    pub fn encode(&mut self, src: &Frame, time_100ns: i64) -> Result<Vec<EncodedFrame>> {
        let t0 = std::time::Instant::now();
        let format = pixel(src.format);
        anyhow::ensure!(format == self.input, "capture format changed from {:?} to {:?}", self.input, format);
        let key = self.keyframe.swap(false, Ordering::Relaxed);
        let pict = if key { ffi::AVPictureType::AV_PICTURE_TYPE_I } else { ffi::AVPictureType::AV_PICTURE_TYPE_NONE };
        let t1;
        match &src.data {
            FrameData::Gpu(g) => {
                anyhow::ensure!(self.gpu, "capture switched to GPU memory mid-stream");
                t1 = std::time::Instant::now();
                // SAFETY: a new reference to the CUDA frame (no copy) carries this call's timing.
                unsafe {
                    let mut f = ffi::av_frame_clone(g.as_ptr());
                    anyhow::ensure!(!f.is_null(), "out of memory");
                    (*f).pts = time_100ns;
                    (*f).pict_type = pict;
                    let e = ffi::avcodec_send_frame(self.encoder.as_mut_ptr(), f);
                    ffi::av_frame_free(&mut f);
                    if e < 0 {
                        return Err(ff::Error::from(e)).context("encode");
                    }
                }
            }
            FrameData::Cpu { stride, bytes } => {
                anyhow::ensure!(!self.gpu, "capture fell back to CPU memory; restart the encoder");
                self.fill(src, *stride, bytes, format)?;
                t1 = std::time::Instant::now();
                self.frame.set_pts(Some(time_100ns));
                self.frame.set_kind(if key { ff::picture::Type::I } else { ff::picture::Type::None });
                self.encoder.send_frame(&self.frame).context("encode")?;
            }
        }
        let t2 = std::time::Instant::now();

        let mut out = Vec::new();
        let mut packet = ff::Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => out.push(EncodedFrame {
                    data: packet.data().unwrap_or_default().to_vec(),
                    keyframe: packet.is_key(),
                    time_100ns: packet.pts().unwrap_or(time_100ns),
                }),
                Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => break,
                Err(ff::Error::Eof) => break,
                Err(e) => return Err(e).context("encode"),
            }
        }
        let t3 = std::time::Instant::now();
        self.stages.0 += (t1 - t0).as_micros();
        self.stages.1 += (t2 - t1).as_micros();
        self.stages.2 += (t3 - t2).as_micros();
        self.stages.3 += 1;
        Ok(out)
    }
}
