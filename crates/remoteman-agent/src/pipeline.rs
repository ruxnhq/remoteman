//! The picture path: PipeWire DMA-BUF → one Vulkan pass → NVENC. Two threads: PipeWire's, which
//! converts each frame while the compositor's buffer is still valid, and the encoder's.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use remoteman_capture::{self as capture, Event, QuitSender};
use remoteman_encode::{Codec, Config, How};
use remoteman_gpu::Chroma;
use remoteman_core::host::Link;
use remoteman_core::{self as control, CursorShape, Message};

pub struct Settings {
    pub node: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The HiDPI scale the client wants the desktop drawn at.
    pub scale: f32,
    pub mode: control::Mode,
    pub bitrate: u32,
    /// The client draws the cursor itself: report it apart from the picture.
    pub cursor: bool,
    /// For trying things out: how many recent and long-term frames the encoder keeps.
    pub refs: Option<(u32, u32)>,
}

pub struct Pipeline {
    quit: Option<QuitSender<()>>,
    keyframe: Arc<AtomicBool>,
    /// A loss report not yet acted on: the first frame the client missed, and the newest it
    /// had seen when it said so.
    lost: Arc<Mutex<Option<(u32, u32)>>>,
    /// The bitrate wanted, in bits a second; the encoder follows it at its next frame.
    bitrate: Arc<AtomicU32>,
    /// Frames the compositor made that were skipped because the encoder was still busy.
    pub skipped: Arc<AtomicU32>,
    /// The scale the compositor draws the monitor at, as `f32` bits (what was asked for, until
    /// the compositor says what it chose).
    pub scale: Arc<AtomicU32>,
    threads: Vec<JoinHandle<()>>,
}

struct Converted {
    slot: usize,
    capture_us: u64,
}

