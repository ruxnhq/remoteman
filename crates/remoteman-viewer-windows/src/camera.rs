//! This PC's camera, lent to the computer (docs/PROTOCOL.md, "Camera"): the first camera
//! Windows lists, read with Media Foundation's Source Reader, which hands its pictures over as
//! NV12 fitted into 1280 by 720 (decoding a camera's JPEG and scaling on the way), and encoded
//! as the protocol asks: H.264, Annex-B, the parameter sets in front of every keyframe, no
//! B-frames, a keyframe every two seconds and whenever one is asked for.
//!
//! The encoder is FFmpeg's (the same library the picture is decoded with), the first of these
//! that opens: NVIDIA's (`h264_nvenc`), Windows' own Media Foundation encoder (`h264_mf`, the
//! graphics card's if it has one, else Microsoft's in software), or OpenH264. The choices and
//! their settings are the Linux viewer's (`remoteman-camera/src/source.rs`), with Media
//! Foundation's encoder in place of VA-API. That crate isn't used here: it is built on FFmpeg's
//! safe wrapper and on PipeWire, neither of which this crate has. `REMOTEMAN_CAMERA_ENCODER`
//! names one encoder to use and no other, for tests.

use std::ffi::CString;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use ffmpeg_sys_next as ffi;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::PWSTR;

/// What the protocol's one sure mode allows.
pub const MAX: (u32, u32) = (1280, 720);
pub const FPS: u32 = 30;
const BITRATE: i64 = 2_500_000;

/// The encoders tried, in order, with their options. (Not Microsoft's own software encoder
/// through `h264_mf`: tried on this PC, it held frames back, 24 out for 40 in, so a keyframe
/// asked for came out late. OpenH264, in the FFmpeg the app carries, is the software one.)
const ENCODERS: [(&str, &[(&str, &str)]); 3] = [
    ("h264_nvenc", &[("preset", "p1"), ("tune", "ull"), ("zerolatency", "1"), ("delay", "0"), ("forced-idr", "1"), ("rc", "cbr")]),
    // The graphics card's encoder through Windows (AMD's, Intel's, NVIDIA's).
    ("h264_mf", &[("hw_encoding", "1"), ("scenario", "display_remoting"), ("rate_control", "cbr")]),
    // Not skipping frames to keep to the bitrate (the Linux viewer lets it): skipped, the
    // frames after a keyframe asked for no longer line up with the pictures given.
    ("libopenh264", &[("allow_skip_frames", "0")]),
];

/// The largest even size within [`MAX`] of the same shape.
pub fn fit(width: u32, height: u32) -> (u32, u32) {
    let (w, h) = (width.max(2) as u64, height.max(2) as u64);
    let (mut ow, mut oh) = (w, h);
    if w > MAX.0 as u64 || h > MAX.1 as u64 {
        if w * MAX.1 as u64 >= h * MAX.0 as u64 {
            (ow, oh) = (MAX.0 as u64, h * MAX.0 as u64 / w);
        } else {
            (ow, oh) = (w * MAX.1 as u64 / h, MAX.1 as u64);
        }
    }
    ((ow as u32 & !1).max(2), (oh as u32 & !1).max(2))
}

/// Whether an Annex-B access unit holds a sequence parameter set (NAL type 7).
pub fn has_parameter_sets(data: &[u8]) -> bool {
    data.windows(4).any(|w| w[0] == 0 && w[1] == 0 && w[2] == 1 && w[3] & 0x1f == 7)
}

/// One picture in NV12: the luma plane and the interleaved chroma plane, each with its stride.
pub struct Nv12<'a> {
    pub width: u32,
    pub height: u32,
    pub y: &'a [u8],
    pub y_stride: usize,
    pub uv: &'a [u8],
    pub uv_stride: usize,
}

/// An H.264 encoder that keeps to the protocol.
pub struct H264 {
    ctx: *mut ffi::AVCodecContext,
    frame: *mut ffi::AVFrame,
    packet: *mut ffi::AVPacket,
    /// Which of [`ENCODERS`] it is.
    which: usize,
    pub name: &'static str,
    /// It takes I420 (three planes) rather than NV12.
    planar: bool,
    pub width: u32,
    pub height: u32,
    count: i64,
    /// A keyframe was asked for and the next picture is to be made one.
    owed: bool,
    /// The picture (its number) that was made a keyframe and hasn't come out yet.
    waiting: Option<i64>,
    /// The encoder gave an ordinary frame where a keyframe was asked for: it is opened again,
    /// since a new encoder's first frame is always a keyframe.
    reopen: bool,
}

