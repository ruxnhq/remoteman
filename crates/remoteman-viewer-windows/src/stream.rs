//! What the threads share. For as long as the program runs there is one window with its draw
//! thread (`Scene`); a session comes and goes (`Shared`): the core hands its frames to a decode
//! thread, which leaves the newest picture where the draw thread picks it up. As the Linux
//! viewer's `stream.rs`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use anyhow::Result;
use remoteman_core::media::{BLOB_CLIPBOARD, CLIPBOARD_LIMIT, ClipboardHeader, Packet};
use remoteman_core::{BlobEvent, CursorShape, Frame, Handler, Message, Stage, StreamInfo};

use crate::decode::{Decoder, Picture};
use crate::gpu::Gpu;

/// The window, as its threads see it.
#[derive(Default)]
pub struct Scene {
    /// The interface as it was last described, for the draw thread to draw.
    pub frame: Mutex<Option<Arc<crate::ui::Frame>>>,
    /// The session whose picture is shown, if there is one.
    pub session: Mutex<Option<Arc<Shared>>>,
    /// Counts the reasons the draw thread has been given to look again.
    pokes: Mutex<u64>,
    poked: Condvar,
    /// The window's client area in pixels: width in the high half.
    pub client_size: AtomicU64,
    /// The window, for waking it from other threads (0 until there is one).
    pub hwnd: AtomicIsize,
    /// Set when it is time to leave.
    pub quit: AtomicBool,
    /// A picture of the window to save once the frame numbered so has been drawn, and how
    /// many have been saved.
    pub shot: Mutex<Option<(PathBuf, u64)>>,
    pub shots: AtomicU32,
    /// Why drawing stopped, if it has: nothing can be shown then.
    pub failed: Mutex<Option<String>>,
}

impl Scene {
    /// Have the draw thread look again: there is a new picture or a new interface.
    pub fn poke(&self) {
        *self.pokes.lock().unwrap() += 1;
        self.poked.notify_one();
    }

    /// Wait until poked since `seen` was last brought up to date, but not for ever.
    pub fn wait(&self, seen: &mut u64, at_most: Duration) {
        let mut pokes = self.pokes.lock().unwrap();
        if *pokes == *seen {
            pokes = self.poked.wait_timeout(pokes, at_most).unwrap().0;
        }
        *seen = *pokes;
    }

    /// Have the window (from any thread) look at what has changed.
    pub fn wake_window(&self) {
        crate::window::wake(self.hwnd.load(Ordering::Relaxed));
    }
}

/// What the threads of one session share.
pub struct Shared {
    pub scene: Arc<Scene>,
    /// The newest decoded picture, until the draw thread takes it.
    pub picture: Mutex<Option<Picture>>,
    /// The host's newest cursor shape, until the window takes it.
    pub cursor: Mutex<Option<CursorShape>>,
    /// Whether the host's cursor is on this display at all, and where it last was.
    pub cursor_gone: AtomicBool,
    pub cursor_at: Mutex<(f32, f32)>,
    /// It has moved since the window last looked (for a pointer that is drawn here).
    pub cursor_moved: AtomicBool,
    /// How far connecting has come: a `Stage` as a number.
    pub stage: AtomicU8,
    /// Why the session ended, once it has.
    pub closed: Mutex<Option<String>>,
    /// What is decoding, e.g. "av1 (d3d11va) 1920x1080".
    pub decoder: Mutex<String>,
    /// The stream's size in pixels, once the host has said: 0 until then. Width in the high half.
    pub stream_size: AtomicU64,
    pub refresh: AtomicU32,
    /// What the host says it can do, when that changes during the session.
    pub features: Mutex<Option<Vec<String>>>,
    /// What the host says the picture is being encoded at, in bits a second (0: it hasn't said).
    pub bitrate: AtomicU32,
    /// The computer is asking its owner about this PC: the code both sides show, and the
    /// seconds its owner has.
    pub pairing: Mutex<Option<(String, u32)>>,
    /// Text copied on the computer, until the window puts it on this clipboard.
    pub copies: Mutex<Vec<String>>,
    /// What the computer said of files sent to it: whether it has them, and where or why not.
    pub results: Mutex<Vec<(bool, String)>>,
    /// What became of files the computer sent, until the window answers it and says so here.
    pub received: Mutex<Vec<remoteman_core::receive::Outcome>>,
    /// Where files from the computer go.
    pub folder: Mutex<PathBuf>,
    /// Plays the computer's sound, while that is on.
    pub sound: Mutex<Option<crate::sound::Player>>,
    /// The decoder was given frames and gave no picture: the window asks the computer for a
    /// keyframe (nothing was lost on the way, so nothing else will).
    pub stuck: AtomicBool,
    /// The second display's picture and figures, which come as a stream of their own
    /// (`None` in the second display's own).
    pub second: Option<Arc<Shared>>,
    /// The computer took the second display away, or couldn't make it; with why.
    pub second_gone: Mutex<Option<String>>,
    /// Number the clipboard blobs and the files sent.
    pub copy_seq: AtomicU32,
    pub file_seq: AtomicU32,

