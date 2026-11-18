//! A running session: the core hands frames to a decode thread, which leaves the newest picture
//! where the draw thread picks it up. As the Linux viewer's `stream.rs`.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use anyhow::Result;
use remoteman_core::{Client, CursorShape, Frame, Handler, StreamInfo};

use crate::decode::{Decoder, Picture};
use crate::gpu::Gpu;

/// What the threads share.
#[derive(Default)]
pub struct Shared {
    /// The newest decoded picture, until the draw thread takes it; `fresh` wakes that thread.
    pub picture: Mutex<Option<Picture>>,
    pub fresh: Condvar,
    /// The host's newest cursor shape, until the window takes it.
    pub cursor: Mutex<Option<CursorShape>>,
    /// Whether the host's cursor is on this display at all, and where it last was.
    pub cursor_gone: AtomicBool,
    pub cursor_at: Mutex<(f32, f32)>,
    /// Why the session ended, once it has.
    pub closed: Mutex<Option<String>>,
    /// What is decoding, e.g. "av1 (d3d11va) 1920x1080".
    pub decoder: Mutex<String>,
    /// The stream's size in pixels, once the host has said: 0 until then. Width in the high half.
    pub stream_size: AtomicU64,
    /// The window's client area in pixels, the same way.
    pub client_size: AtomicU64,
    /// The window, for waking it from other threads (0 until there is one).
    pub hwnd: AtomicIsize,
    /// Set when it is time to leave.
    pub quit: AtomicBool,

    // Since the figures were last printed:
    pub decoded: AtomicU32,
    pub presented: AtomicU32,
    /// Pictures never shown because a newer one arrived first.
    pub replaced: AtomicU32,
    /// The longest decode, in microseconds.
    pub decode_us_max: AtomicU32,
    // The newest frame's age when it was decoded and when it had been handed to the swap
    // chain (it is on the glass a refresh or so later), in microseconds (0: not known yet).
    pub age_decoded_us: AtomicU64,
    pub age_presented_us: AtomicU64,
    // Since the start:
    pub decoded_total: AtomicU32,
    pub presented_total: AtomicU32,
    pub replaced_total: AtomicU32,
}

pub fn pack(width: u32, height: u32) -> u64 {
    (width as u64) << 32 | height as u64
}

pub fn unpack(size: u64) -> (u32, u32) {
    ((size >> 32) as u32, size as u32)
}

enum Msg {
    Stream(StreamInfo),
    Frame(Vec<u8>, Option<i64>),
}

pub struct Feed {
    tx: mpsc::SyncSender<Msg>,
    shared: Arc<Shared>,
}

impl Feed {
    /// Has the window look at the cursor again; it owns the cursor, so only it may change it.
    fn wake_window(&self) {
        crate::window::wake(self.shared.hwnd.load(Ordering::Relaxed));
    }
}

impl Handler for Feed {
    fn stream(&mut self, info: &StreamInfo) {
        say!("stream: {:?} {}x{} at {} Hz, {}", info.mode, info.width, info.height, info.refresh, info.colour);
        self.shared.stream_size.store(pack(info.width, info.height), Ordering::Relaxed);
        let _ = self.tx.send(Msg::Stream(info.clone()));
    }

    fn frame(&mut self, frame: &Frame, age_us: Option<i64>) {
        // Every frame leans on the one before, so none may be skipped here: if the decoder
        // falls behind, this waits, and the loss shows up upstream as a keyframe request.
        let _ = self.tx.send(Msg::Frame(frame.data.clone(), age_us));
    }

    fn cursor_shape(&mut self, shape: &CursorShape) {
        *self.shared.cursor.lock().unwrap() = Some(shape.clone());
        self.wake_window();
    }

    fn cursor_at(&mut self, x: f32, y: f32, visible: bool) {
        *self.shared.cursor_at.lock().unwrap() = (x, y);
        if self.shared.cursor_gone.swap(!visible, Ordering::Relaxed) == visible {
            self.wake_window();
        }
    }

    fn closed(&mut self, reason: &str) {
        *self.shared.closed.lock().unwrap() = Some(reason.to_owned());
    }
}