// SAFETY: the encoder is used by one thread at a time (it is moved, never shared).
unsafe impl Send for H264 {}

impl Drop for H264 {
    fn drop(&mut self) {
        // SAFETY: all three are ours; each call takes a null in its stride.
        unsafe {
            ffi::av_frame_free(&mut self.frame);
            ffi::av_packet_free(&mut self.packet);
            ffi::avcodec_free_context(&mut self.ctx);
        }
    }
}

fn error(err: i32) -> String {
    let mut buf = [0 as std::ffi::c_char; 128];
    // SAFETY: the buffer is as large as said, and FFmpeg ends what it writes.
    unsafe {
        ffi::av_strerror(err, buf.as_mut_ptr(), buf.len());
        std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

/// Those of [`ENCODERS`] the test asked for, if it asked, by index.
fn candidates() -> Vec<usize> {
    let only = std::env::var("REMOTEMAN_CAMERA_ENCODER").ok();
    (0..ENCODERS.len()).filter(|i| only.as_deref().is_none_or(|o| o == ENCODERS[*i].0)).collect()
}

impl H264 {
    /// The first encoder that opens for pictures of this size.
    pub fn open(width: u32, height: u32) -> Result<Self> {
        // SAFETY: a plain setting.
        unsafe { ffi::av_log_set_level(ffi::AV_LOG_FATAL) };
        let mut why = anyhow!("this FFmpeg has no H.264 encoder remoteman can use (NVIDIA's, Media Foundation's or OpenH264)");
        for which in candidates() {
            match Self::open_one(which, width, height) {
                Ok(encoder) => return Ok(encoder),
                Err(e) => why = e,
            }
        }
        Err(why)
    }

    fn open_one(which: usize, width: u32, height: u32) -> Result<Self> {
        let (name, options) = ENCODERS[which];
        let planar = name == "libopenh264";
        let format = if planar { ffi::AVPixelFormat::AV_PIX_FMT_YUV420P } else { ffi::AVPixelFormat::AV_PIX_FMT_NV12 };
        let cname = CString::new(name)?;
        // SAFETY: FFmpeg's allocation and set-up calls, each checked; `this` owns what is
        // allocated from the start, so a failure frees it.
        unsafe {
            let codec = ffi::avcodec_find_encoder_by_name(cname.as_ptr());
            if codec.is_null() {
                bail!("this FFmpeg has no {name}");
            }
            let mut this = Self { ctx: ffi::avcodec_alloc_context3(codec), frame: ptr::null_mut(), packet: ptr::null_mut(), which, name, planar, width, height, count: 0, owed: true, waiting: None, reopen: false };
            anyhow::ensure!(!this.ctx.is_null(), "out of memory");
            let c = &mut *this.ctx;
            c.width = width as i32;
            c.height = height as i32;
            c.pix_fmt = format;
            c.time_base = ffi::AVRational { num: 1, den: FPS as i32 };
            c.framerate = ffi::AVRational { num: FPS as i32, den: 1 };
            c.bit_rate = BITRATE;
            c.rc_max_rate = BITRATE;
            c.max_b_frames = 0;
            c.gop_size = (FPS * 2) as i32;
            let mut dict: *mut ffi::AVDictionary = ptr::null_mut();
            for (k, v) in options {
                let (k, v) = (CString::new(*k)?, CString::new(*v)?);
                ffi::av_dict_set(&mut dict, k.as_ptr(), v.as_ptr(), 0);
            }
            let err = ffi::avcodec_open2(this.ctx, codec, &mut dict);
            ffi::av_dict_free(&mut dict);
            if err < 0 {
                bail!("{name} can't be opened: {}", error(err));
            }
            this.frame = ffi::av_frame_alloc();
            this.packet = ffi::av_packet_alloc();
            anyhow::ensure!(!this.frame.is_null() && !this.packet.is_null(), "out of memory");
            (*this.frame).format = format as i32;
            (*this.frame).width = width as i32;
            (*this.frame).height = height as i32;
            let err = ffi::av_frame_get_buffer(this.frame, 0);
            anyhow::ensure!(err >= 0, "no memory for the encoder's picture ({})", error(err));
            Ok(this)
        }
    }

    /// Encode one picture (of the size the encoder was opened for); `key`: make it a keyframe.
    /// Every frame that comes out goes to `out` with whether it is a keyframe.
    pub fn encode(&mut self, picture: &Nv12, key: bool, out: &mut dyn FnMut(&[u8], bool)) -> Result<()> {
        anyhow::ensure!(picture.width == self.width && picture.height == self.height, "a picture of another size");
        if self.reopen {
            say!("camera: {} gave an ordinary frame where a keyframe was asked for; opening it again", self.name);
            // What it still holds comes out first (ordinary frames, which lean on frames the
            // other side has), then a new encoder starts with a keyframe.
            // SAFETY: the end of the stream, said to our own encoder.
            if unsafe { ffi::avcodec_send_frame(self.ctx, ptr::null()) } >= 0 {
                self.waiting = None;
                self.drain(out);
            }
            let again = Self::open_one(self.which, self.width, self.height)?;
            *self = again;
        }
        let (w, h) = (self.width as usize, self.height as usize);
        anyhow::ensure!(picture.y.len() >= picture.y_stride * (h - 1) + w && picture.uv.len() >= picture.uv_stride * (h / 2 - 1) + w, "the picture is smaller than its size");
        let force = key || self.owed;
        if force {
            self.owed = false;
            self.waiting.get_or_insert(self.count);
        }
        // SAFETY: the frame is ours and writable after `av_frame_make_writable`; each plane is
        // written within its stride and rows, and each source plane read within the length
        // checked above.
        unsafe {
            let err = ffi::av_frame_make_writable(self.frame);
            anyhow::ensure!(err >= 0, "the encoder's picture: {}", error(err));
            let f = &mut *self.frame;
            let y = std::slice::from_raw_parts_mut(f.data[0], f.linesize[0] as usize * h);
            for row in 0..h {
                y[row * f.linesize[0] as usize..][..w].copy_from_slice(&picture.y[row * picture.y_stride..][..w]);
            }
            if self.planar {
                let (u, v) = (std::slice::from_raw_parts_mut(f.data[1], f.linesize[1] as usize * h / 2), std::slice::from_raw_parts_mut(f.data[2], f.linesize[2] as usize * h / 2));
                for row in 0..h / 2 {
                    let from = &picture.uv[row * picture.uv_stride..][..w];
                    for x in 0..w / 2 {
                        u[row * f.linesize[1] as usize + x] = from[2 * x];
                        v[row * f.linesize[2] as usize + x] = from[2 * x + 1];
                    }
                }
            } else {
                let uv = std::slice::from_raw_parts_mut(f.data[1], f.linesize[1] as usize * h / 2);
                for row in 0..h / 2 {
                    uv[row * f.linesize[1] as usize..][..w].copy_from_slice(&picture.uv[row * picture.uv_stride..][..w]);
                }
            }
            f.pts = self.count;
            self.count += 1;
            f.pict_type = if force { ffi::AVPictureType::AV_PICTURE_TYPE_I } else { ffi::AVPictureType::AV_PICTURE_TYPE_NONE };
            if force {
                f.flags |= ffi::AV_FRAME_FLAG_KEY;
            } else {
                f.flags &= !ffi::AV_FRAME_FLAG_KEY;
            }
            let err = ffi::avcodec_send_frame(self.ctx, self.frame);
            anyhow::ensure!(err >= 0, "{} refused a picture: {}", self.name, error(err));
        }
        self.drain(out);
        Ok(())
    }

    /// Hand over every frame the encoder has ready; note whether the keyframe asked for came.
    fn drain(&mut self, out: &mut dyn FnMut(&[u8], bool)) {
        // SAFETY: packets from our own encoder, read within their size and then given back.
        unsafe {
            while ffi::avcodec_receive_packet(self.ctx, self.packet) >= 0 {
                let p = &*self.packet;
                let data = std::slice::from_raw_parts(p.data, p.size.max(0) as usize);
                let is_key = p.flags & ffi::AV_PKT_FLAG_KEY != 0;
                let extra = (*self.ctx).extradata;
                if is_key && !has_parameter_sets(data) && !extra.is_null() && (*self.ctx).extradata_size > 0 {
                    // Parameter sets only in the encoder's header: they go in front, as the
                    // protocol has them on every keyframe.
                    let mut whole = std::slice::from_raw_parts(extra, (*self.ctx).extradata_size as usize).to_vec();
                    whole.extend_from_slice(data);
                    out(&whole, true);
                } else {
                    out(data, is_key);
                }
                // The picture made a keyframe (or a later one) is out: was it one?
                if self.waiting.is_some_and(|asked| is_key || p.pts >= asked) {
                    self.waiting = None;
                    if !is_key {
                        self.reopen = true;
                    }
                }
                ffi::av_packet_unref(self.packet);
            }
        }
    }

    /// The next frame is to be a keyframe.
    pub fn keyframe(&mut self) {
        self.owed = true;
    }
}

/// What the camera did, for the log and the tests.
#[derive(Default)]
pub struct Stats {
    /// Pictures the camera gave, and frames encoded and sent from them.
    pub pictures: AtomicU32,
    pub frames: AtomicU32,
    pub keyframes: AtomicU32,
    pub bytes: AtomicU64,
}

/// The camera, being read and encoded. Dropping it lets go of the camera.
pub struct Camera {
    quit: Arc<AtomicBool>,
    key: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub stats: Arc<Stats>,
    /// The camera's name and what it gives, and the encoder, as the log says them.
    #[allow(dead_code)]
    pub device: String,
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        say!("camera: stopped after {} frames from {} pictures ({} keyframes, {} KB)", self.stats.frames.load(Ordering::Relaxed), self.stats.pictures.load(Ordering::Relaxed), self.stats.keyframes.load(Ordering::Relaxed), self.stats.bytes.load(Ordering::Relaxed) / 1024);
    }
}

impl Camera {
    /// Open the camera and call `on_frame` with every encoded frame (its bytes, and whether it
    /// is a keyframe) on a thread of the camera's own. Fails if there is no camera, Windows
    /// keeps it from apps, it gives no picture within a few seconds, or nothing can encode it.
    /// This waits for the first picture: call it off the window's thread.
    pub fn start(on_frame: impl FnMut(&[u8], bool) + Send + 'static) -> Result<Camera> {
        let quit = Arc::new(AtomicBool::new(false));
        let key = Arc::new(AtomicBool::new(true));
        let stats = Arc::new(Stats::default());
        let (ready_tx, ready_rx) = mpsc::channel::<Result<String>>();
        let (q, k, s) = (quit.clone(), key.clone(), stats.clone());
        let thread = std::thread::Builder::new().name("camera".into()).spawn(move || {
            if let Err(e) = run(&q, &k, &s, &ready_tx, Box::new(on_frame)) {
                if ready_tx.send(Err(anyhow!("{e:#}"))).is_err() {
                    say!("camera: stopped: {e:#}");
                }
            }
        })?;
        let stop = |thread: JoinHandle<()>| {
            quit.store(true, Ordering::Relaxed);
            let _ = thread.join();
        };
        match ready_rx.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok(device)) => {
                say!("camera: {device}");
                Ok(Camera { quit, key, thread: Some(thread), stats, device })
            }
            Ok(Err(e)) => {
                stop(thread);
                Err(e)
            }
            Err(_) => {
                stop(thread);
                bail!("the camera gave no picture")
            }
        }
    }

    /// The computer lost a frame: the next one is a keyframe.
    pub fn keyframe(&self) {
        self.key.store(true, Ordering::Relaxed);
    }
}

