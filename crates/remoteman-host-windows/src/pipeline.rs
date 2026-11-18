//! The picture path: the driver's shared texture → one Direct3D conversion → the encoder → the
//! client. One thread does all of it, because the device's context is one thread's at a time
//! and the encoder works on the same device. Sending waits while the network is behind, and
//! while it waits the driver goes on replacing the frame in its channel with newer ones: frames
//! are skipped before encoding, never after (every frame refers to the one before).
//!
//! No frame, no send: a still desktop costs nothing. The last converted picture is kept in the
//! encoder's input, so a keyframe or a recovery frame can be made from it when a client asks
//! and nothing on the desktop has changed.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use remoteman_core::host::{Accept, Link};
use remoteman_core::{Message, Mode};
use remoteman_encode::FrameEncoder;
use remoteman_encode::recovery::{Recovery, Sent};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED};
use windows::Win32::System::Performance::QueryPerformanceCounter;
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};

use crate::capture::{Gpu, Source, Take};
use crate::convert::{self, Converter};
use crate::display;
use crate::encode;
use crate::vdd::{Driver, Metadata};

pub struct Settings {
    pub driver: Arc<Driver>,
    /// The modes the client decodes and this host encodes, the client's favourite first. The
    /// first one this GPU's encoder opens for is used.
    pub modes: Vec<Mode>,
    pub fps: u32,
    pub bitrate: u32,
    /// Where on the desktop to draw a dot for a moment, to make Windows show a frame when the
    /// channel has none yet.
    pub nudge_at: (i32, i32),
    /// The size the monitor was just brought to, if it was: the channel of that size is the one
    /// to wait for, since for a moment after a mode change the driver still has the old one.
    pub expect: Option<(u32, u32)>,
    /// Send nothing until [`Pipeline::release`]: for a picture that replaces another, which the
    /// client must be told of before its first frame.
    pub held: bool,
    /// Called when the picture's size changes under a running stream (someone changed the
    /// monitor's mode), after the client has been told.
    pub resized: Box<dyn Fn(u32, u32) + Send>,
}

/// What the picture turned out to be.
#[derive(Debug, Clone, Copy)]
pub struct Started {
    pub mode: Mode,
    pub width: u32,
    pub height: u32,
}

/// An event the picture's thread waits on beside the driver's.
struct Wake(HANDLE);
// SAFETY: an event handle; setting it is allowed from any thread.
unsafe impl Send for Wake {}
unsafe impl Sync for Wake {}
impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: ours, and not used again.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct Control {
    stop: AtomicBool,
    held: AtomicBool,
    keyframe: AtomicBool,
    /// A loss report not yet acted on: the first frame the client missed, and the newest it
    /// had seen when it said so.
    lost: Mutex<Option<(u32, u32)>>,
    /// The bitrate wanted, in bits a second; the encoder follows it at its next turn.
    bitrate: AtomicU32,
    wake: Wake,
}

impl Control {
    fn wake(&self) {
        // SAFETY: a live event.
        let _ = unsafe { SetEvent(self.wake.0) };
    }
}

pub struct Pipeline {
    control: Arc<Control>,
    thread: Option<JoinHandle<()>>,
}

