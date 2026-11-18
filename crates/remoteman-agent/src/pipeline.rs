//! The picture path: PipeWire DMA-BUF → one Vulkan pass → NVENC. Two threads: PipeWire's, which
//! converts each frame while the compositor's buffer is still valid, and the encoder's.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use remoteman_capture::{self as capture, Event, QuitSender};
use remoteman_encode::{Codec, Config, Encoder};
use remoteman_gpu::{Chroma, Converter};
use remoteman_core::host::Link;
use remoteman_core::{self as control, CursorShape};

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
}

pub struct Pipeline {
    quit: Option<QuitSender<()>>,
    keyframe: Arc<AtomicBool>,
    /// The first frame the client reported missing, plus one (0: nothing reported).
    lost: Arc<AtomicU32>,
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
        let Settings { node, width, height, fps, scale, mode, bitrate, cursor } = settings;
        let chroma = match mode.chroma {
            control::Chroma::C420 => Chroma::C420,
            control::Chroma::C444 => Chroma::C444,
        };
        let codec = match mode.codec {
            control::Codec::Hevc => Codec::Hevc,
            control::Codec::Av1 => Codec::Av1,
        };
        anyhow::ensure!(mode.depth == 10, "only 10-bit is wired up");
        let mut conv = Converter::new(width, height, chroma, SLOTS).context("Vulkan converter")?;
        let modifiers = conv.modifiers()?;
        anyhow::ensure!(!modifiers.is_empty(), "this GPU offers no DMA-BUF layout for the compositor's format");
        let pitch = conv.pitch;
        let inputs: Vec<(i32, u64)> = conv.slots.iter_mut().map(|s| (std::mem::replace(&mut s.fd, -1), s.size)).collect();

        // Two may wait and one may be encoding, so with four slots the next to write is free.
        let (tx, rx) = mpsc::sync_channel::<Converted>(2);
        let keyframe = Arc::new(AtomicBool::new(true));
        let skipped = Arc::new(AtomicU32::new(0));

        let (ready_tx, ready_rx) = mpsc::channel::<Result<String>>();
        let key = keyframe.clone();
        let lost = Arc::new(AtomicU32::new(0));
        let lost_r = lost.clone();
        let out = link.clone();
        let encoder = std::thread::Builder::new().name("encode".into()).spawn(move || {
            let opened = (|| {
                let mut enc = Encoder::new(&Config { codec, chroma444: chroma == Chroma::C444, width, height, fps, bitrate, preset: 1 })?;
                for (fd, size) in inputs {
                    enc.add_input(fd, size, pitch)?;
                }
                anyhow::Ok(enc)
            })();
            let mut enc = match opened {
                Ok(e) => {
                    let _ = ready_tx.send(Ok(e.note.clone()));
                    e
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // How many earlier frames the encoder keeps to refer to (as the wrapper sets it up).
            const REFERENCES: u64 = 4;
            let mut n = 0u64;
            while let Ok(c) = rx.recv() {
                let mut force = key.swap(false, Ordering::Relaxed);
                let mut recovery = false;
                let reported = lost_r.swap(0, Ordering::Relaxed);
                if reported != 0 && !force {
                    // The client missed frames from `first` on, and nothing sent since is any
                    // use to it either. If the encoder still holds a frame from before the loss,
                    // drop its references to everything after and carry on from there: no
                    // keyframe, so no burst. Otherwise start afresh.
                    let first = (reported - 1) as u64;
                    if first < n && n - first < REFERENCES && (first..n).all(|t| enc.invalidate(t).is_ok()) {
                        recovery = true;
                    } else {
                        force = true;
                    }
                }
                match enc.encode(c.slot, n, force) {
                    Ok((data, key)) => {
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
        })?;
        let note = ready_rx.recv().context("encoder thread died")?.context("NVENC")?;
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
                    if (f.width, f.height) != (conv.width, conv.height) {
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
        Ok(Self { quit: Some(quit), keyframe, lost, skipped, scale: actual_scale, threads: vec![capture, encoder] })
    }

    /// Make the next frame a keyframe.
    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }

    /// The client missed frames from `first` on: recover at the next frame.
    pub fn report_lost(&self, first: u32) {
        // Keep the earliest report not yet acted on.
        let mut old = self.lost.load(Ordering::Relaxed);
        loop {
            let new = if old == 0 { first + 1 } else { old.min(first + 1) };
            match self.lost.compare_exchange_weak(old, new, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(now) => old = now,
            }
        }
    }
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