/// The cameras Windows lists, by name, in its order.
#[allow(dead_code)] // For the tests and the log.
pub fn cameras() -> Result<Vec<String>> {
    // SAFETY: Media Foundation's calls as documented; the list Windows allocates is walked
    // within its count and given back.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)?;
        let list = devices()?;
        Ok(list.iter().map(|a| name_of(a)).collect())
    }
}

/// Windows' video capture devices.
unsafe fn devices() -> Result<Vec<IMFActivate>> {
    // SAFETY: as `cameras`.
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes = attributes.context("no attributes")?;
        attributes.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
        let (mut array, mut count) = (ptr::null_mut(), 0u32);
        MFEnumDeviceSources(&attributes, &mut array, &mut count).context("list the cameras")?;
        let mut out = Vec::new();
        for i in 0..count as usize {
            if let Some(a) = (*array.add(i)).take() {
                out.push(a);
            }
        }
        if !array.is_null() {
            CoTaskMemFree(Some(array as *const _));
        }
        Ok(out)
    }
}

fn name_of(activate: &IMFActivate) -> String {
    let (mut name, mut len) = (PWSTR::null(), 0u32);
    // SAFETY: Windows allocates the string, which is read and given back.
    unsafe {
        if activate.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut name, &mut len).is_err() || name.is_null() {
            return "a camera".into();
        }
        let text = name.to_string().unwrap_or_default();
        CoTaskMemFree(Some(name.0 as *const _));
        text
    }
}