impl Pipeline {
    /// Start streaming the driver's monitor to `link`. Returns once the first picture's size
    /// and mode are known (or it is clear there will be none).
    pub fn start(settings: Settings, link: Link) -> Result<(Self, Started)> {
        // SAFETY: an unnamed event that resets itself.
        let wake = Wake(unsafe { CreateEventW(None, false, false, None) }.context("CreateEvent")?);
        let control = Arc::new(Control { stop: AtomicBool::new(false), held: AtomicBool::new(settings.held), keyframe: AtomicBool::new(true), lost: Mutex::new(None), bitrate: AtomicU32::new(settings.bitrate), wake });
        let (ready_tx, ready_rx) = mpsc::channel::<Result<Started>>();
        let c = control.clone();
        let thread = std::thread::Builder::new().name("picture".into()).spawn(move || {
            let picture = match Picture::open(&settings) {
                Ok(p) => {
                    let _ = ready_tx.send(Ok(Started { mode: p.mode, width: p.width, height: p.height }));
                    p
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            if let Err(e) = run(picture, &settings, &link, &c) {
                tracing::error!("the picture stopped: {e:#}");
            }
        })?;
        let started = ready_rx.recv().context("the picture's thread died")??;
        Ok((Self { control, thread: Some(thread) }, started))
    }

    /// Let a pipeline that was started held back send.
    pub fn release(&self) {
        self.control.held.store(false, Ordering::Relaxed);
        self.control.wake();
    }

    /// Make the next frame a keyframe.
    pub fn request_keyframe(&self) {
        self.control.keyframe.store(true, Ordering::Relaxed);
        self.control.wake();
    }

    /// Encode at this many bits a second from now on.
    pub fn set_bitrate(&self, bitrate: u32) {
        self.control.bitrate.store(bitrate, Ordering::Relaxed);
        self.control.wake();
    }

    /// The client missed frames from `first` on, and had seen up to `last` when it said so:
    /// recover at the next frame.
    pub fn report_lost(&self, first: u32, last: u32) {
        {
            // Of reports not yet acted on, keep the earliest loss and the newest sighting.
            let mut lost = self.control.lost.lock().unwrap();
            *lost = Some(lost.map_or((first, last), |(f, l)| (f.min(first), l.max(last))));
        }
        self.control.wake();
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::Relaxed);
        self.control.wake();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Everything one picture is made with. In this order because they are dropped in it: the
/// encoder before the texture it reads, and all of them before the device.
struct Picture {
    encoder: Box<dyn FrameEncoder>,
    recovery: Recovery,
    converter: Converter,
    source: Source,
    gpu: Gpu,
    mode: Mode,
    width: u32,
    height: u32,
    /// The encoder's input holds a picture: there is something to encode again.
    have: bool,
    /// Take the newest frame even if it was taken before (a new client, or a new channel).
    again: bool,
}

/// The driver's frame channel, waiting for it if it isn't there yet: the driver makes it at
/// the first frame after it was first asked about, so the desktop is made to draw something.
fn wait_for_channel(s: &Settings, patience: Duration) -> Result<crate::vdd::Channel> {
    let began = Instant::now();
    let mut tries = 0u32;
    loop {
        if let Some(channel) = display::channel(&s.driver)? {
            let size = channel.snapshot().map(|m| (m.width, m.height));
            // Not the size the monitor is in now: the channel of the swap chain Windows is
            // about to take away. Give the new one three seconds, then take what there is.
            if s.expect.is_none() || size == s.expect || began.elapsed() > Duration::from_secs(3) {
                if tries > 0 {
                    tracing::info!(after_ms = began.elapsed().as_millis() as u64, tries, ?size, "the frame channel is there");
                }
                return Ok(channel);
            }
        }
        if tries == 0 {
            tracing::info!("the driver has no frame channel yet (it makes one at the next frame Windows shows); making the desktop draw");
        }
        ensure!(began.elapsed() < patience, "the driver's frame channel didn't appear in {} s: no frame was shown on its monitor (is the monitor part of the desktop, and is this process in the desktop's session?)", patience.as_secs());
        tries += 1;
        display::nudge(s.nudge_at);
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A converter and an encoder for the picture `meta` describes, in the first of `modes` the
/// encoder opens for.
fn build(gpu: &Gpu, meta: &Metadata, s: &Settings, bitrate: u32) -> Result<(Converter, Box<dyn FrameEncoder>, Mode)> {
    let (width, height) = (meta.width, meta.height);
    ensure!(width % 2 == 0 && height % 2 == 0 && width >= 64 && height >= 64, "the monitor is {width}x{height}; sizes must be even and at least 64");
    let (input, note) = convert::input_for(meta.format, meta.is_hdr != 0)?;
    if let Some(note) = note {
        tracing::warn!("{note}");
    }
    let converter = Converter::new(gpu, width, height, input)?;
    let mut refused = Vec::new();
    for &mode in &s.modes {
        match encode::open(gpu, &converter.output, &encode::Settings { mode, width, height, fps: s.fps, bitrate }) {
            Ok(encoder) => {
                tracing::info!(?mode, width, height, fps = s.fps, bitrate, format = meta.format, hdr = meta.is_hdr, ?input, "encoder ready ({})", encoder.note());
                return Ok((converter, encoder, mode));
            }
            Err(e) => {
                tracing::warn!(?mode, "{e:#}");
                refused.push(format!("{mode:?}: {e:#}"));
            }
        }
    }
    bail!("no encoder opened: {}", refused.join("; "))
}

impl Picture {
    fn open(s: &Settings) -> Result<Self> {
        let channel = wait_for_channel(s, Duration::from_secs(6))?;
        let meta = channel.snapshot().context("the frame channel's metadata isn't readable (never whole, or not this driver's)")?;
        tracing::info!(
            width = meta.width,
            height = meta.height,
            format = meta.format,
            hdr = meta.is_hdr,
            slots = channel.slots.len(),
            frames = meta.frame_counter,
            generation = meta.generation(),
            flags = format!("{:#x}", channel.flags),
            "frame channel"
        );
        let gpu = Gpu::on_adapter((meta.luid_low, meta.luid_high))?;
        let source = Source::open(&gpu, channel)?;
        let (converter, encoder, mode) = build(&gpu, &source.meta, s, s.bitrate)?;
        let recovery = Recovery::new(encoder.short_term(), encoder.long_term(), s.fps);
        Ok(Self { encoder, recovery, converter, gpu, mode, width: source.meta.width, height: source.meta.height, source, have: false, again: true })
    }

    /// The channel in hand is dead (or may be): take the driver's current one. If the picture
    /// is another size or format now, the stream starts afresh as that. False: the driver has
    /// no channel at the moment (between two modes); try again later.
    fn reopen(&mut self, s: &Settings, link: &Link, bitrate: u32) -> Result<bool> {
        let Some(channel) = display::channel(&s.driver)? else { return Ok(false) };
        let source = Source::open(&self.gpu, channel)?;
        let (old, new) = (self.source.meta, source.meta);
        if (old.width, old.height, old.format, old.is_hdr) != (new.width, new.height, new.format, new.is_hdr) {
            tracing::info!(from = format!("{}x{}", old.width, old.height), to = format!("{}x{}", new.width, new.height), format = new.format, hdr = new.is_hdr, "the monitor changed; the picture starts afresh");
            // Only this mode: the client has its decoder set up for it.
            let only = Settings { driver: s.driver.clone(), modes: vec![self.mode], fps: s.fps, bitrate, nudge_at: s.nudge_at, expect: None, held: false, resized: Box::new(|_, _| {}) };
            let (converter, encoder, mode) = build(&self.gpu, &new, &only, bitrate)?;
            // The old encoder goes before the texture it reads from.
            self.encoder = encoder;
            self.converter = converter;
            self.recovery = Recovery::new(self.encoder.short_term(), self.encoder.long_term(), s.fps);
            (self.width, self.height, self.have) = (new.width, new.height, false);
            if !link.restart(Accept { mode, width: new.width, height: new.height, refresh: s.fps, colour: "bt709".into() }) {
                bail!("the client is gone");
            }
            (s.resized)(new.width, new.height);
        } else {
            tracing::info!("the driver's frame channel was replaced; carrying on with the new one");
        }
        self.source = source;
        self.again = true;
        Ok(true)
    }
}

/// How long ago Windows meant a frame to be shown, in microseconds; 0 if that is still to come
/// or makes no sense.
fn age_us(meta: &Metadata) -> u64 {
    let mut now = 0i64;
    // SAFETY: a live out-parameter.
    if unsafe { QueryPerformanceCounter(&mut now) }.is_err() || meta.qpc_frequency == 0 || meta.present_qpc == 0 {
        return 0;
    }
    let ticks = (now as u64).saturating_sub(meta.present_qpc);
    let us = ticks.saturating_mul(1_000_000) / meta.qpc_frequency;
    if us > 100_000 { 0 } else { us }
}

fn run(mut p: Picture, s: &Settings, link: &Link, control: &Control) -> Result<()> {
    let mut rate = s.bitrate;
    let (mut frames, mut keys, mut repeats) = (0u64, 0u64, 0u64);
    // What the encoder made of its rate: bytes in all, the largest frame, and its quantiser
    // summed (for the average) and at the last frame.
    let (mut bytes, mut largest, mut qp_sum, mut qp_last) = (0u64, 0usize, 0u64, 0u32);
    let began = Instant::now();
    // When a frame last came, and when the channel was last checked for having been replaced.
    let (mut quiet_since, mut checked) = (Instant::now(), Instant::now());
    // The stream must begin with a keyframe after it starts afresh.
    let mut afresh = false;
    let mut wait_failed = false;
    loop {
        let held = control.held.load(Ordering::Relaxed);
        if !p.again || held {
            // Held back: wait for the word only, and leave the driver's frames where they are.
            let handles = [control.wake.0, p.source.channel().ready];
            // SAFETY: live events. The timeout is for the housekeeping below.
            let waited = unsafe { WaitForMultipleObjects(&handles[..if held { 1 } else { 2 }], false, 100) };
            if waited == WAIT_FAILED {
                // Not to become a loop that eats a core in silence.
                if !std::mem::replace(&mut wait_failed, true) {
                    tracing::warn!("waiting for the driver's frame event failed ({}); looking for frames ten times a second instead. Said once.", windows::core::Error::from_thread());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if control.stop.load(Ordering::Relaxed) {
            break;
        }
        if control.held.load(Ordering::Relaxed) {
            continue;
        }
        // A new bitrate takes effect here, with no keyframe.
        let want = control.bitrate.load(Ordering::Relaxed);
        if want != rate {
            match p.encoder.set_bitrate(want) {
                Ok(()) => {
                    tracing::info!(from = rate, to = want, "bitrate");
                    rate = want;
                    link.send_message(Message::Status { bitrate: Some(rate) });
                }
                Err(e) => {
                    tracing::warn!("couldn't change the bitrate: {e:#}");
                    control.bitrate.store(rate, Ordering::Relaxed);
                }
            }
        }

        let again = std::mem::take(&mut p.again);
        let taken = {
            let Picture { source, converter, gpu, .. } = &mut p;
            source.take(gpu, again, |view, meta| {
                converter.convert(gpu, view);
                link.now_us().saturating_sub(age_us(meta))
            })?
        };
        let stamp = match taken {
            Take::Frame(us) => {
                p.have = true;
                quiet_since = Instant::now();
                Some(us)
            }
            // Asked for the newest frame whatever its state and didn't get it: the driver
            // is writing into that very texture. Ask again in a moment.
            Take::Nothing if again => {
                p.again = true;
                std::thread::sleep(Duration::from_millis(5));
                None
            }
            Take::Nothing => None,
            Take::Stale => {
                afresh |= p.reopen(s, link, rate)?;
                checked = Instant::now();
                None
            }
        };
        // Windows gives the monitor a new swap chain on a mode change and at other times of its
        // own choosing, and the driver then makes a new channel without a word in the old one.
        // So when frames stop, ask now and then whether the channel is still the driver's.
        if stamp.is_none() && quiet_since.elapsed() > Duration::from_secs(1) && checked.elapsed() > Duration::from_secs(1) {
            checked = Instant::now();
            match display::channel(&s.driver) {
                Ok(Some(current)) if !current.same_as(p.source.channel()) => {
                    drop(current);
                    afresh |= p.reopen(s, link, rate)?;
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("asking after the frame channel: {e:#}"),
            }
        }
        if afresh && p.have {
            control.keyframe.store(true, Ordering::Relaxed);
            *control.lost.lock().unwrap() = None;
            afresh = false;
        }
        if !p.have {
            continue;
        }

        // Encode when there is a new picture, or when the client needs an answer (a keyframe,
        // a loss) and the desktop is still: then the last picture is encoded again.
        let asked = control.keyframe.load(Ordering::Relaxed) || control.lost.lock().unwrap().is_some();
        let capture_us = match stamp {
            Some(us) => us,
            None if asked => {
                repeats += 1;
                link.now_us()
            }
            None => continue,
        };
        let idr = control.keyframe.swap(false, Ordering::Relaxed);
        let reported = control.lost.lock().unwrap().take();
        let Picture { encoder, recovery, .. } = &mut p;
        let n = recovery.number();
        let plan = recovery.plan(idr, reported, &mut |t| encoder.invalidate(t).inspect_err(|e| tracing::debug!(t, "invalidate: {e:#}")));
        if reported.is_some() {
            tracing::debug!(?reported, n, ?plan, "loss report");
        }
        let made = encoder.encode_how(0, n, plan.how).context("encode")?;
        let sent = recovery.made(&plan, made.key, made.marked, made.referred);
        frames += 1;
        keys += made.key as u64;
        bytes += made.data.len() as u64;
        largest = largest.max(made.data.len());
        qp_sum += made.qp as u64;
        qp_last = made.qp;
        let delivered = match sent {
            Sent::Key => link.send_frame(made.data, true, capture_us),
            Sent::Recovery => link.send_recovery_frame(made.data, capture_us),
            Sent::Delta => link.send_frame(made.data, false, capture_us),
        };
        if !delivered {
            break;
        }
    }
    let driver = p.source.counts();
    tracing::info!(
        frames,
        keyframes = keys,
        encoded_again = repeats,
        seconds = began.elapsed().as_secs(),
        kilobytes = bytes / 1000,
        mbit_per_s = format!("{:.1}", bytes as f64 * 8.0 / 1e6 / began.elapsed().as_secs_f64().max(0.001)),
        target_mbit = format!("{:.1}", rate as f64 / 1e6),
        largest_frame_bytes = largest,
        qp_average = format!("{:.1}", qp_sum as f64 / frames.max(1) as f64),
        qp_last,
        driver_frames = driver.map(|d| d.0),
        driver_replaced_unread = driver.map(|d| d.1),
        driver_dropped_held = driver.map(|d| d.2),
        driver_dropped_failed = driver.map(|d| d.3),
        "picture over (the driver's counts are since its channel was made)"
    );
    let t = &p.recovery.tally;
    if t.reports > 0 {
        tracing::info!(
            reports = t.reports,
            recovered = t.recovered,
            long_term = t.long_term,
            early = t.early,
            too_old = t.too_old,
            refused = t.refused,
            unasked = t.unasked,
            unsure = t.unsure,
            behind = ?t.behind,
            "loss reports: answered from a recent frame, from a long-term frame, came before the last answer could arrive, too old (keyframe), refused by the encoder (keyframe); keyframes the encoder made unasked; long-term answers built from another frame than asked; how many frames behind each report was"
        );
    }
    Ok(())
}
