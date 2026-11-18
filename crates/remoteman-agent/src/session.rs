//! What the agent does for a client: a virtual monitor made for its display, the picture out,
//! its input in. The connection itself is the shared core's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use remoteman_core::host::{Accept, ClientInfo, HostHandler, Link};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use remoteman_core::media::{BLOB_CLIPBOARD, BLOB_FILE, CLIPBOARD_LIMIT, ClipboardHeader, FileHeader, safe_file_name};
use remoteman_desktop::clipboard::{self, Clipboard};
use remoteman_audio::{DefaultSink, MicSource, SpeakerSink};
use remoteman_core::{BlobEvent, Chroma, Codec, Display, Event, Message, Mode, Packet};
use remoteman_desktop::mutter::VirtualDisplay;

use crate::pipeline::{Pipeline, Settings};

/// What this host encodes.
const MODES: [Mode; 3] = [
    Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 },
    Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 },
    Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 },
];

/// A client being served.
struct Live {
    desktop: VirtualDisplay,
    pipeline: Pipeline,
    size: (u32, u32),
    /// Putting the display where the client asked for it, which waits for the monitor to exist.
    placing: tokio::task::JoinHandle<()>,
    /// The client's name, for what is shown on the desktop (its speaker, its microphone).
    name: String,
    /// What the client asked for, to start the picture afresh from when it changes its mind.
    link: Link,
    screen: Display,
    decoders: Vec<Mode>,
    cursor: bool,
    mode: Mode,
    /// The most the picture may use, as the client last said (0: this host's own figure).
    ceiling: u32,
    /// Files being received: where each is being written, and where it will end up.
    files: BTreeMap<u32, Incoming>,
    /// The clipboard shared with the client, while it is: the desktop's side of it, and the
    /// task sending the client what is copied here.
    clipboard: Option<(Arc<Clipboard>, tokio::task::JoinHandle<()>)>,
    /// What the client copied, on its way in: its type and the content so far.
    copied: BTreeMap<u32, (String, Vec<u8>)>,
}

/// A file on its way in.
struct Incoming {
    /// The sender's number for it, to answer with.
    id: u32,
    file: std::fs::File,
    /// Written under this name first, so a half-arrived file is never taken for a whole one.
    part: PathBuf,
    done: PathBuf,
    size: u64,
    got: u64,
}

/// What a device may do besides see and control the desktop. Off unless the computer's owner
/// said otherwise (`--allow`), because any paired device could otherwise write files here.
#[derive(Debug, Clone, Copy, Default)]
pub struct Allowed {
    pub files: bool,
    /// Sharing the clipboard both ways: a device could otherwise read whatever is copied here.
    pub clipboard: bool,
}

pub struct Agent {
    /// For talking to Mutter over D-Bus.
    runtime: tokio::runtime::Runtime,
    bitrate: u32,
    refs: Option<(u32, u32)>,
    allowed: Allowed,
    /// A login screen's session, and whether it is one made for a remote client.
    login_screen: Option<bool>,
    /// The client wants the clipboard shared (kept across a restart of the picture).
    clipboard_wanted: bool,
    /// The desktop's sound going to the client: the output it has been made (put back when
    /// this goes; first, so that it goes before the speaker does) and the speaker itself.
    sound: Option<(Option<DefaultSink>, SpeakerSink)>,
    /// The client's microphone, as one the desktop's programs can pick.
    microphone: Option<MicSource>,
    /// The client's camera, as one the desktop's programs can pick, and when it last had to
    /// ask the client for a keyframe.
    camera: Option<(remoteman_camera::Camera, Option<std::time::Instant>)>,
    /// A mark of the last copy sent from the desktop to the client, to tell a repeat.
    last_copy: Arc<std::sync::Mutex<Option<u64>>>,
    /// Make the client's speaker the desktop's output while it is on (`--keep-output`: don't).
    take_output: bool,
    /// Counts what was copied here and sent, for the client to tell new from stale.
    copies: Arc<AtomicU32>,
    live: Option<Live>,
}

/// The lowest and highest bitrate a client may ask for, in bits a second.
const BITRATE: std::ops::RangeInclusive<u32> = 1_000_000..=150_000_000;

