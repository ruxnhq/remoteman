//! The computer's sound: Opus packets from the core's thread into a jitter buffer, played by an
//! AAudio stream in low-latency mode. Nothing of it crosses into Java but the switch and the
//! figures.
//!
//! The playout buffer is `remoteman-audio`'s [`JitterBuffer`] (the Linux and Windows viewers'
//! too): it holds [`JITTER_FRAMES`] of 10 ms before it starts, orders packets by `seq`, makes up
//! the frames of lost ones with Opus's concealment, and catches up when too much is held. AAudio's
//! data callback pulls from it, so a frame is decoded only when the device asks for it.
//!
//! When the output goes away (headphones pulled, a Bluetooth headset coming or going), AAudio
//! ends the stream with "disconnected"; a new one is opened on the new output, on a thread of
//! its own, as AAudio asks.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use anyhow::{Context, Result};
use ndk::audio::{
    AudioCallbackResult, AudioContentType, AudioDirection, AudioError, AudioFormat, AudioPerformanceMode, AudioSharingMode, AudioStream, AudioStreamBuilder,
    AudioStreamState, AudioUsage, Clockid,
};
use remoteman_audio::{Channels, JITTER_FRAMES, JitterBuffer, Packet, SAMPLE_RATE};

/// What the callback and the core's thread share.
struct Shared {
    jitter: Mutex<JitterBuffer>,
    channels: usize,
    /// Another app has the sound (a call, say): play silence, keep taking packets.
    muted: AtomicBool,
    /// The player is being let go: a disconnected stream isn't reopened.
    closing: AtomicBool,
    /// Times the stream was opened anew on another output.
    reopened: AtomicU64,
    /// The loudest sample handed to the device since the figures were last read (f32 bits):
    /// sound, not just frames, is reaching it.
    peak: AtomicU32,
}

/// An AAudio stream is used from one thread at a time here (behind a mutex).
struct Stream(AudioStream);
unsafe impl Send for Stream {}

/// Plays one sender's packets. Dropping it stops the sound.
pub struct Player {
    shared: Arc<Shared>,
    stream: Arc<Mutex<Option<Stream>>>,
    pub channels: Channels,
}

/// What a player did so far and how late its sound is.
pub struct Figures {
    pub played: u64,
    pub concealed: u64,
    pub late: u64,
    pub skipped: u64,
    /// Held in the jitter buffer now, in µs.
    pub held_us: i64,
    /// From being handed to AAudio to being heard, in µs (-1 not known yet): AAudio's own
    /// buffer plus the device's path (which for Bluetooth is most of it).
    pub output_us: i64,
    /// Times the device found nothing to play since the stream was opened.
    pub underruns: i32,
    /// AAudio's buffer, in frames, and the device's burst.
    pub buffer_frames: i32,
    pub burst_frames: i32,
    pub exclusive: bool,
    pub low_latency: bool,
    /// The loudest sample played since the last figures, 0 to 1.
    pub peak: f32,
    /// What the playout buffer holds before it plays, in frames of 10 ms.
    pub depth: usize,
}

impl Player {
    pub fn open(channels: Channels) -> Result<Self> {
        let shared = Arc::new(Shared {
            jitter: Mutex::new(JitterBuffer::new(channels, JITTER_FRAMES)?),
            channels: channels.count(),
            muted: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            reopened: AtomicU64::new(0),
            peak: AtomicU32::new(0),
        });
        let stream = Arc::new(Mutex::new(None));
        *stream.lock().unwrap() = Some(open_stream(&shared, Arc::downgrade(&stream))?);
        Ok(Self { shared, stream, channels })
    }

    pub fn push(&self, seq: u32, capture_us: u64, data: &[u8]) {
        if let Ok(mut j) = self.shared.jitter.lock() {
            j.push(Packet { seq, capture_us, data: data.to_vec() });
        }
    }

    pub fn mute(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }

    pub fn figures(&self) -> Figures {
        let (stats, held, depth) = {
            let j = self.shared.jitter.lock().unwrap();
            (j.stats(), j.held_samples(), j.depth())
        };
        let mut f = Figures {
            played: stats.played,
            concealed: stats.concealed,
            late: stats.late,
            skipped: stats.skipped,
            held_us: held as i64 * 1_000_000 / SAMPLE_RATE as i64,
            output_us: -1,
            underruns: 0,
            buffer_frames: 0,
            burst_frames: 0,
            exclusive: false,
            low_latency: false,
            peak: f32::from_bits(self.shared.peak.swap(0, Ordering::Relaxed)),
            depth,
        };
        if let Some(Stream(s)) = self.stream.lock().unwrap().as_ref() {
            f.underruns = s.x_run_count();
            f.buffer_frames = s.buffer_size_in_frames();
            f.burst_frames = s.frames_per_burst();
            f.exclusive = s.sharing_mode() == AudioSharingMode::Exclusive;
            f.low_latency = s.performance_mode() == AudioPerformanceMode::LowLatency;
            // Where the device is now, from where it was at a known time: what was written
            // and isn't heard yet is the output's delay.
            if let Ok(ts) = s.timestamp(Clockid::Monotonic) {
                let rate = s.sample_rate() as i64;
                let now = monotonic_ns();
                let heard = ts.frame_position + (now - ts.time_nanoseconds) * rate / 1_000_000_000;
                let ahead = s.frames_written() - heard;
                if ahead >= 0 && rate > 0 {
                    f.output_us = ahead * 1_000_000 / rate;
                }
            }
        }
        f
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Relaxed);
        if let Some(Stream(s)) = self.stream.lock().unwrap().take() {
            stop(&s);
            // Dropping it closes it.
        }
        let s = self.shared.jitter.lock().unwrap().stats();
        log::info!("sound: {} packets played, {} concealed, {} late, {} skipped", s.played, s.concealed, s.late, s.skipped);
    }
}