impl Pipeline {
    /// Start streaming `settings.node` to `link`. When the network can't take frames, the encoder
    /// waits and frames are skipped before encoding (never after, since every frame refers to
    /// the one before).
    pub fn start(settings: Settings, link: Link) -> Result<Self> {
        const SLOTS: usize = 4;
        let Settings { node, width, height, fps, scale, mode, bitrate, cursor, refs } = settings;
        let chroma = match mode.chroma {
            control::Chroma::C420 => Chroma::C420,
            control::Chroma::C444 => Chroma::C444,
        };
        let codec = match mode.codec {
            control::Codec::Hevc => Codec::Hevc,
            control::Codec::Av1 => Codec::Av1,
        };
        // NVENC behind the Vulkan converter, or VA-API: whichever GPU the compositor is on.
        let (mut conv, modifiers, opener) = crate::gpu::open(width, height, mode, fps, bitrate, SLOTS)?;

        // Two may wait and one may be encoding, so with four slots the next to write is free.
        let (tx, rx) = mpsc::sync_channel::<Converted>(2);
        let keyframe = Arc::new(AtomicBool::new(true));
        let skipped = Arc::new(AtomicU32::new(0));

        let (ready_tx, ready_rx) = mpsc::channel::<Result<String>>();
        let key = keyframe.clone();
        let lost = Arc::new(Mutex::new(None::<(u32, u32)>));
        let lost_r = lost.clone();
        let wanted = Arc::new(AtomicU32::new(bitrate));
        let wanted_r = wanted.clone();
        let out = link.clone();
        let encoder = std::thread::Builder::new().name("encode".into()).spawn(move || {
            let opened = (|| {
                opener.open(&Config { codec, chroma444: chroma == Chroma::C444, width, height, fps, bitrate, preset: 1, long_term: refs.map_or(2, |r| r.1), short_term: refs.map(|r| r.0) })
            })();
            let mut enc = match opened {
                Ok(e) => {
                    let _ = ready_tx.send(Ok(e.note().to_owned()));
                    e
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // Recent frames the encoder can fall back on, and long-term ones: frames it keeps
            // when told, so that a loss reported too late for the recent ones still doesn't
            // cost a keyframe.
            let short = enc.short_term() as u64;
            let slots = (enc.long_term() as usize).min(2);
            // The long-term frames the client holds too, by frame number.
            let mut kept = [None::<u64>; 2];
            let mut kept_last = 0u64;
            // A new one four times a second: with two, one is always older than any loss
            // reported within a quarter of a second. With room for only one, twice a second: a
            // late report then finds it too new now and then, and gets a keyframe.
            let every = (fps as u64 / if slots > 1 { 4 } else { 2 }).max(8);
            let mut n = 0u64;
            // The last frame sent that a client can restart from (a keyframe or recovery point).
            let mut answer = None::<u64>;
            let mut tally = Tally::default();
            let mut rate = bitrate;
            // Where the last picture encoded is (it stays there until newer ones have come).
            let mut last = None::<usize>;
            loop {
                // A still desktop sends no frames, and a new bitrate is answered all the same.
                let c = match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(c) => Some(c),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                // A new bitrate takes effect here, with no keyframe.
                let want = wanted_r.load(Ordering::Relaxed);
                if want != rate {
                    match enc.set_bitrate(want) {
                        Ok(()) => {
                            tracing::info!(from = rate, to = want, "bitrate");
                            rate = want;
                            out.send_message(Message::Status { bitrate: Some(rate) });
                        }
                        Err(e) => {
                            tracing::warn!("couldn't change the bitrate: {e:#}");
                            wanted_r.store(rate, Ordering::Relaxed);
                        }
                    }
                }
                // Nothing new to encode. If the client is asking for something (a keyframe, or
                // frames it lost), the last picture is encoded again to answer it: on a still
                // desktop no other frame would ever come, and the client would be left with
                // nothing, or with a picture it can't use.
                let c = match c {
                    Some(c) => {
                        last = Some(c.slot);
                        c
                    }
                    None => match last {
                        Some(slot) if key.load(Ordering::Relaxed) || lost_r.lock().unwrap().is_some() => Converted { slot, capture_us: out.now_us() },
                        _ => continue,
                    },
                };
                let mut how = How { idr: key.swap(false, Ordering::Relaxed), ..How::default() };
                let mut recovery = false;
                let reported = lost_r.lock().unwrap().take();
                if let (Some((first, last)), false) = (reported, how.idr) {
                    // The client missed frames from `first` on, and nothing sent since is any
                    // use to it either.
                    let (first, last) = (first as u64, last as u64);
                    let behind = n.saturating_sub(first);
                    tally.reports += 1;
                    tally.behind[(behind as usize).min(tally.behind.len() - 1)] += 1;
                    if answer.is_some_and(|a| last < a) {
                        // Said before the answer already sent could have reached the client.
                        tally.early += 1;
                    } else {
                        // Stop referring to anything from the loss on, long-term frames
                        // included. Then carry on from the newest frame the client does have:
                        // a recent one if the encoder still holds it, else a long-term one.
                        // No keyframe, so no burst. Failing both, start afresh.
                        let dropped = first < n && (first..n).try_for_each(|t| enc.invalidate(t)).inspect_err(|e| tracing::debug!(first, n, "invalidate: {e:#}")).is_ok();
                        for k in &mut kept {
                            if k.is_some_and(|f| f >= first) {
                                *k = None;
                            }
                        }
                        let old = (0..slots).filter(|&i| kept[i].is_some()).max_by_key(|&i| kept[i]);
                        if !dropped {
                            how.idr = true;
                            tally.refused += 1;
                        } else if behind < short {
                            recovery = true;
                            tally.recovered += 1;
                        } else if let Some(slot) = old {
                            how.refer = 1 << slot;
                            recovery = true;
                            tally.long_term += 1;
                        } else {
                            how.idr = true;
                            tally.too_old += 1;
                        }
                    }
                    tracing::debug!(first, last, n, behind, recovery, ?how, "loss report");
                }
                if how.idr {
                    // A keyframe empties the encoder's store.
                    kept = [None; 2];
                }
                if slots > 0 && how.refer == 0 && (kept[..slots].contains(&None) || n - kept_last >= every) {
                    // Replace the older one (or fill an empty place).
                    how.mark = (0..slots).min_by_key(|&i| kept[i]).map(|i| i as u32);
                }
                match enc.encode_how(c.slot, n, how) {
                    Ok(made) => {
                        let (data, key) = (made.data, made.key);
                        if key && !how.idr {
                            kept = [None; 2];
                            if n > 0 {
                                tally.unasked += 1;
                            }
                        }
                        if let Some(slot) = made.marked.filter(|s| (*s as usize) < slots) {
                            kept[slot as usize] = Some(n);
                            kept_last = n;
                        }
                        if how.refer != 0 {
                            tracing::debug!(n, asked = how.refer, got = made.referred, key, "long-term answer");
                        }
                        if how.refer != 0 && !key && made.referred != 0 && made.referred != how.refer {
                            // Built from another long-term frame than the one asked for (the
                            // HEVC encoder doesn't say which it used, so nothing to compare
                            // there): it may lean on something the client lacks.
                            tally.unsure += 1;
                        }
                        if key || recovery {
                            answer = Some(n);
                        }
                        n += 1;
                        let sent = if recovery && !key { out.send_recovery_frame(data, c.capture_us) } else { out.send_frame(data, key, c.capture_us) };
                        if !sent {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::error!("encode: {e:#}");
                        break;
                    }
                }
            }
            tracing::info!(frames = n, "the picture ended");
            if tally.reports > 0 {
                let Tally { reports, recovered, long_term, early, too_old, refused, unasked, unsure, behind } = tally;
                tracing::info!(
                    reports,
                    recovered,
                    long_term,
                    early,
                    too_old,
                    refused,
                    unasked,
                    unsure,
                    ?behind,
                    "loss reports: answered from a recent frame, from a long-term frame, came before the last answer could arrive, too old (keyframe), refused by the encoder (keyframe); keyframes the encoder made unasked; long-term answers built from another frame than asked; how many frames behind each report was"
                );
            }
        })?;
        let note = ready_rx.recv().context("encoder thread died")?.context("encoder")?;
        tracing::info!(?codec, ?chroma, width, height, fps, bitrate, "encoder ready ({note})");

        let actual_scale = Arc::new(AtomicU32::new(scale.to_bits()));
        let scale_w = actual_scale.clone();
        let (quit, quit_rx) = capture::quit_channel();
        let skip = skipped.clone();
        let capture = std::thread::Builder::new().name("capture".into()).spawn(move || {
            let mut next = 0usize;
            let cursor_meta = cursor;
            let on_event = Box::new(move |ev: Event| match ev {
                Event::Scale(s) => {
                    tracing::info!(scale = s, "the compositor draws the monitor at this scale");
                    scale_w.store(s.to_bits(), Ordering::Relaxed);
                }
                Event::Cursor(c) => {
                    if let Some(s) = c.shape {
                        link.cursor_shape(CursorShape { width: s.width, height: s.height, hot_x: s.hot_x, hot_y: s.hot_y, rgba: s.rgba });
                    }
                    link.cursor_at(c.x as f32 / width as f32, c.y as f32 / height as f32, c.on_screen);
                }
                Event::Format { width, height, modifier } => {
                    tracing::info!(width, height, modifier = format!("{modifier:#x}"), "stream format");
                    conv.reset_images();
                }
                Event::Frame(f) => {
                    if (f.width, f.height) != conv.size() {
                        return;
                    }
                    let capture_us = link.now_us();
                    match conv.convert(f.fd, f.modifier, f.offset, f.stride, next) {
                        Ok(()) => {
                            if tx.try_send(Converted { slot: next, capture_us }).is_ok() {
                                next = (next + 1) % SLOTS;
                            } else {
                                skip.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(e) => tracing::error!("convert: {e:#}"),
                    }
                }
            });
            if let Err(e) = capture::run(node, (width, height), fps, Some(scale), cursor_meta, &modifiers, on_event, quit_rx) {
                tracing::error!("capture: {e:#}");
            }
        })?;
        Ok(Self { quit: Some(quit), keyframe, lost, bitrate: wanted, skipped, scale: actual_scale, threads: vec![capture, encoder] })
    }

    /// Make the next frame a keyframe.
    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }

    /// Encode at this many bits a second from the next frame on.
    pub fn set_bitrate(&self, bitrate: u32) {
        self.bitrate.store(bitrate, Ordering::Relaxed);
    }

    /// The client missed frames from `first` on, and had seen up to `last` when it said so:
    /// recover at the next frame.
    pub fn report_lost(&self, first: u32, last: u32) {
        // Of reports not yet acted on, keep the earliest loss and the newest sighting.
        let mut lost = self.lost.lock().unwrap();
        *lost = Some(lost.map_or((first, last), |(f, l)| (f.min(first), l.max(last))));
    }
}

/// What became of the client's loss reports, for the log.
#[derive(Default)]
struct Tally {
    reports: u32,
    recovered: u32,
    long_term: u32,
    early: u32,
    too_old: u32,
    refused: u32,
    unasked: u32,
    unsure: u32,
    /// How many frames had been encoded since the lost one when each report came (the last
    /// entry: that many or more).
    behind: [u32; 12],
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // Stopping PipeWire's loop ends the capture thread; its channel closing ends the encoder.
        if let Some(q) = self.quit.take() {
            let _ = q.send(());
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
