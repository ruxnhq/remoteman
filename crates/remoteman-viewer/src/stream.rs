//! A running session: the core hands frames to a decode thread, which leaves the newest picture
//! where the window's frame clock picks it up.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use anyhow::Result;
use remoteman_core::media::{BLOB_CLIPBOARD, BLOB_FILE, CLIPBOARD_LIMIT, ClipboardHeader, FileHeader};
use remoteman_core::{BlobEvent, Client, CursorShape, Display, Frame, Handler, Message, Mode, Options, Packet, StreamInfo};

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
    /// The stream's size in pixels and its rate, once the host has said (for the window's bar).
    pub format: Mutex<Option<(u32, u32, u32)>>,
    /// Bytes of video received since it was last read (for the figures).
    pub bytes: AtomicU64,
    /// What plays the computer's sound here, while it is on.
    pub sound: Mutex<Option<remoteman_audio::Player>>,
    /// What records this device's microphone for the computer, while it is on.
    pub microphone: Mutex<Option<remoteman_audio::MicCapture>>,
    /// What reads this device's camera for the computer, while it is on, and how many frames
    /// it has sent.
    pub camera: Mutex<Option<remoteman_camera::Source>>,
    pub camera_frames: AtomicU32,
    /// The computer has been told the camera is on (it may still be opening).
    camera_told: std::sync::atomic::AtomicBool,
    /// Counts the times the camera was switched on or off.
    camera_turn: AtomicU32,
    /// Why the camera couldn't be opened, until the window says so and lets the button out.
    pub camera_failed: Mutex<Option<String>>,
    /// What was copied on the computer, whole, until the window takes it: its type and bytes.
    pub copies: Mutex<Vec<(String, Vec<u8>)>>,
    /// What became of files sent to the computer, until the window says so: whether it
    /// arrived, and where it was put or why not.
    pub results: Mutex<Vec<(bool, String)>>,
    /// What the computer offers now: what it said at the start, then whatever it has said since
    /// (its owner can switch the clipboard and files on and off while a device is connected).
    pub features: Mutex<Vec<String>>,
    /// The computer changed what it offers, until the window has caught up.
    pub features_changed: std::sync::atomic::AtomicBool,
    /// The computer said the second display is gone, and why, until the window has closed it.
    pub second_gone: Mutex<Option<String>>,
    /// Where files from the computer go.
    pub folder: Mutex<PathBuf>,
    /// What became of files the computer sent, until the window answers it and says so: the
    /// computer's number for the file, and where it was put or why not.
    pub received: Mutex<Vec<remoteman_core::receive::Outcome>>,
    /// Counts the copies and the files sent.
    copy_seq: AtomicU32,
    file_seq: AtomicU32,
}

enum Msg {
    Stream(StreamInfo),
    Frame(Vec<u8>, Option<i64>),
}

struct Feed {
    tx: mpsc::SyncSender<Msg>,
    /// To the second display's decoder.
    tx2: mpsc::SyncSender<Msg>,
    shared: Arc<Shared>,
    /// Copies from the computer that are still arriving.
    arriving: HashMap<u32, (String, Vec<u8>)>,
    /// Files from the computer that are still arriving.
    files: remoteman_core::receive::Receiver,
    pairing: Arc<Mutex<Option<(String, u32)>>>,
}

impl Handler for Feed {
    fn stream(&mut self, info: &StreamInfo) {
        *self.shared.format.lock().unwrap() = Some((info.width, info.height, info.refresh));
        let _ = self.tx.send(Msg::Stream(info.clone()));
    }

    fn frame(&mut self, frame: &Frame, age_us: Option<i64>) {
        // Every frame leans on the one before, so none may be skipped here: if the decoder
        // falls behind, this waits, and the loss shows up upstream as a keyframe request.
        self.shared.bytes.fetch_add(frame.data.len() as u64, Ordering::Relaxed);
        let _ = self.tx.send(Msg::Frame(frame.data.clone(), age_us));
    }

