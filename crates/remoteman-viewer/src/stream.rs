//! A running session: the core hands frames to a decode thread, which leaves the newest picture
//! where the window's frame clock picks it up.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use anyhow::Result;
use remoteman_core::{Client, CursorShape, Display, Frame, Handler, Mode, Options, StreamInfo};

use crate::decode::{Decoder, Picture};

/// What the window reads.
#[derive(Default)]
pub struct Shared {
    /// The newest decoded picture, until the window takes it.
    pub picture: Mutex<Option<Picture>>,
    /// The host's newest cursor shape, until the window takes it.
    pub cursor: Mutex<Option<CursorShape>>,
    /// Why the session ended, once it has.
    pub closed: Mutex<Option<String>>,
    /// What is decoding, e.g. "av1 (cuda) 2752×2064".
    pub decoder: Mutex<String>,
    pub decoded: AtomicU32,
    /// Pictures the window never showed because a newer one arrived first.
    pub replaced: AtomicU32,
    /// The newest frame's age when it was decoded, in microseconds (0: not known yet).
    pub age_us: AtomicU64,
    /// The longest decode (with the copy from the GPU) since it was last read, in microseconds.
    pub decode_us_max: AtomicU32,
    /// Set by the window when GTK refuses a DMA-BUF picture: copy them down from now on.
    pub no_dmabuf: std::sync::atomic::AtomicBool,
}

enum Msg {
    Stream(StreamInfo),
    Frame(Vec<u8>, Option<i64>),
}

struct Feed {
    tx: mpsc::SyncSender<Msg>,
    shared: Arc<Shared>,
}

impl Handler for Feed {
    fn stream(&mut self, info: &StreamInfo) {
        let _ = self.tx.send(Msg::Stream(info.clone()));
    }

    fn frame(&mut self, frame: &Frame, age_us: Option<i64>) {
        // Every frame leans on the one before, so none may be skipped here: if the decoder
        // falls behind, this waits, and the loss shows up upstream as a keyframe request.
        let _ = self.tx.send(Msg::Frame(frame.data.clone(), age_us));
    }

    fn cursor_shape(&mut self, shape: &CursorShape) {
        *self.shared.cursor.lock().unwrap() = Some(shape.clone());
    }

    fn closed(&mut self, reason: &str) {
        *self.shared.closed.lock().unwrap() = Some(reason.to_owned());
    }
}

pub struct Session {
    pub client: Client,
    pub shared: Arc<Shared>,
}

pub struct Request {
    pub host: String,
    pub state: PathBuf,
    pub pair: bool,
    pub display: Display,
    pub decoders: Vec<Mode>,
}

impl Session {
    /// Connect (this blocks) and start decoding.
    pub fn start(r: Request) -> Result<Self> {
        let shared = Arc::new(Shared::default());
        let (tx, rx) = mpsc::sync_channel::<Msg>(8);
        let s = shared.clone();
        std::thread::Builder::new().name("decode".into()).spawn(move || {
            let mut decoder: Option<Decoder> = None;
            while let Ok(msg) = rx.recv() {
                match msg {
                    Msg::Stream(info) => match Decoder::new(info.mode.codec) {
                        Ok(d) => {
                            *s.decoder.lock().unwrap() = format!("{} {}×{}", d.name, info.width, info.height);
                            decoder = Some(d);
                        }
                        Err(e) => {
                            *s.closed.lock().unwrap() = Some(format!("{e:#}"));
                            return;
                        }
                    },
                    Msg::Frame(data, age) => {
                        let Some(d) = decoder.as_mut() else { continue };
                        if s.no_dmabuf.load(Ordering::Relaxed) {
                            d.stop_exporting();
                        }
                        let t = std::time::Instant::now();
                        match d.decode(&data) {
                            Ok(Some(picture)) => {
                                let took = t.elapsed().as_micros() as u32;
                                s.decode_us_max.fetch_max(took, Ordering::Relaxed);
                                s.decoded.fetch_add(1, Ordering::Relaxed);
                                if let Some(a) = age {
                                    s.age_us.store((a.max(0) as u64) + took as u64, Ordering::Relaxed);
                                }
                                if s.picture.lock().unwrap().replace(picture).is_some() {
                                    s.replaced.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                *s.closed.lock().unwrap() = Some(format!("{e:#}"));
                                return;
                            }
                        }
                    }
                }
            }
        })?;
        let hostname = std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "Linux".into());
        let options = Options { host: r.host, state: r.state, name: hostname, pair: r.pair, display: r.display, decoders: r.decoders, cursor: true };
        let client = Client::connect(options, Feed { tx, shared: shared.clone() })?;
        Ok(Self { client, shared })
    }
}