fn subtype_name(g: &windows::core::GUID) -> String {
    match *g {
        x if x == MFVideoFormat_NV12 => "NV12".into(),
        x if x == MFVideoFormat_YUY2 => "YUY2".into(),
        x if x == MFVideoFormat_MJPG => "MJPG".into(),
        x => {
            // A FourCC in the GUID's first part.
            let b = x.data1.to_le_bytes();
            if b.iter().all(|c| c.is_ascii_graphic()) { String::from_utf8_lossy(&b).into_owned() } else { format!("{x:?}") }
        }
    }
}

/// The camera's own picture layouts: the one closest to 1280 by 720 at 30 a second, a raw one
/// before a JPEG one of the same size and rate.
unsafe fn choose(reader: &IMFSourceReader, stream: u32) -> Result<(IMFMediaType, u32, u32, f64, String)> {
    // SAFETY: plain Media Foundation queries on a live reader.
    unsafe {
        let mut best: Option<(i64, IMFMediaType, u32, u32, f64, String)> = None;
        for i in 0.. {
            let Ok(t) = reader.GetNativeMediaType(stream, i) else { break };
            let size = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
            let (w, h) = ((size >> 32) as u32, size as u32);
            let rate = t.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(0);
            let fps = if rate as u32 == 0 { 0.0 } else { (rate >> 32) as f64 / (rate as u32) as f64 };
            let subtype = t.GetGUID(&MF_MT_SUBTYPE).map(|g| subtype_name(&g)).unwrap_or_default();
            if w == 0 || h == 0 {
                continue;
            }
            // Less is better: far from 720 lines, below 30 a second, and JPEG, in that order.
            let lines = (h as i64 - MAX.1 as i64).abs() + if h < MAX.1 { 2000 } else { 0 };
            let slow = if fps + 0.5 < FPS as f64 { 1 } else { 0 };
            let score = slow * 1_000_000 + lines * 10 + if subtype == "MJPG" { 1 } else { 0 } + if fps > 61.0 { 2 } else { 0 };
            if best.as_ref().is_none_or(|b| score < b.0) {
                best = Some((score, t, w, h, fps, subtype));
            }
        }
        let (_, t, w, h, fps, subtype) = best.context("the camera offers no picture layout")?;
        Ok((t, w, h, fps, subtype))
    }
}