    // Since the figures were last read:
    pub decoded: AtomicU32,
    pub presented: AtomicU32,
    /// Pictures never shown because a newer one arrived first.
    pub replaced: AtomicU32,
    /// The longest decode, in microseconds.
    pub decode_us_max: AtomicU32,
    /// The bytes of video that arrived.
    pub bytes: AtomicU64,
    // The newest frame's age when it was decoded and when it had been handed to the swap
    // chain (it is on the glass a refresh or so later), in microseconds (0: not known yet).
    pub age_decoded_us: AtomicU64,
    pub age_presented_us: AtomicU64,
    // Since the start:
    pub decoded_total: AtomicU32,
    pub presented_total: AtomicU32,
    pub replaced_total: AtomicU32,
}

impl Shared {
    fn new(scene: Arc<Scene>, second: Option<Arc<Shared>>) -> Self {
        Self {
            scene,
            second,
            second_gone: Mutex::default(),
            picture: Mutex::default(),
            cursor: Mutex::default(),
            cursor_gone: AtomicBool::default(),
            cursor_at: Mutex::default(),
            cursor_moved: AtomicBool::default(),
            stage: AtomicU8::default(),
            closed: Mutex::default(),
            decoder: Mutex::default(),
            stream_size: AtomicU64::default(),
            refresh: AtomicU32::default(),
            features: Mutex::default(),
            bitrate: AtomicU32::default(),
            pairing: Mutex::default(),
            copies: Mutex::default(),
            results: Mutex::default(),
            received: Mutex::default(),
            folder: Mutex::default(),
            sound: Mutex::default(),
            stuck: AtomicBool::default(),
            copy_seq: AtomicU32::default(),
            file_seq: AtomicU32::default(),
            decoded: AtomicU32::default(),
            presented: AtomicU32::default(),
            replaced: AtomicU32::default(),
            decode_us_max: AtomicU32::default(),
            bytes: AtomicU64::default(),
            age_decoded_us: AtomicU64::default(),
            age_presented_us: AtomicU64::default(),
            decoded_total: AtomicU32::default(),
            presented_total: AtomicU32::default(),
            replaced_total: AtomicU32::default(),
        }
    }
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
    /// To the second display's decoder.
    tx2: mpsc::SyncSender<Msg>,
    shared: Arc<Shared>,
    /// Clipboard blobs on their way in: what kind, and what has come.
    arriving: std::collections::HashMap<u32, (String, Vec<u8>)>,
    files: remoteman_core::receive::Receiver,
}

impl Feed {
    /// Has the window look at the cursor again; it owns the cursor, so only it may change it.
    fn wake_window(&self) {
        self.shared.scene.wake_window();
    }
}

impl Handler for Feed {
    fn stream(&mut self, info: &StreamInfo) {
        say!("stream: {:?} {}x{} at {} Hz, {}", info.mode, info.width, info.height, info.refresh, info.colour);
        self.shared.stream_size.store(pack(info.width, info.height), Ordering::Relaxed);
        self.shared.refresh.store(info.refresh, Ordering::Relaxed);
        self.wake_window();
        let _ = self.tx.send(Msg::Stream(info.clone()));
    }

    fn frame(&mut self, frame: &Frame, age_us: Option<i64>) {
        // Every frame leans on the one before, so none may be skipped here: if the decoder
        // falls behind, this waits, and the loss shows up upstream as a keyframe request.
        self.shared.bytes.fetch_add(frame.data.len() as u64, Ordering::Relaxed);
        let _ = self.tx.send(Msg::Frame(frame.data.clone(), age_us));
    }