impl Agent {
    pub fn new(bitrate: u32, refs: Option<(u32, u32)>, allowed: Allowed, take_output: bool) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).thread_name("desktop").enable_all().build()?;
        // What kind of session this is doesn't change while it lasts.
        let login_screen = match runtime.block_on(remoteman_desktop::logind::session_kind()) {
            Ok(kind) => kind.is_login_screen().then_some(kind.remote),
            Err(e) => {
                tracing::debug!("couldn't ask logind about this session: {e:#}");
                None
            }
        };
        if login_screen.is_some() {
            tracing::info!("this is a login screen's session: no files, no clipboard, and it ends when its client goes");
        }
        Ok(Self { runtime, bitrate, refs, allowed, login_screen, clipboard_wanted: false, sound: None, microphone: None, camera: None, last_copy: Arc::default(), take_output, copies: Arc::default(), live: None })
    }

    /// What the picture is encoded at when the client's ceiling is `ceiling`.
    fn rate(&self, ceiling: u32) -> u32 {
        if ceiling == 0 { self.bitrate } else { ceiling.clamp(*BITRATE.start(), *BITRATE.end()) }
    }

    /// Make the client's display and start its picture.
    fn start(&mut self, name: String, screen: Display, decoders: Vec<Mode>, cursor: bool, ceiling: u32, link: Link) -> Result<Accept, String> {
        // The client's order of preference decides.
        let mode = decoders.iter().copied().find(|m| MODES.contains(m)).ok_or("no video mode in common")?;
        if screen.width % 2 != 0 || screen.height % 2 != 0 || screen.width < 64 || screen.height < 64 || screen.width > 8192 || screen.height > 8192 {
            return Err("display sizes must be even, from 64 to 8192".into());
        }
        let refresh = screen.refresh.clamp(1, 240);
        let desktop = self.runtime.block_on(VirtualDisplay::create(cursor)).map_err(|e| format!("{e:#}"))?;
        let settings = Settings {
            node: desktop.node,
            width: screen.width,
            height: screen.height,
            fps: refresh,
            scale: screen.scale.clamp(1.0, 4.0),
            mode,
            bitrate: self.rate(ceiling),
            cursor,
            refs: self.refs,
        };
        let pipeline = match Pipeline::start(settings, link.clone()) {
            Ok(p) => p,
            Err(e) => {
                self.runtime.block_on(desktop.close());
                return Err(format!("{e:#}"));
            }
        };
        // Not waited for: the client must hear about its stream before the first frame reaches it.
        let place = desktop.place(screen.placement);
        let placing = self.runtime.spawn(async move {
            if let Err(e) = place.await {
                // The client still gets its display, just beside the others.
                tracing::warn!("couldn't make the display {:?}: {e:#}", screen.placement);
            }
        });
        self.live = Some(Live { desktop, pipeline, size: (screen.width, screen.height), placing, name, link, screen, decoders, cursor, mode, ceiling, files: BTreeMap::new(), clipboard: None, copied: BTreeMap::new() });
        // The clipboard belongs to the display's session, so a new display shares it anew.
        self.share_clipboard(self.clipboard_wanted);
        Ok(Accept { mode, width: screen.width, height: screen.height, refresh, colour: "bt709".into() })
    }

    /// Stop the picture and take the display away. Files half received are thrown away.
    fn stop(&mut self) -> Option<Live> {
        let mut l = self.live.take()?;
        tracing::info!(skipped = l.pipeline.skipped.load(std::sync::atomic::Ordering::Relaxed), "session over");
        l.placing.abort();
        for (_, f) in std::mem::take(&mut l.files) {
            let _ = std::fs::remove_file(&f.part);
        }
        if let Some((clipboard, watching)) = l.clipboard.take() {
            watching.abort();
            let _ = self.runtime.block_on(clipboard.disable());
        }
        Some(l)
    }

    /// Send the desktop's sound to the client, or stop. The client gets a speaker of its own
    /// on the desktop, which (unless told to keep its hands off) becomes the output while it
    /// is on, so that what plays here is heard there; the output chosen before comes back
    /// when it goes.
    fn send_sound(&mut self, on: bool) {
        self.sound = None;
        let (Some(l), true, None) = (self.live.as_ref(), on, self.login_screen) else { return };
        let (link, clock) = (l.link.clone(), l.link.clone());
        link.send_message(Message::SoundFormat { codec: "opus".into(), rate: remoteman_audio::SAMPLE_RATE, channels: 2, frame_us: remoteman_audio::FRAME_US as u32 });
        // The speaker says nothing while nothing plays. Its packets count on without a hole
        // across such a pause, so the pause shows in their times: mark the first one after.
        let mut last = None::<u64>;
        let speaker = SpeakerSink::open("remoteman-speaker", &format!("{} (remoteman)", l.name), move || clock.now_us(), move |p| {
            let start = last.is_none_or(|t| p.capture_us.saturating_sub(t) > 3 * remoteman_audio::FRAME_US);
            last = Some(p.capture_us);
            link.send_sound(start, p.seq, p.capture_us, &p.data);
        });
        match speaker {
            Ok(speaker) => {
                let output = if self.take_output {
                    DefaultSink::set(speaker.node().name()).inspect_err(|e| tracing::warn!("couldn't make the device's speaker the output: {e:#}")).ok()
                } else {
                    None
                };
                tracing::info!(taken_over = output.is_some(), "the desktop's sound goes to the device");
                self.sound = Some((output, speaker));
            }
            Err(e) => tracing::warn!("couldn't make a speaker for the device: {e:#}"),
        }
    }

    /// Present the client's microphone as one the desktop's programs can pick, or take it away.
    fn take_microphone(&mut self, on: bool) {
        self.microphone = None;
        let (Some(l), true, None) = (self.live.as_ref(), on, self.login_screen) else { return };
        match MicSource::open("remoteman-microphone", &format!("{} microphone (remoteman)", l.name)) {
            Ok(microphone) => {
                tracing::info!("the device's microphone is on the desktop");
                self.microphone = Some(microphone);
            }
            Err(e) => tracing::warn!("couldn't make a microphone for the device: {e:#}"),
        }
    }

    /// Present the client's camera as one the desktop's programs can pick, or take it away.
    /// The device says what it sends; what isn't said is the usual 1280x720 at 30 a second.
    fn take_camera(&mut self, on: bool, codec: &str, width: u32, height: u32, fps: u32) {
        self.camera = None;
        let (Some(l), true, None) = (self.live.as_ref(), on, self.login_screen) else { return };
        if !codec.is_empty() && codec != "h264" {
            tracing::warn!(codec, "the device's camera is sent in a codec this host doesn't take; only h264 is");
            return;
        }
        let usual = remoteman_camera::Options::default();
        let or = |said: u32, usual: u32| if said == 0 { usual } else { said };
        let options = remoteman_camera::Options { width: or(width, usual.width), height: or(height, usual.height), fps: or(fps, usual.fps), ..usual };
        match remoteman_camera::Camera::start_with(&format!("{} camera (remoteman)", l.name), options) {
            Ok(camera) => {
                tracing::info!(node = camera.node_name(), loopback = ?camera.loopback_device(), "the device's camera is on the desktop");
                self.camera = Some((camera, None));
            }
            Err(e) => tracing::warn!("couldn't make a camera for the device: {e:#}"),
        }
    }

    /// Share the clipboard with the client, both ways, or stop. Only where the computer's
    /// owner allowed it, and never at a login screen.
    fn share_clipboard(&mut self, on: bool) {
        let allowed = self.allowed.clipboard && self.login_screen.is_none();
        let Some(l) = self.live.as_mut() else { return };
        if let Some((clipboard, watching)) = l.clipboard.take() {
            watching.abort();
            let _ = self.runtime.block_on(clipboard.disable());
            tracing::info!("the clipboard is no longer shared with the device");
        }
        if on && !allowed {
            tracing::info!("the device asked to share the clipboard, which isn't allowed here");
        }
        if !on || !allowed {
            return;
        }
        let (clipboard, mut changes) = match self.runtime.block_on(Clipboard::new(&l.desktop, clipboard::DEFAULT_CAP)) {
            Ok(c) => c,
            Err(e) => return tracing::warn!("couldn't share the clipboard: {e:#}"),
        };
        let clipboard = Arc::new(clipboard);
        let (desktop, link, copies) = (clipboard.clone(), l.link.clone(), self.copies.clone());
        *self.last_copy.lock().unwrap() = None;
        let last_copy = self.last_copy.clone();
        // Whatever is copied on the desktop goes to the client: text if there is any, else a
        // picture.
        let watching = self.runtime.spawn(async move {
            while let Some(types) = changes.next().await {
                let (read_as, send_as) = match clipboard::text_type(&types) {
                    Some(text) => (text.to_owned(), "text/plain;charset=utf-8"),
                    None if types.iter().any(|t| t == "image/png") => ("image/png".to_owned(), "image/png"),
                    None => continue,
                };
                match desktop.read(&read_as).await {
                    Ok(bytes) if !bytes.is_empty() => {
                        // The desktop announces one copy more than once (twice, as a rule):
                        // the same thing again is not sent again.
                        let mark = {
                            use std::hash::{Hash, Hasher};
                            let mut h = std::collections::hash_map::DefaultHasher::new();
                            (send_as, &bytes).hash(&mut h);
                            h.finish()
                        };
                        if last_copy.lock().unwrap().replace(mark) == Some(mark) {
                            continue;
                        }
                        // What was copied is never logged, only what kind of thing and how much.
                        tracing::info!(kind = send_as, bytes = bytes.len(), "a copy on the desktop goes to the device");
                        let header = ClipboardHeader { seq: copies.fetch_add(1, Ordering::Relaxed) + 1, mime: send_as.into(), size: bytes.len() as u64 };
                        let blob = link.open_blob(BLOB_CLIPBOARD, serde_json::to_string(&header).unwrap_or_default());
                        // Writing waits while the network is behind, which this task mustn't.
                        let _ = tokio::task::spawn_blocking(move || blob.write(&bytes) && blob.finish()).await;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("couldn't read the desktop's clipboard: {e:#}"),
                }
            }
        });
        l.clipboard = Some((clipboard, watching));
        tracing::info!("the clipboard is shared with the device");
    }

    /// A clipboard blob from the client: what was copied on the device becomes what the
    /// desktop pastes.
    fn copied(&mut self, event: &BlobEvent) {
        let Some(l) = self.live.as_mut() else { return };
        match event {
            BlobEvent::Open { id, header, .. } => {
                let Ok(h) = serde_json::from_str::<ClipboardHeader>(header) else { return };
                if l.clipboard.is_some() && h.size <= CLIPBOARD_LIMIT {
                    l.copied.insert(*id, (h.mime, Vec::with_capacity(h.size as usize)));
                }
            }
            BlobEvent::Data { id, bytes } => {
                let over = l.copied.get_mut(id).is_some_and(|c| {
                    c.1.extend_from_slice(bytes);
                    c.1.len() as u64 > CLIPBOARD_LIMIT
                });
                if over {
                    l.copied.remove(id);
                }
            }
            BlobEvent::Close { id, complete } => {
                let (Some((mime, bytes)), Some((clipboard, _)), true) = (l.copied.remove(id), l.clipboard.as_ref(), *complete) else { return };
                let clipboard = clipboard.clone();
                // The desktop's clipboard is the device's copy now: the next copy on the
                // desktop is news even if it is what was sent before.
                *self.last_copy.lock().unwrap() = None;
                self.runtime.spawn(async move {
                    let (kind, size) = (mime.clone(), bytes.len());
                    match clipboard.offer(vec![(mime, bytes)]).await {
                        Ok(_) => tracing::info!(kind, bytes = size, "a copy on the device is on the desktop's clipboard"),
                        Err(e) => tracing::warn!("couldn't put the device's copy on the clipboard: {e:#}"),
                    }
                });
            }
        }
    }

    /// The client changed what it wants: a new ceiling for the bitrate takes effect in the
    /// running picture; another display or other decoders start the picture afresh.
    fn configure(&mut self, display: Option<Display>, bitrate: Option<u32>, decoders: Option<Vec<Mode>>) {
        let Some(l) = self.live.as_mut() else { return };
        if let Some(b) = bitrate {
            l.ceiling = b;
            let rate = if b == 0 { self.bitrate } else { b.clamp(*BITRATE.start(), *BITRATE.end()) };
            l.pipeline.set_bitrate(rate);
        }
        let screen = display.unwrap_or(l.screen);
        let decoders = decoders.unwrap_or_else(|| l.decoders.clone());
        let mode = decoders.iter().copied().find(|m| MODES.contains(m));
        if screen == l.screen && mode == Some(l.mode) {
            return;
        }
        tracing::info!(?screen, ?mode, "the client asked for another picture");
        // The old picture goes first, display and all; then the client is told, and only then
        // does the new one start, so its first frame follows the news of it.
        let Some(old) = self.stop() else { return };
        let (link, cursor, ceiling, name) = (old.link.clone(), old.cursor, old.ceiling, old.name.clone());
        let (was_screen, was_decoders) = (old.screen, old.decoders.clone());
        drop(old.pipeline);
        self.runtime.block_on(old.desktop.close());
        let refresh = screen.refresh.clamp(1, 240);
        if let Some(mode) = mode {
            link.restart(Accept { mode, width: screen.width, height: screen.height, refresh, colour: "bt709".into() });
        }
        if let Err(e) = self.start(name.clone(), screen, decoders, cursor, ceiling, link.clone()) {
            // Back to what worked, rather than leave the client with nothing.
            tracing::warn!("couldn't start that picture ({e}); going back to the one before");
            let refresh = was_screen.refresh.clamp(1, 240);
            if let Some(mode) = was_decoders.iter().copied().find(|m| MODES.contains(m)) {
                link.restart(Accept { mode, width: was_screen.width, height: was_screen.height, refresh, colour: "bt709".into() });
            }
            if let Err(e) = self.start(name, was_screen, was_decoders, cursor, ceiling, link) {
                tracing::error!("couldn't restore the picture: {e}");
            }
        }
    }

    /// A file blob from the client: written into the user's Downloads folder under a name of
    /// this host's choosing, and answered with where it went.
    fn file(&mut self, event: &BlobEvent) {
        let allowed = self.allowed.files && self.login_screen.is_none();
        let Some(l) = self.live.as_mut() else { return };
        match event {
            BlobEvent::Open { id, header, .. } => {
                let Ok(h) = serde_json::from_str::<FileHeader>(header) else { return };
                let refuse = |why: &str| l.link.send_message(Message::FileResult { id: h.id, ok: false, message: why.into() });
                if !allowed {
                    return refuse("this computer doesn't take files from this device");
                }
                let folder = downloads();
                if let Err(e) = std::fs::create_dir_all(&folder) {
                    return refuse(&format!("{}: {e}", folder.display()));
                }
                // Never over a file that is there: "name (2).ext" and so on.
                let name = safe_file_name(&h.name);
                let (stem, ext) = match name.rsplit_once('.') {
                    Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
                    _ => (name.clone(), String::new()),
                };
                let done = (1..1000)
                    .map(|n| folder.join(if n == 1 { name.clone() } else { format!("{stem} ({n}){ext}") }))
                    .find(|p| !p.exists() && !part_of(p).exists());
                let Some(done) = done else { return refuse("too many files of that name") };
                let part = part_of(&done);
                match std::fs::OpenOptions::new().write(true).create_new(true).open(&part) {
                    Ok(file) => {
                        l.files.insert(*id, Incoming { id: h.id, file, part, done, size: h.size, got: 0 });
                    }
                    Err(e) => refuse(&format!("{}: {e}", part.display())),
                }
            }
            BlobEvent::Data { id, bytes } => {
                let Some(f) = l.files.get_mut(id) else { return };
                f.got += bytes.len() as u64;
                // More than it said it would be, or a disk that won't take it: give it up.
                let failed = if f.got > f.size { Some("it was longer than it said".to_owned()) } else { std::io::Write::write_all(&mut f.file, bytes).err().map(|e| e.to_string()) };
                if let Some(why) = failed {
                    let f = l.files.remove(id).unwrap();
                    let _ = std::fs::remove_file(&f.part);
                    l.link.send_message(Message::FileResult { id: f.id, ok: false, message: why });
                }
            }
            BlobEvent::Close { id, complete } => {
                let Some(f) = l.files.remove(id) else { return };
                let whole = *complete && f.got == f.size;
                let result = if whole { f.file.sync_all().and_then(|()| std::fs::rename(&f.part, &f.done)).map_err(|e| e.to_string()) } else { Err("it didn't arrive whole".to_owned()) };
                match result {
                    Ok(()) => {
                        tracing::info!(file = %f.done.display(), bytes = f.got, "a file from the device");
                        l.link.send_message(Message::FileResult { id: f.id, ok: true, message: f.done.display().to_string() });
                    }
                    Err(why) => {
                        let _ = std::fs::remove_file(&f.part);
                        l.link.send_message(Message::FileResult { id: f.id, ok: false, message: why });
                    }
                }
            }
        }
    }
}

