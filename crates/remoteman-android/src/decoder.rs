//! The picture: MediaCodec decoding straight into the app's Surface.
//!
//! Frames are queued on the core's thread as they arrive (never waiting long: a full decoder
//! drops the frame and the app asks for a keyframe), and a thread of our own takes decoded
//! pictures out and shows each one at once.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ndk::media::media_codec::{DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection};
use ndk::media::media_format::MediaFormat;
use ndk::native_window::NativeWindow;
use remoteman_core::{Codec, Frame, StreamInfo};

/// What the app's statistics line shows.
#[derive(Default)]
pub struct Stats {
    /// Frames handed to the decoder.
    pub queued: AtomicU32,
    pub decoded: AtomicU32,
    pub dropped: AtomicU32,
    /// The newest shown picture's age: from the host's capture to its release to the screen, in
    /// microseconds; -1 until the clocks have been compared.
    pub age_us: AtomicI32,
}

/// AMediaCodec may be fed on one thread and drained on another.
struct Shared(MediaCodec);
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

/// When each frame in the decoder arrived, and how old it was then, by its capture time.
type InFlight = Mutex<VecDeque<(u64, Option<i64>, Instant)>>;

struct Live {
    codec: Arc<Shared>,
    stop: Arc<AtomicBool>,
    drain: Option<JoinHandle<()>>,
    in_flight: Arc<InFlight>,
}

pub struct Decoder {
    window: NativeWindow,
    stats: Arc<Stats>,
    info: Option<StreamInfo>,
    live: Option<Live>,
    /// No decoder for this stream could be made: drop frames without asking for keyframes.
    broken: bool,
    /// A frame was dropped here, so the ones after it can't be decoded: drop them too until the
    /// keyframe that was asked for.
    waiting_for_key: bool,
}

const BUFFER_FLAG_KEY_FRAME: u32 = 1;

impl Decoder {
    pub fn new(window: NativeWindow) -> Self {
        let stats = Arc::new(Stats::default());
        stats.age_us.store(-1, Ordering::Relaxed);
        Decoder { window, stats, info: None, live: None, broken: false, waiting_for_key: false }
    }

    pub fn stats(&self) -> Arc<Stats> {
        self.stats.clone()
    }

    /// The stream is (re)starting: the decoder is made afresh at its first keyframe.
    pub fn stream(&mut self, info: &StreamInfo) {
        self.stop();
        self.info = Some(info.clone());
        self.broken = false;
        self.waiting_for_key = false;
    }