    fn stream2(&mut self, info: &StreamInfo) {
        say!("second display: {:?} {}x{} at {} Hz", info.mode, info.width, info.height, info.refresh);
        if let Some(second) = &self.shared.second {
            second.stream_size.store(pack(info.width, info.height), Ordering::Relaxed);
            second.refresh.store(info.refresh, Ordering::Relaxed);
        }
        let _ = self.tx2.send(Msg::Stream(info.clone()));
    }

    fn frame2(&mut self, frame: &Frame, age_us: Option<i64>) {
        let _ = self.tx2.send(Msg::Frame(frame.data.clone(), age_us));
    }

    fn stage(&mut self, stage: Stage) {
        self.shared.stage.store(stage as u8, Ordering::Relaxed);
        self.wake_window();
    }

    fn message(&mut self, message: &Message) {
        match message {
            Message::Status { bitrate: Some(bits) } => self.shared.bitrate.store(*bits, Ordering::Relaxed),
            Message::Features { features } => {
                *self.shared.features.lock().unwrap() = Some(features.clone());
                self.wake_window();
            }
            Message::Display { index: 1, on: false, message, .. } => {
                *self.shared.second_gone.lock().unwrap() = Some(message.clone());
                self.wake_window();
            }
            Message::FileResult { ok, message, .. } => {
                self.shared.results.lock().unwrap().push((*ok, message.clone()));
                self.wake_window();
            }
            _ => {}
        }
    }

    fn pairing(&mut self, code: &str, seconds: u32) {
        *self.shared.pairing.lock().unwrap() = Some((code.to_owned(), seconds));
        self.wake_window();
    }

    fn sound(&mut self, packet: &Packet) {
        if let Some(player) = self.shared.sound.lock().unwrap().as_ref() {
            player.push(packet);
        }
    }

    fn blob(&mut self, event: &BlobEvent) {
        let folder = self.shared.folder.lock().unwrap().clone();
        if let Some(outcome) = self.files.blob(event, &folder) {
            self.shared.received.lock().unwrap().push(outcome);
            self.wake_window();
        }
        match event {
            BlobEvent::Open { id, kind, header } if *kind == BLOB_CLIPBOARD => {
                if let Ok(h) = serde_json::from_str::<ClipboardHeader>(header) {
                    if h.size <= CLIPBOARD_LIMIT {
                        self.arriving.insert(*id, (h.mime, Vec::with_capacity(h.size as usize)));
                    }
                }
            }
            BlobEvent::Open { .. } => {}
            BlobEvent::Data { id, bytes } => {
                let over = self.arriving.get_mut(id).is_some_and(|copy| {
                    copy.1.extend_from_slice(bytes);
                    copy.1.len() as u64 > CLIPBOARD_LIMIT
                });
                if over {
                    self.arriving.remove(id);
                }
            }
            BlobEvent::Close { id, complete } => {
                // Text only: a picture copied on the computer is not put on this clipboard yet.
                if let (Some((kind, bytes)), true) = (self.arriving.remove(id), *complete) {
                    if kind.starts_with("text/plain") {
                        self.shared.copies.lock().unwrap().push(String::from_utf8_lossy(&bytes).into_owned());
                        self.wake_window();
                    }
                }
            }
        }
    }

    fn cursor_shape(&mut self, shape: &CursorShape) {
        *self.shared.cursor.lock().unwrap() = Some(shape.clone());
        self.wake_window();
    }

    fn cursor_at(&mut self, x: f32, y: f32, visible: bool) {
        *self.shared.cursor_at.lock().unwrap() = (x, y);
        // The window is told of a change of presence, and once of a move it hasn't looked at.
        let moved = !self.shared.cursor_moved.swap(true, Ordering::Relaxed);
        if self.shared.cursor_gone.swap(!visible, Ordering::Relaxed) == visible || moved {
            self.wake_window();
        }
    }

    fn closed(&mut self, reason: &str) {
        *self.shared.closed.lock().unwrap() = Some(reason.to_owned());
        self.wake_window();
    }
}