/// Stop a stream and wait (a little) until it has: stopping is asynchronous, and the stream and
/// its callbacks are freed when it is dropped right after.
fn stop(s: &AudioStream) {
    let _ = s.request_stop();
    let mut state = s.state();
    for _ in 0..4 {
        if !matches!(state, AudioStreamState::Started | AudioStreamState::Starting | AudioStreamState::Stopping) {
            break;
        }
        match s.wait_for_state_change(state, 50_000_000) {
            Ok(next) => state = next,
            Err(_) => break,
        }
    }
}

fn monotonic_ns() -> i64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    // 32-bit Android (armeabi-v7a) has 32-bit fields here: widen before multiplying.
    t.tv_sec as i64 * 1_000_000_000 + t.tv_nsec as i64
}

fn open_stream(shared: &Arc<Shared>, holder: Weak<Mutex<Option<Stream>>>) -> Result<Stream> {
    let channels = shared.channels;
    let data = shared.clone();
    let errors = Arc::downgrade(shared);
    let stream = AudioStreamBuilder::new()
        .context("AAudio")?
        .direction(AudioDirection::Output)
        .format(AudioFormat::PCM_Float)
        .channel_count(channels as i32)
        .sample_rate(SAMPLE_RATE as i32)
        // Exclusive (MMAP) where the device has it; AAudio falls back to shared by itself.
        .sharing_mode(AudioSharingMode::Exclusive)
        .performance_mode(AudioPerformanceMode::LowLatency)
        // A game's sound: the media volume, and other apps duck for it as for a game.
        .usage(AudioUsage::Game)
        .content_type(AudioContentType::Music)
        .data_callback(Box::new(move |_stream: &AudioStream, out: *mut c_void, frames: i32| {
            // Called on AAudio's real-time thread. The lock is only ever held as long as it
            // takes to file a packet or decode one frame.
            let out = unsafe { std::slice::from_raw_parts_mut(out as *mut f32, frames.max(0) as usize * channels) };
            match data.jitter.lock() {
                Ok(mut j) => j.pull(out),
                Err(_) => out.fill(0.0),
            }
            if data.muted.load(Ordering::Relaxed) {
                out.fill(0.0);
            }
            let loudest = out.iter().fold(0f32, |m, s| m.max(s.abs()));
            data.peak.fetch_max(loudest.to_bits(), Ordering::Relaxed);
            AudioCallbackResult::Continue
        }))
        .error_callback(Box::new(move |_stream: &AudioStream, error: AudioError| {
            log::warn!("sound: the output stream ended: {error:?}");
            if !matches!(error, AudioError::Disconnected) {
                return;
            }
            let (Some(shared), holder) = (errors.upgrade(), holder.clone()) else { return };
            // Not from this callback: AAudio wants another thread to close and reopen.
            std::thread::spawn(move || reopen(shared, holder));
        }))
        .open_stream()
        .context("opening the AAudio stream")?;
    // Two bursts: the least that plays without gaps on most devices, as AAudio's own
    // examples start with. Underruns are counted, not chased: the jitter buffer is what absorbs
    // the network.
    let burst = stream.frames_per_burst();
    if burst > 0 {
        let _ = stream.set_buffer_size_in_frames(burst * 2);
    }
    log::info!(
        "sound: AAudio {} Hz, {} channels, {:?}, {:?}, burst {} frames, buffer {} of {} frames, device {}",
        stream.sample_rate(),
        stream.channel_count(),
        stream.sharing_mode(),
        stream.performance_mode(),
        burst,
        stream.buffer_size_in_frames(),
        stream.buffer_capacity_in_frames(),
        stream.device_id()
    );
    stream.request_start().context("starting the AAudio stream")?;
    Ok(Stream(stream))
}

/// The output went away: close the old stream and play on whatever the output is now.
fn reopen(shared: Arc<Shared>, holder: Weak<Mutex<Option<Stream>>>) {
    let Some(holder) = holder.upgrade() else { return };
    let mut slot = holder.lock().unwrap();
    if shared.closing.load(Ordering::Relaxed) {
        return;
    }
    if let Some(Stream(old)) = slot.take() {
        stop(&old);
    }
    match open_stream(&shared, Arc::downgrade(&holder)) {
        Ok(s) => {
            shared.reopened.fetch_add(1, Ordering::Relaxed);
            log::info!("sound: playing on the new output");
            *slot = Some(s);
        }
        Err(e) => log::warn!("sound: no output to play on: {e:#}"),
    }
}