    fn stream2(&mut self, info: &StreamInfo) {
        let _ = self.tx2.send(Msg::Stream(info.clone()));
    }

    fn frame2(&mut self, frame: &Frame, age_us: Option<i64>) {
        let _ = self.tx2.send(Msg::Frame(frame.data.clone(), age_us));
    }

    fn pairing(&mut self, code: &str, seconds: u32) {
        *self.pairing.lock().unwrap() = Some((code.to_owned(), seconds));
    }

    fn cursor_shape(&mut self, shape: &CursorShape) {
        *self.shared.cursor.lock().unwrap() = Some(shape.clone());
    }

    fn blob(&mut self, event: &BlobEvent) {
        let folder = self.shared.folder.lock().unwrap().clone();
        if let Some(outcome) = self.files.blob(event, &folder) {
            self.shared.received.lock().unwrap().push(outcome);
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
                if let (Some(copy), true) = (self.arriving.remove(id), *complete) {
                    self.shared.copies.lock().unwrap().push(copy);
                }
            }
        }
    }

    fn message(&mut self, message: &Message) {
        match message {
            Message::FileResult { ok, message, .. } => self.shared.results.lock().unwrap().push((*ok, message.clone())),
            // The computer lost a frame of the camera: the next one is whole.
            Message::CameraKeyframe => {
                if let Some(camera) = self.shared.camera.lock().unwrap().as_ref() {
                    camera.keyframe();
                }
            }
            // The computer took the second display away (or couldn't make it).
            Message::Display { index: 1, on: false, message, .. } => *self.shared.second_gone.lock().unwrap() = Some(message.clone()),
            Message::Features { features } => {
                *self.shared.features.lock().unwrap() = features.clone();
                self.shared.features_changed.store(true, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn sound(&mut self, packet: &Packet) {
        // The player keeps a few packets ahead and conceals the ones that never come.
        if let Some(player) = self.shared.sound.lock().unwrap().as_ref() {
            player.push(remoteman_audio::Packet { seq: packet.seq, capture_us: packet.time_us, data: packet.data.clone() });
        }
    }

    fn closed(&mut self, reason: &str) {
        *self.shared.closed.lock().unwrap() = Some(reason.to_owned());
    }
}

/// Start a thread that decodes one picture stream into `shared` (its newest picture, its
/// figures, and why it stopped if it did).
fn decoding(shared: Arc<Shared>) -> Result<mpsc::SyncSender<Msg>> {
    let (tx, rx) = mpsc::sync_channel::<Msg>(8);
    let s = shared;
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
    Ok(tx)
}

pub struct Session {
    pub client: Client,
    pub shared: Arc<Shared>,
    /// The second display's side: its newest picture and figures.
    pub second: Arc<Shared>,
}

pub struct Request {
    pub host: String,
    pub state: PathBuf,
    pub pair: bool,
    pub display: Display,
    pub decoders: Vec<Mode>,
    /// The computer's cursor comes apart from the picture, for this window's pointer to wear;
    /// otherwise the computer draws it into the picture.
    pub cursor: bool,
    /// The computer's user's password, if it said it is locked and this is the try after.
    pub unlock: Option<String>,
    /// Only into a session already running on the computer: never its login screen.
    pub attach_only: bool,
    /// Where files from the computer go.
    pub folder: PathBuf,
    /// Told the code to show, and the seconds there are, if the computer holds this device
    /// until its owner has confirmed it.
    pub pairing: Arc<Mutex<Option<(String, u32)>>>,
}

impl Session {
    /// Whether the computer offers a feature (`sound`, `clipboard`, `files`, ...).
    pub fn offers(&self, feature: &str) -> bool {
        self.shared.features.lock().unwrap().iter().any(|f| f == feature)
    }

    /// Play the computer's sound on this device's default output, or stop. While it plays
    /// here the computer's own speakers are silent, if the computer is set up that way.
    /// Share the clipboard with the computer from now on, or stop. Copies then go both ways.
    pub fn share_clipboard(&self, on: bool) {
        self.client.send_message(Message::Clipboard { on });
    }

    /// Send what was copied here: `text/plain;charset=utf-8` or `image/png`.
    pub fn send_copy(&self, kind: &str, bytes: Vec<u8>) {
        if bytes.is_empty() || bytes.len() as u64 > CLIPBOARD_LIMIT {
            return;
        }
        let header = ClipboardHeader { seq: self.shared.copy_seq.fetch_add(1, Ordering::Relaxed) + 1, mime: kind.to_owned(), size: bytes.len() as u64 };
        let blob = self.client.open_blob(BLOB_CLIPBOARD, serde_json::to_string(&header).unwrap_or_default());
        // Writing waits while the network is behind, which the window mustn't.
        let _ = std::thread::Builder::new().name("copy".into()).spawn(move || blob.write(&bytes) && blob.finish());
    }

    /// Send a file to the computer, which decides where it goes and answers when it is there
    /// (`Shared::results`). Fails at once if the file can't be opened.
    pub fn send_file(&self, path: &Path) -> Result<String> {
        let mut file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let header = FileHeader { id: self.shared.file_seq.fetch_add(1, Ordering::Relaxed) + 1, name: name.clone(), size };
        let blob = self.client.open_blob(BLOB_FILE, serde_json::to_string(&header).unwrap_or_default());
        let results = self.shared.clone();
        let said = name.clone();
        std::thread::Builder::new().name("file".into()).spawn(move || {
            let mut piece = vec![0u8; 256 * 1024];
            loop {
                match file.read(&mut piece) {
                    Ok(0) => {
                        blob.finish();
                        return;
                    }
                    Ok(n) if blob.write(&piece[..n]) => {}
                    // The connection went, or the file can't be read on: given up (dropping
                    // the blob tells the computer).
                    Ok(_) => return,
                    Err(e) => {
                        results.results.lock().unwrap().push((false, format!("{said} could not be read: {e}")));
                        return;
                    }
                }
            }
        })?;
        Ok(name)
    }

    /// Let the computer use this device's camera, or stop. `from`: a PipeWire camera node to
    /// read and no other, instead of the default camera. The camera is opened on a thread of
    /// its own, since one that doesn't answer takes seconds to give up on: if it can't be
    /// opened, the window hears why through `Shared::camera_failed`.
    pub fn set_camera(&self, on: bool, from: Option<&str>) {
        let off = Message::Camera { on: false, codec: String::new(), width: 0, height: 0, fps: 0 };
        // Each change of mind has a number, so a camera that opens after it was switched off
        // again is let go of.
        let turn = self.shared.camera_turn.fetch_add(1, Ordering::Relaxed) + 1;
        if !on {
            // (Never held while a camera stops: the computer's requests for a keyframe take
            // the lock too, on the connection's own thread.)
            let had = self.shared.camera.lock().unwrap().take();
            drop(had);
            if self.shared.camera_told.swap(false, Ordering::Relaxed) {
                self.client.send_message(off);
                println!("camera: {} frames sent", self.shared.camera_frames.swap(0, Ordering::Relaxed));
            }
            return;
        }
        if self.shared.camera_told.swap(true, Ordering::Relaxed) {
            return;
        }
        // Said first: frames that reach the computer before it are thrown away there.
        self.client.send_message(Message::Camera { on: true, codec: "h264".into(), width: 1280, height: 720, fps: 30 });
        let began = std::time::Instant::now();
        let (sender, shared, from) = (self.client.camera_sender(), self.shared.clone(), from.map(str::to_owned));
        let started = std::thread::Builder::new().name("camera-start".into()).spawn(move || {
            let counted = shared.clone();
            let opened = remoteman_camera::Source::start(from.as_deref(), move |data, key| {
                counted.camera_frames.fetch_add(1, Ordering::Relaxed);
                sender.send(data, key, began.elapsed().as_micros() as u64);
            });
            let current = || shared.camera_turn.load(Ordering::Relaxed) == turn;
            match opened {
                Ok(source) => {
                    let mut camera = shared.camera.lock().unwrap();
                    if current() {
                        *camera = Some(source);
                    }
                    // (Otherwise it was switched off meanwhile, and the camera is let go of here.)
                }
                Err(e) if current() => *shared.camera_failed.lock().unwrap() = Some(format!("{e:#}")),
                Err(_) => {}
            }
        });
        if let Err(e) = started {
            *self.shared.camera_failed.lock().unwrap() = Some(e.to_string());
        }
    }

    /// Let the computer use this device's microphone, or stop. `from`: a PipeWire node to
    /// record and no other, instead of the default microphone.
    pub fn set_microphone(&self, on: bool, from: Option<&str>) -> Result<()> {
        let mut microphone = self.shared.microphone.lock().unwrap();
        if !on {
            if microphone.take().is_some() {
                self.client.send_message(Message::Mic { on: false, codec: String::new(), rate: 0, channels: 0, frame_us: 0 });
            }
            return Ok(());
        }
        if microphone.is_some() {
            return Ok(());
        }
        let began = std::time::Instant::now();
        let sender = self.client.mic_sender();
        // Silence is left out by the recorder; the first packet after a gap says so.
        let mut last = None::<u64>;
        let capture = remoteman_audio::MicCapture::open("remoteman", from, move || began.elapsed().as_micros() as u64, move |packet| {
            let start = last.is_none_or(|at| packet.capture_us.saturating_sub(at) > 25_000);
            last = Some(packet.capture_us);
            sender.send(start, packet.seq, packet.capture_us, &packet.data);
        })?;
        self.client.send_message(Message::Mic { on: true, codec: "opus".into(), rate: remoteman_audio::SAMPLE_RATE, channels: 1, frame_us: remoteman_audio::FRAME_US as u32 });
        *microphone = Some(capture);
        Ok(())
    }

    /// `to`: a PipeWire node to play on and no other, instead of the default output.
    pub fn set_sound(&self, on: bool, to: Option<&str>) -> Result<()> {
        let mut player = self.shared.sound.lock().unwrap();
        if on && player.is_none() {
            *player = Some(remoteman_audio::Player::open("remoteman", to)?);
        } else if !on {
            if let Some(p) = player.take() {
                let s = p.stats();
                println!("sound: {} packets played, {} concealed, {} late, {} skipped", s.played, s.concealed, s.late, s.skipped);
            }
        }
        self.client.send_message(Message::Sound { on });
        Ok(())
    }

    /// Connect (this blocks) and start decoding.
    pub fn start(r: Request) -> Result<Self> {
        let shared = Arc::new(Shared::default());
        *shared.folder.lock().unwrap() = r.folder.clone();
        let tx = decoding(shared.clone())?;
        // The second display's picture, when there is one, is decoded apart and kept apart.
        let second = Arc::new(Shared::default());
        let tx2 = decoding(second.clone())?;
        let hostname = std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "Linux".into());
        let options = Options { host: r.host, state: r.state, name: hostname, pair: r.pair, display: r.display, decoders: r.decoders, test_drop_one_in: None, test_delay_lost_ms: None, unlock: r.unlock.clone(), attach_only: r.attach_only, cursor: r.cursor };
        let client = Client::connect(options, Feed { tx, tx2, shared: shared.clone(), arriving: HashMap::new(), files: Default::default(), pairing: r.pairing.clone() })?;
        // What it said in its hello, unless it has already said otherwise since.
        if !shared.features_changed.load(Ordering::Relaxed) {
            *shared.features.lock().unwrap() = client.host_features.clone();
        }
        Ok(Self { client, shared, second })
    }
}