    /// Queue one frame. False when it couldn't be, and the stream needs a keyframe.
    pub fn frame(&mut self, frame: &Frame, age_us: Option<i64>) -> bool {
        if self.broken {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if self.waiting_for_key {
            if !frame.key {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            self.waiting_for_key = false;
        }
        if self.live.is_none() {
            if !frame.key {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            match self.start(frame) {
                Ok(live) => self.live = Some(live),
                Err(e) => {
                    log::error!("no decoder: {e:#}");
                    self.broken = true;
                    return true;
                }
            }
        }
        let live = self.live.as_ref().unwrap();
        match queue(&live.codec.0, frame) {
            Ok(true) => {
                let q = self.stats.queued.fetch_add(1, Ordering::Relaxed) + 1;
                if q <= 3 || q % 120 == 0 {
                    log::info!("queued {q} frames (this one {} bytes, key {}), shown {}", frame.data.len(), frame.key, self.stats.decoded.load(Ordering::Relaxed));
                }
                let mut f = live.in_flight.lock().unwrap();
                if f.len() > 64 {
                    f.pop_front();
                }
                f.push_back((frame.capture_us, age_us, Instant::now()));
                true
            }
            Ok(false) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                self.waiting_for_key = true;
                false
            }
            Err(e) => {
                log::warn!("decoder failed: {e:#}; starting it again at the next keyframe");
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                self.stop();
                false
            }
        }
    }

    fn start(&self, first: &Frame) -> anyhow::Result<Live> {
        let info = self.info.as_ref().ok_or_else(|| anyhow::anyhow!("a frame before the stream"))?;
        let mime = match info.mode.codec {
            Codec::Hevc => "video/hevc",
            Codec::Av1 => "video/av01",
        };
        let codec = MediaCodec::from_decoder_type(mime).ok_or_else(|| anyhow::anyhow!("this device has no {mime} decoder"))?;
        let mut format = MediaFormat::new();
        format.set_str("mime", mime);
        format.set_i32("width", info.width as i32);
        format.set_i32("height", info.height as i32);
        format.set_i32("max-input-size", 8 << 20);
        // Show each picture as soon as it is decoded (Android 11 and later; ignored before).
        format.set_i32("low-latency", 1);
        // Realtime.
        format.set_i32("priority", 0);
        if info.mode.codec == Codec::Hevc {
            let sets = parameter_sets(&first.data);
            if !sets.is_empty() {
                format.set_buffer("csd-0", &sets);
            }
        }
        codec.configure(&format, Some(&self.window), MediaCodecDirection::Decoder).map_err(|e| anyhow::anyhow!("configure {mime}: {e:?}"))?;
        codec.start().map_err(|e| anyhow::anyhow!("start {mime}: {e:?}"))?;
        log::info!("decoding {mime} {}x{} with {}", info.width, info.height, codec.name().unwrap_or_default());
        let codec = Arc::new(Shared(codec));
        let stop = Arc::new(AtomicBool::new(false));
        let in_flight: Arc<InFlight> = Arc::default();
        let drain = {
            let (codec, stop, in_flight, stats) = (codec.clone(), stop.clone(), in_flight.clone(), self.stats.clone());
            std::thread::Builder::new().name("remoteman-show".into()).spawn(move || drain(&codec.0, &stop, &in_flight, &stats))?
        };
        Ok(Live { codec, stop, drain: Some(drain), in_flight })
    }

    pub fn stop(&mut self) {
        if let Some(mut live) = self.live.take() {
            live.stop.store(true, Ordering::Relaxed);
            if let Some(t) = live.drain.take() {
                let _ = t.join();
            }
            let _ = live.codec.0.stop();
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Copy a frame into the decoder. Ok(false): it had no room.
fn queue(codec: &MediaCodec, frame: &Frame) -> anyhow::Result<bool> {
    let mut buffer = match codec.dequeue_input_buffer(Duration::from_millis(4)).map_err(|e| anyhow::anyhow!("dequeue input: {e:?}"))? {
        DequeuedInputBufferResult::Buffer(b) => b,
        DequeuedInputBufferResult::TryAgainLater => return Ok(false),
    };
    let space = buffer.buffer_mut();
    anyhow::ensure!(space.len() >= frame.data.len(), "a frame of {} bytes in an input buffer of {}", frame.data.len(), space.len());
    for (to, from) in space.iter_mut().zip(&frame.data) {
        to.write(*from);
    }
    let flags = if frame.key { BUFFER_FLAG_KEY_FRAME } else { 0 };
    codec.queue_input_buffer(buffer, 0, frame.data.len(), frame.capture_us, flags).map_err(|e| anyhow::anyhow!("queue input: {e:?}"))?;
    Ok(true)
}

fn drain(codec: &MediaCodec, stop: &AtomicBool, in_flight: &InFlight, stats: &Stats) {
    while !stop.load(Ordering::Relaxed) {
        match codec.dequeue_output_buffer(Duration::from_millis(20)) {
            Ok(DequeuedOutputBufferInfoResult::Buffer(buffer)) => {
                let pts = buffer.info().presentation_time_us() as u64;
                if let Err(e) = codec.release_output_buffer(buffer, true) {
                    log::warn!("show: {e:?}");
                }
                let n = stats.decoded.fetch_add(1, Ordering::Relaxed) + 1;
                let mut f = in_flight.lock().unwrap();
                while let Some(&(capture, age, at)) = f.front() {
                    if capture > pts {
                        break;
                    }
                    f.pop_front();
                    if capture == pts {
                        if let Some(age) = age {
                            let shown = age + at.elapsed().as_micros() as i64;
                            stats.age_us.store(shown.clamp(0, i32::MAX as i64) as i32, Ordering::Relaxed);
                        }
                    }
                }
                if n == 1 || n % 300 == 0 {
                    log::info!("shown {n} pictures, newest {} ms old", stats.age_us.load(Ordering::Relaxed) as f64 / 1000.0);
                }
            }
            Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => log::info!("decoder output: {:?}", codec.output_format()),
            Ok(_) => {}
            Err(e) => {
                log::warn!("dequeue output: {e:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// HEVC's VPS, SPS and PPS from an Annex-B access unit, each with its start code, as
/// MediaCodec wants them in `csd-0`.
fn parameter_sets(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nal_units(data) {
        if let Some(&b) = nal.first() {
            if (32..=34).contains(&((b >> 1) & 0x3f)) {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(nal);
            }
        }
    }
    out
}

fn nal_units(d: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 2 < d.len() {
        if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map_or(d.len(), |&n| n - 3);
        while end > s && d[end - 1] == 0 {
            end -= 1;
        }
        out.push(&d[s..end]);
    }
    out
}