fn denied(e: &windows::core::Error) -> bool {
    e.code().0 as u32 == 0x8007_0005
}

fn run(quit: &AtomicBool, key: &AtomicBool, stats: &Stats, ready: &mpsc::Sender<Result<String>>, mut on_frame: Box<dyn FnMut(&[u8], bool) + Send>) -> Result<()> {
    const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
    // SAFETY: Media Foundation's calls as documented, on this thread, which initialises COM for
    // itself; each sample's buffer is read within the length it says it holds, then unlocked.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).context("start Media Foundation")?;
        let list = devices()?;
        let activate = list.first().context("this PC has no camera")?;
        let name = name_of(activate);
        let source: IMFMediaSource = activate.ActivateObject().map_err(|e| if denied(&e) { anyhow!("Windows keeps the camera from apps (Settings, Privacy & security, Camera)") } else { anyhow!("open {name}: {e}") })?;
        let result = (|| -> Result<()> {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 2)?;
            let attributes = attributes.context("no attributes")?;
            // Decoding JPEG, converting to NV12 and scaling, inside the reader.
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
            attributes.SetUINT32(&MF_LOW_LATENCY, 1)?;
            let reader = MFCreateSourceReaderFromMediaSource(&source, &attributes).context("read the camera")?;
            let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
            reader.SetStreamSelection(STREAM, true)?;
            let (native, nw, nh, fps, subtype) = choose(&reader, STREAM)?;
            reader.SetCurrentMediaType(STREAM, None, &native).context("set the camera's picture layout")?;
            let (w, h) = fit(nw, nh);
            let wanted = MFCreateMediaType()?;
            wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            wanted.SetUINT64(&MF_MT_FRAME_SIZE, (w as u64) << 32 | h as u64)?;
            reader.SetCurrentMediaType(STREAM, None, &wanted).with_context(|| format!("the camera's pictures can't be had as NV12 at {w}x{h}"))?;
            let mut size = (w, h);
            let mut stride = 0usize;
            let mut encoder: Option<H264> = None;
            // The camera's own time of the last picture used, in 100 ns units.
            let mut last: Option<i64> = None;
            // When the last keyframe went: one every two seconds or so, by the clock (a camera
            // in a dim room gives fewer pictures than it says, so counting them is not enough).
            let mut keyed = Instant::now();
            let mut said = format!("{name}, {nw}x{nh} {subtype} at {fps:.0} a second, sent as {w}x{h}");
            let mut told = false;
            while !quit.load(Ordering::Relaxed) {
                let (mut flags, mut time, mut sample) = (0u32, 0i64, None);
                if let Err(e) = reader.ReadSample(STREAM, 0, None, Some(&mut flags), Some(&mut time), Some(&mut sample)) {
                    if denied(&e) {
                        bail!("Windows keeps the camera from apps (Settings, Privacy & security, Camera)");
                    }
                    bail!("the camera stopped: {e}");
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    bail!("the camera stopped giving pictures");
                }
                if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 || stride == 0 {
                    let now = reader.GetCurrentMediaType(STREAM)?;
                    let s = now.GetUINT64(&MF_MT_FRAME_SIZE)?;
                    size = ((s >> 32) as u32 & !1, s as u32 & !1);
                    stride = now.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|v| v as i32).unwrap_or(0).unsigned_abs() as usize;
                    stride = stride.max(1);
                    if encoder.as_ref().is_some_and(|e| (e.width, e.height) != size) {
                        encoder = None;
                    }
                }
                let Some(sample) = sample else { continue };
                stats.pictures.fetch_add(1, Ordering::Relaxed);
                // A camera faster than the stream: the extra pictures are left out, by the
                // camera's own times (when they arrive here is bunched).
                if time > 0 && last.is_some_and(|at| (0..10_000_000 / (FPS as i64 + 2)).contains(&(time - at))) {
                    continue;
                }
                last = Some(time);
                let buffer = sample.ConvertToContiguousBuffer()?;
                let (mut data, mut length) = (ptr::null_mut(), 0u32);
                buffer.Lock(&mut data, None, Some(&mut length))?;
                let bytes = std::slice::from_raw_parts(data, length as usize);
                let (pw, ph) = (size.0 as usize, size.1 as usize);
                // Rows `stride` apart (at least the width); the chroma after the luma's rows,
                // which may be more than the picture's if the buffer is padded.
                let row = if stride >= pw { stride } else { pw };
                let rows = (bytes.len() / row) * 2 / 3;
                let result = if rows < ph {
                    Err(anyhow!("the camera's buffer ({} bytes) is smaller than a {pw}x{ph} picture", bytes.len()))
                } else {
                    let picture = Nv12 { width: size.0, height: size.1, y: &bytes[..row * ph], y_stride: row, uv: &bytes[row * rows..], uv_stride: row };
                    if encoder.is_none() {
                        let e = H264::open(size.0, size.1)?;
                        said = format!("{said}, encoded by {}", e.name);
                        encoder = Some(e);
                    }
                    let e = encoder.as_mut().unwrap();
                    if key.swap(false, Ordering::Relaxed) || keyed.elapsed() >= Duration::from_secs(2) {
                        keyed = Instant::now();
                        e.keyframe();
                    }
                    e.encode(&picture, false, &mut |frame, is_key| {
                        stats.frames.fetch_add(1, Ordering::Relaxed);
                        stats.bytes.fetch_add(frame.len() as u64, Ordering::Relaxed);
                        if is_key {
                            stats.keyframes.fetch_add(1, Ordering::Relaxed);
                        }
                        on_frame(frame, is_key);
                    })
                };
                let _ = buffer.Unlock();
                result?;
                // The first picture through is what the caller is waiting for.
                if !told {
                    told = true;
                    let _ = ready.send(Ok(said.clone()));
                }
            }
            Ok(())
        })();
        let _ = source.Shutdown();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A moving test picture in NV12.
    fn picture(width: u32, height: u32, n: u32) -> (Vec<u8>, Vec<u8>) {
        let (w, h) = (width as usize, height as usize);
        let mut y = vec![0u8; w * h];
        for row in 0..h {
            for x in 0..w {
                y[row * w + x] = ((x + row + n as usize * 7) % 220 + 16) as u8;
            }
        }
        let mut uv = vec![128u8; w * h / 2];
        for (i, b) in uv.iter_mut().enumerate() {
            *b = (64 + (i / 2 + n as usize * 3) % 128) as u8;
        }
        (y, uv)
    }

    /// Decode a list of Annex-B frames with FFmpeg's own H.264 decoder: how many pictures came
    /// out and how many frames it refused.
    fn decode(frames: &[Vec<u8>]) -> (usize, usize) {
        // SAFETY: FFmpeg's decoder, as decode.rs uses it.
        unsafe {
            let codec = ffi::avcodec_find_decoder(ffi::AVCodecID::AV_CODEC_ID_H264);
            let mut ctx = ffi::avcodec_alloc_context3(codec);
            (*ctx).err_recognition = ffi::AV_EF_EXPLODE as i32;
            assert!(ffi::avcodec_open2(ctx, codec, ptr::null_mut()) >= 0);
            let (mut packet, mut frame) = (ffi::av_packet_alloc(), ffi::av_frame_alloc());
            let (mut pictures, mut refused) = (0, 0);
            for data in frames {
                ffi::av_new_packet(packet, data.len() as i32);
                ptr::copy_nonoverlapping(data.as_ptr(), (*packet).data, data.len());
                if ffi::avcodec_send_packet(ctx, packet) < 0 {
                    refused += 1;
                }
                ffi::av_packet_unref(packet);
                while ffi::avcodec_receive_frame(ctx, frame) >= 0 {
                    if (*frame).decode_error_flags != 0 {
                        refused += 1;
                    }
                    pictures += 1;
                    ffi::av_frame_unref(frame);
                }
            }
            ffi::avcodec_send_packet(ctx, ptr::null());
            while ffi::avcodec_receive_frame(ctx, frame) >= 0 {
                pictures += 1;
                ffi::av_frame_unref(frame);
            }
            ffi::av_frame_free(&mut frame);
            ffi::av_packet_free(&mut packet);
            ffi::avcodec_free_context(&mut ctx);
            (pictures, refused)
        }
    }

    /// Each encoder this PC can open gives what the protocol asks for: a keyframe first and
    /// whenever one is asked for, each with its parameter sets in front, and no frame held
    /// back; and what it gives decodes, from the start and from the keyframe asked for. (One
    /// test, because it chooses the encoder through the environment.) Each encoder's frames are
    /// written to `camera-synthetic-<encoder>-<n>.h264` in the folder `REMOTEMAN_TEST_OUT`
    /// names, if it names one.
    #[test]
    fn every_encoder_here_keeps_to_the_protocol() {
        let mut tried = Vec::new();
        for which in 0..ENCODERS.len() {
            let (w, h) = (640, 480);
            let Ok(mut encoder) = H264::open_one(which, w, h) else { continue };
            let name = encoder.name;
            let mut frames: Vec<(Vec<u8>, bool)> = Vec::new();
            for n in 0..40u32 {
                let (y, uv) = picture(w, h, n);
                let p = Nv12 { width: w, height: h, y: &y, y_stride: w as usize, uv: &uv, uv_stride: w as usize };
                encoder.encode(&p, n == 20, &mut |data, key| frames.push((data.to_vec(), key))).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            }
            let keys: Vec<usize> = frames.iter().enumerate().filter(|(_, f)| f.1).map(|(i, _)| i).collect();
            assert_eq!(frames.len(), 40, "{name} ({which}) held frames back: {} out of 40, keyframes at {keys:?}", frames.len());
            // A keyframe first and where one was asked for; an encoder may add its own (OpenH264
            // does at what it takes for a change of scene), which the protocol allows.
            assert!(keys.first() == Some(&0) && keys.contains(&20), "{name} ({which}): keyframes at {keys:?}");
            for &i in &keys {
                let data = &frames[i].0;
                assert!(data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1]), "{name}: not Annex-B");
                assert!(has_parameter_sets(data), "{name}: keyframe {i} has no parameter sets");
            }
            let all: Vec<Vec<u8>> = frames.iter().map(|f| f.0.clone()).collect();
            let (pictures, refused) = decode(&all);
            assert_eq!((pictures, refused), (40, 0), "{name}: decoded from the start");
            let (pictures, refused) = decode(&all[20..]);
            assert_eq!((pictures, refused), (20, 0), "{name}: decoded from the keyframe asked for");
            if let Some(dir) = test_out() {
                let _ = std::fs::write(dir.join(format!("camera-synthetic-{name}-{which}.h264")), all.concat());
            }
            println!("{name} ({which}): 40 frames, keyframes at {keys:?} with parameter sets, all 40 decode, and 20 from the second keyframe");
            tried.push(name);
        }
        assert!(!tried.is_empty(), "no encoder opened on this PC");
    }

    #[test]
    fn fits_within_the_protocols_size() {
        assert_eq!(fit(1280, 720), (1280, 720));
        assert_eq!(fit(640, 480), (640, 480));
        assert_eq!(fit(1920, 1080), (1280, 720));
        assert_eq!(fit(1080, 1920), (404, 720));
        assert_eq!(fit(1600, 1200), (960, 720));
        assert_eq!(fit(641, 481), (640, 480));
    }

    /// The real camera for four seconds, a keyframe asked for after two: what Windows gives is
    /// encoded, and what comes out decodes. Written to `camera-real.h264` in the folder
    /// `REMOTEMAN_TEST_OUT` names, if it names one (it is a recording of the room).
    /// `cargo test -p remoteman-viewer-windows --lib real_camera -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn the_real_camera_is_encoded() {
        println!("cameras: {:?}", cameras());
        let frames = Arc::new(std::sync::Mutex::new(Vec::<(Vec<u8>, bool)>::new()));
        let f = frames.clone();
        let camera = Camera::start(move |data, key| f.lock().unwrap().push((data.to_vec(), key))).unwrap_or_else(|e| panic!("the camera: {e:#}"));
        println!("camera: {}", camera.device);
        let stats = camera.stats.clone();
        std::thread::sleep(Duration::from_secs(2));
        let asked_at = frames.lock().unwrap().len();
        camera.keyframe();
        std::thread::sleep(Duration::from_secs(2));
        drop(camera);
        println!("camera: it gave {} pictures, {} were encoded", stats.pictures.load(std::sync::atomic::Ordering::Relaxed), stats.frames.load(std::sync::atomic::Ordering::Relaxed));
        let frames = std::mem::take(&mut *frames.lock().unwrap());
        let keys: Vec<usize> = frames.iter().enumerate().filter(|(_, f)| f.1).map(|(i, _)| i).collect();
        println!("camera: {} frames in 4 s, keyframes at {keys:?} (one asked for at frame {asked_at})", frames.len());
        assert!(frames.len() >= 60, "fewer than 15 frames a second");
        assert!(keys.first() == Some(&0), "the first frame isn't a keyframe");
        assert!(keys.iter().any(|&k| k >= asked_at && k <= asked_at + 3), "no keyframe where one was asked for");
        for &k in &keys {
            assert!(has_parameter_sets(&frames[k].0), "keyframe {k} has no parameter sets");
        }
        let all: Vec<Vec<u8>> = frames.iter().map(|f| f.0.clone()).collect();
        let (pictures, refused) = decode(&all);
        println!("camera: FFmpeg decoded {pictures} pictures, refused {refused}");
        assert_eq!(refused, 0);
        if let Some(dir) = test_out() {
            let _ = std::fs::write(dir.join("camera-real.h264"), all.concat());
        }
    }

    /// Where a test may leave what it made, if `REMOTEMAN_TEST_OUT` says.
    fn test_out() -> Option<std::path::PathBuf> {
        let dir = std::path::PathBuf::from(std::env::var_os("REMOTEMAN_TEST_OUT")?);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }
}
