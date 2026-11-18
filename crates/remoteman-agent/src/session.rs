//! What the agent does for a client: a virtual monitor made for its display, the picture out,
//! its input in. The connection itself is the shared core's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use remoteman_core::host::{Accept, ClientInfo, HostHandler, Link};
use remoteman_core::media::{BLOB_FILE, FileHeader, safe_file_name};
use remoteman_core::{BlobEvent, Chroma, Codec, Display, Event, Message, Mode};
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
}

pub struct Agent {
    /// For talking to Mutter over D-Bus.
    runtime: tokio::runtime::Runtime,
    bitrate: u32,
    refs: Option<(u32, u32)>,
    allowed: Allowed,
    live: Option<Live>,
}

/// The lowest and highest bitrate a client may ask for, in bits a second.
const BITRATE: std::ops::RangeInclusive<u32> = 1_000_000..=150_000_000;

impl Agent {
    pub fn new(bitrate: u32, refs: Option<(u32, u32)>, allowed: Allowed) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).thread_name("desktop").enable_all().build()?;
        Ok(Self { runtime, bitrate, refs, allowed, live: None })
    }

    /// What the picture is encoded at when the client's ceiling is `ceiling`.
    fn rate(&self, ceiling: u32) -> u32 {
        if ceiling == 0 { self.bitrate } else { ceiling.clamp(*BITRATE.start(), *BITRATE.end()) }
    }

    /// Make the client's display and start its picture.
    fn start(&mut self, screen: Display, decoders: Vec<Mode>, cursor: bool, ceiling: u32, link: Link) -> Result<Accept, String> {
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
        self.live = Some(Live { desktop, pipeline, size: (screen.width, screen.height), placing, link, screen, decoders, cursor, mode, ceiling, files: BTreeMap::new() });
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
        Some(l)
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
        let (link, cursor, ceiling) = (old.link.clone(), old.cursor, old.ceiling);
        let (was_screen, was_decoders) = (old.screen, old.decoders.clone());
        drop(old.pipeline);
        self.runtime.block_on(old.desktop.close());
        let refresh = screen.refresh.clamp(1, 240);
        if let Some(mode) = mode {
            link.restart(Accept { mode, width: screen.width, height: screen.height, refresh, colour: "bt709".into() });
        }
        if let Err(e) = self.start(screen, decoders, cursor, ceiling, link.clone()) {
            // Back to what worked, rather than leave the client with nothing.
            tracing::warn!("couldn't start that picture ({e}); going back to the one before");
            let refresh = was_screen.refresh.clamp(1, 240);
            if let Some(mode) = was_decoders.iter().copied().find(|m| MODES.contains(m)) {
                link.restart(Accept { mode, width: was_screen.width, height: was_screen.height, refresh, colour: "bt709".into() });
            }
            if let Err(e) = self.start(was_screen, was_decoders, cursor, ceiling, link) {
                tracing::error!("couldn't restore the picture: {e}");
            }
        }
    }

    /// A file blob from the client: written into the user's Downloads folder under a name of
    /// this host's choosing, and answered with where it went.
    fn file(&mut self, event: &BlobEvent) {
        let allowed = self.allowed.files && !at_login_screen();
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

/// A login screen's session takes no files and shares no clipboard: nobody has logged in.
fn at_login_screen() -> bool {
    std::env::var("XDG_SESSION_CLASS").is_ok_and(|c| c == "greeter")
}

impl HostHandler for Agent {
    fn connected(&mut self, client: &ClientInfo, link: Link) -> Result<Accept, String> {
        self.left();
        let screen = client.display;
        tracing::info!(name = %client.name, ?screen, decoders = ?client.decoders, cursor_apart = client.cursor, "client");
        self.start(screen, client.decoders.clone(), client.cursor, 0, link)
    }

    fn message(&mut self, message: &Message) {
        if let Message::Configure { display, bitrate, decoders } = message {
            self.configure(*display, *bitrate, decoders.clone());
        }
    }

    fn blob(&mut self, event: &BlobEvent) {
        // Only files so far; a blob of another kind never gets an entry, so its content and
        // its end fall through.
        let is_file = match event {
            BlobEvent::Open { kind, .. } => *kind == BLOB_FILE,
            other => self.live.as_ref().is_some_and(|l| l.files.contains_key(&other.id())),
        };
        if is_file {
            self.file(event);
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
        if let Some(l) = self.stop() {
            drop(l.pipeline);
            self.runtime.block_on(l.desktop.close());
        }
    }
}