/// Start the decode thread; what comes back is handed to `Client::connect`.
pub fn start(gpu: &Gpu) -> Result<(Arc<Shared>, Feed)> {
    let shared = Arc::new(Shared::default());
    let (tx, rx) = mpsc::sync_channel::<Msg>(8);
    let (s, gpu) = (shared.clone(), gpu.clone());
    std::thread::Builder::new().name("decode".into()).spawn(move || {
        if let Err(e) = decode_loop(&s, &gpu, rx) {
            say!("decoding stopped: {e:#}");
            *s.closed.lock().unwrap() = Some(format!("{e:#}"));
        }
    })?;
    Ok((shared.clone(), Feed { tx, shared }))
}

fn decode_loop(s: &Shared, gpu: &Gpu, rx: mpsc::Receiver<Msg>) -> Result<()> {
    let mut decoder: Option<Decoder> = None;
    let mut info: Option<StreamInfo> = None;
    // Whether decoder textures may also be sampled: tried once, given up for good if the
    // driver refuses. `REMOTEMAN_SAMPLE=0` starts without, to test the copying path on a card
    // that doesn't need it.
    let mut sampled = std::env::var("REMOTEMAN_SAMPLE").as_deref() != Ok("0");
    let mut first = true;
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Stream(i) => {
                decoder = Some(Decoder::new(gpu, i.mode.codec, sampled)?);
                first = true;
                info = Some(i);
            }
            Msg::Frame(data, age) => {
                let (Some(d), Some(i)) = (decoder.as_mut(), info.as_ref()) else { continue };
                let t = std::time::Instant::now();
                let mut decoded = d.decode(&data, age);
                if first && sampled && decoded.is_err() {
                    // The card is only asked for its textures at the first frame, a keyframe.
                    // Try that frame again with textures for decoding alone.
                    say!("no decoder textures that can be sampled ({:#}): copying each picture on the GPU instead", decoded.as_ref().err().unwrap());
                    sampled = false;
                    *d = Decoder::new(gpu, i.mode.codec, false)?;
                    decoded = d.decode(&data, age);
                }
                let Some(mut picture) = decoded? else { continue };
                if first {
                    first = false;
                    *s.decoder.lock().unwrap() = format!("{} {}x{}", d.name, picture.width, picture.height);
                    say!("decoding: {}", s.decoder.lock().unwrap());
                }
                let took = t.elapsed().as_micros() as u32;
                s.decode_us_max.fetch_max(took, Ordering::Relaxed);
                s.decoded.fetch_add(1, Ordering::Relaxed);
                s.decoded_total.fetch_add(1, Ordering::Relaxed);
                picture.age_us = age.map(|a| a.max(0) + took as i64);
                if let Some(a) = picture.age_us {
                    s.age_decoded_us.store(a as u64, Ordering::Relaxed);
                }
                // The newest picture wins; one the draw thread hasn't taken yet is never seen.
                if s.picture.lock().unwrap().replace(picture).is_some() {
                    s.replaced.fetch_add(1, Ordering::Relaxed);
                    s.replaced_total.fetch_add(1, Ordering::Relaxed);
                }
                s.fresh.notify_one();
            }
        }
    }
    Ok(())
}

/// `--stats`: a line a second.
pub fn print_stats(shared: Arc<Shared>, client: Arc<Client>) {
    let _ = std::thread::Builder::new().name("stats".into()).spawn(move || {
        let ms = |us: u64| if us == 0 { "?".to_owned() } else { format!("{:.1}", us as f64 / 1000.0) };
        while !shared.quit.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(1));
            say!(
                "stats: decoded {}, presented {}, replaced unseen {}, age {} ms when decoded and {} ms when handed to the display, decode {:.1} ms at most, lost {}, {}",
                shared.decoded.swap(0, Ordering::Relaxed),
                shared.presented.swap(0, Ordering::Relaxed),
                shared.replaced.swap(0, Ordering::Relaxed),
                ms(shared.age_decoded_us.load(Ordering::Relaxed)),
                ms(shared.age_presented_us.load(Ordering::Relaxed)),
                shared.decode_us_max.swap(0, Ordering::Relaxed) as f64 / 1000.0,
                client.stats().lost,
                shared.decoder.lock().unwrap(),
            );
        }
    });
}