/// The name a file is written under until it is whole.
fn part_of(done: &Path) -> PathBuf {
    let mut name = done.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    done.with_file_name(name)
}

/// The user's Downloads folder, as the desktop names it.
fn downloads() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    // ~/.config/user-dirs.dirs: XDG_DOWNLOAD_DIR="$HOME/Downloads"
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("user-dirs.dirs");
    std::fs::read_to_string(config)
        .ok()
        .and_then(|all| all.lines().find_map(|l| l.trim().strip_prefix("XDG_DOWNLOAD_DIR=").map(|v| v.trim_matches('"').replace("$HOME", &home.to_string_lossy()))))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join("Downloads"))
}

impl HostHandler for Agent {
    fn connected(&mut self, client: &ClientInfo, link: Link) -> Result<Accept, String> {
        self.left();
        let screen = client.display;
        tracing::info!(name = %client.name, ?screen, decoders = ?client.decoders, cursor_apart = client.cursor, "client");
        self.start(client.name.clone(), screen, client.decoders.clone(), client.cursor, 0, link)
    }

    fn mic(&mut self, packet: &Packet) {
        if let Some(microphone) = &self.microphone {
            microphone.push(remoteman_audio::Packet { seq: packet.seq, capture_us: packet.time_us, data: packet.data.clone() });
        }
    }