/// Start the decode thread; what comes back is handed to `Client::connect`.
pub fn start(gpu: &Gpu, scene: &Arc<Scene>) -> Result<(Arc<Shared>, Feed)> {
    // The second display has a window of its own, so a scene of its own; its decoder waits
    // for a stream that only comes if one is asked for.
    let second = Arc::new(Shared::new(Arc::new(Scene::default()), None));
    let shared = Arc::new(Shared::new(scene.clone(), Some(second.clone())));
    let (tx, rx) = mpsc::sync_channel::<Msg>(8);
    let (tx2, rx2) = mpsc::sync_channel::<Msg>(8);
    for (s, rx, first) in [(shared.clone(), rx, true), (second, rx2, false)] {
        let (gpu, main) = (gpu.clone(), shared.clone());
        std::thread::Builder::new().name("decode".into()).spawn(move || {
            if let Err(e) = decode_loop(&s, &gpu, rx) {
                say!("decoding stopped: {e:#}");
                if first {
                    *s.closed.lock().unwrap() = Some(format!("{e:#}"));
                } else {
                    *main.second_gone.lock().unwrap() = Some(format!("{e:#}"));
                }
                main.scene.wake_window();
            }
        })?;
    }
    Ok((shared.clone(), Feed { tx, tx2, shared, arriving: Default::default(), files: Default::default() }))
}

fn decode_loop(s: &Shared, gpu: &Gpu, rx: mpsc::Receiver<Msg>) -> Result<()> {
    let mut decoder: Option<Decoder> = None;
    let mut info: Option<StreamInfo> = None;
    // Whether decoder textures may also be sampled: tried once, given up for good if the
    // driver refuses. `REMOTEMAN_SAMPLE=0` starts without, to test the copying path on a card
    // that doesn't need it.
    let mut sampled = std::env::var("REMOTEMAN_SAMPLE").as_deref() != Ok("0");
    let mut first = true;
    // Frames given to the decoder since its last picture, and when the first of them was; how
    // many pictures in a row it gave before that; and when a keyframe was last asked for on
    // its account. As the Linux viewer's.
    let mut barren: (u32, Option<std::time::Instant>) = (0, None);
    let mut in_step = 0u32;
    let mut asked: Option<std::time::Instant> = None;
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Stream(i) => {
                decoder = Some(if crate::decode::is_software(i.mode.codec) { Decoder::software(gpu, i.mode.codec)? } else { Decoder::new(gpu, i.mode.codec, sampled)? });
                first = true;
                info = Some(i);
            }
            Msg::Frame(data, age) => {
                let (Some(d), Some(i)) = (decoder.as_mut(), info.as_ref()) else { continue };
                let t = std::time::Instant::now();
                let mut decoded = d.decode(&data, age);
                if first && sampled && decoded.is_err() && !crate::decode::is_software(i.mode.codec) {
                    // The card is only asked for its textures at the first frame, a keyframe.
                    // Try that frame again with textures for decoding alone.
                    say!("no decoder textures that can be sampled ({:#}): copying each picture on the GPU instead", decoded.as_ref().err().unwrap());
                    sampled = false;
                    *d = Decoder::new(gpu, i.mode.codec, false)?;
                    decoded = d.decode(&data, age);
                }
                // Once pictures have come, a frame the decoder refuses is not the end: it is
                // counted with the frames that gave nothing, and a keyframe is asked for.
                let decoded = match decoded {
                    Err(e) if !first => {
                        if barren.0 == 0 {
                            say!("decoding: a frame was refused ({e:#})");
                        }
                        None
                    }
                    other => other?,
                };
                let Some(mut picture) = decoded else {
                    barren = (barren.0 + 1, barren.1.or(Some(t)));
                    // A decoder that has been giving a picture for every frame and now gives
                    // none for two has refused a frame (seen with a Mac's stream every 2048
                    // frames). Ask at once; one that never was in step gets half a second.
                    let refused = in_step >= 10 && barren.0 >= 2;
                    let silent = barren.0 >= 3 && barren.1.is_some_and(|since| since.elapsed() > Duration::from_millis(500));
                    if (refused || silent) && asked.is_none_or(|a| a.elapsed() > Duration::from_millis(250)) {
                        s.stuck.store(true, Ordering::Relaxed);
                        s.scene.wake_window();
                        asked = Some(std::time::Instant::now());
                        barren = (0, None);
                        in_step = 0;
                    }
                    continue;
                };
                barren = (0, None);
                in_step = in_step.saturating_add(1);
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
                s.scene.poke();
            }
        }
    }
    Ok(())
}
