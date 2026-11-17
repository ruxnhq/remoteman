//! The picture path: PipeWire DMA-BUF → one Vulkan pass → NVENC. Two threads: PipeWire's, which
//! converts each frame while the compositor's buffer is still valid, and the encoder's.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, Result};
use remoteman_capture::{self as capture, Event, QuitSender};
use remoteman_encode::{Codec, Config, Encoder};
use remoteman_gpu::{Chroma, Converter};
use remoteman_proto::control;

/// One encoded frame on its way to the network.
pub struct Encoded {
    pub data: Vec<u8>,
    pub key: bool,
    /// When it was captured, in microseconds on the session's clock.
    pub capture_us: u64,
}

pub struct Settings {
    pub node: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The HiDPI scale the client wants the desktop drawn at.
    pub scale: f32,
    pub mode: control::Mode,
    pub bitrate: u32,
}

pub struct Pipeline {
    quit: Option<QuitSender<()>>,
    keyframe: Arc<AtomicBool>,
    /// Frames the compositor made that were skipped because the encoder was still busy.
    pub skipped: Arc<AtomicU32>,
    threads: Vec<JoinHandle<()>>,
}

struct Converted {
    slot: usize,
    capture_us: u64,
}

impl Pipeline {
    /// Start streaming `settings.node`. Encoded frames go to `out`; when the network can't take
    /// them, the encoder waits and frames are skipped before encoding (never after, since every
    /// frame refers to the one before).
    pub fn start(settings: Settings, clock: Instant, out: tokio::sync::mpsc::Sender<Encoded>) -> Result<Self> {
        const SLOTS: usize = 4;
        let Settings { node, width, height, fps, scale, mode, bitrate } = settings;
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
            let mut n = 0u64;
            while let Ok(c) = rx.recv() {
                let force = key.swap(false, Ordering::Relaxed);
                match enc.encode(c.slot, n, force) {
                    Ok((data, key)) => {
                        n += 1;
                        if out.blocking_send(Encoded { data: data.to_vec(), key, capture_us: c.capture_us }).is_err() {
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

        let (quit, quit_rx) = capture::quit_channel();
        let skip = skipped.clone();
        let capture = std::thread::Builder::new().name("capture".into()).spawn(move || {
            let mut next = 0usize;
            let on_event = Box::new(move |ev: Event| match ev {
                Event::Format { width, height, modifier } => {
                    tracing::info!(width, height, modifier = format!("{modifier:#x}"), "stream format");
                    conv.reset_images();
                }
                Event::Frame(f) => {
                    if (f.width, f.height) != (conv.width, conv.height) {
                        return;
                    }
                    let capture_us = clock.elapsed().as_micros() as u64;
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
            if let Err(e) = capture::run(node, (width, height), fps, Some(scale), &modifiers, on_event, quit_rx) {
                tracing::error!("capture: {e:#}");
            }
        })?;
        Ok(Self { quit: Some(quit), keyframe, skipped, threads: vec![capture, encoder] })
    }

    /// Make the next frame a keyframe.
    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
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