    fn camera(&mut self, frame: &remoteman_core::Frame) {
        let Some((camera, asked)) = &mut self.camera else { return };
        // A frame the decoder can't use: nothing is shown until the device sends a keyframe.
        if camera.push(&frame.data, frame.key, frame.capture_us) == remoteman_camera::Pushed::Lost && asked.is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(250)) {
            *asked = Some(std::time::Instant::now());
            if let Some(l) = &self.live {
                l.link.send_message(Message::CameraKeyframe);
            }
        }
    }

    fn message(&mut self, message: &Message) {
        match message {
            Message::Configure { display, bitrate, decoders } => self.configure(*display, *bitrate, decoders.clone()),
            Message::Clipboard { on } => {
                self.clipboard_wanted = *on;
                self.share_clipboard(*on);
            }
            Message::Sound { on } => self.send_sound(*on),
            Message::Mic { on, .. } => self.take_microphone(*on),
            Message::Camera { on, codec, width, height, fps } => self.take_camera(*on, codec, *width, *height, *fps),
            _ => {}
        }
    }

    fn blob(&mut self, event: &BlobEvent) {
        // A blob of a kind that isn't taken never gets an entry, so its content and its end
        // fall through.
        let (is_file, is_copy) = match event {
            BlobEvent::Open { kind, .. } => (*kind == BLOB_FILE, *kind == BLOB_CLIPBOARD),
            other => self.live.as_ref().map_or((false, false), |l| (l.files.contains_key(&other.id()), l.copied.contains_key(&other.id()))),
        };
        if is_file {
            self.file(event);
        } else if is_copy {
            self.copied(event);
        }
    }

    fn keyframe(&mut self) {
        if let Some(l) = &self.live {
            l.pipeline.request_keyframe();
        }
    }

    fn lost(&mut self, first: u32, last: u32) {
        if let Some(l) = &self.live {
            l.pipeline.report_lost(first, last);
        }
    }

    fn input(&mut self, event: Event) {
        let Some(l) = &self.live else { return };
        let scale = f32::from_bits(l.pipeline.scale.load(std::sync::atomic::Ordering::Relaxed));
        if let Err(e) = self.runtime.block_on(l.desktop.inject(&event, l.size, scale)) {
            tracing::warn!("input: {e:#}");
        }
    }

    fn left(&mut self) {
        self.clipboard_wanted = false;
        (self.sound, self.microphone, self.camera) = (None, None, None);
        if let Some(l) = self.stop() {
            drop(l.pipeline);
            self.runtime.block_on(l.desktop.close());
            // A headless login screen exists for one client; when that client has logged in
            // (and been moved to its session) or gone, nobody else will ever see it, and GDM
            // leaves ending it to us.
            if self.login_screen == Some(true) {
                tracing::info!("the client has left this login screen: ending its session");
                if let Err(e) = self.runtime.block_on(remoteman_desktop::logind::log_out()) {
                    tracing::warn!("couldn't end the login screen's session: {e:#}");
                }
            }
        }
    }
}
