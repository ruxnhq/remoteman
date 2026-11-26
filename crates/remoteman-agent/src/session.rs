//! What the agent does for a client: a virtual monitor made for its display, the picture out,
//! its input in. The connection itself is the shared core's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use remoteman_core::host::{Accept, ClientInfo, HostHandler, Link};
use std::sync::{Arc, mpsc};
use std::sync::atomic::{AtomicU32, Ordering};

use remoteman_core::media::{BLOB_CLIPBOARD, BLOB_FILE, CLIPBOARD_LIMIT, ClipboardHeader, FileHeader, safe_file_name};
use remoteman_desktop::clipboard::{self, Clipboard};
use remoteman_audio::{DefaultSink, MicSource, SpeakerSink};
use remoteman_core::{BlobEvent, Display, Event, Message, Mode, Packet, Side};
use remoteman_desktop::VirtualDisplay;

use crate::pipeline::{Pipeline, Settings};

/// What this host encodes: depends on the GPU (`crate::gpu`).
fn modes() -> Vec<Mode> {
    crate::gpu::modes()
}

/// The modes this host encodes among those the client decodes, the client's favourite first.
fn common(decoders: &[Mode]) -> Vec<Mode> {
    let ours = modes();
    decoders.iter().copied().filter(|m| ours.contains(m)).collect()
}

/// The first of `modes` that `open` succeeds with, and what it made. A card's encoder can lack
/// a mode the list has (NVENC before Ada has no AV1), so a refusal goes on to the next mode,
/// as the Windows host does; only when every one is refused is that the answer, with each reason.
fn first_opened<T>(modes: &[Mode], mut open: impl FnMut(Mode) -> anyhow::Result<T>) -> Result<(Mode, T), String> {
    let mut refused = Vec::new();
    for &mode in modes {
        match open(mode) {
            Ok(made) => return Ok((mode, made)),
            Err(e) => {
                tracing::warn!(?mode, "{e:#}");
                refused.push(format!("{mode:?}: {e:#}"));
            }
        }
    }
    Err(format!("no encoder opened: {}", refused.join("; ")))
}

/// A client being served.
/// A second display in the same session: a monitor and a picture of its own.
struct Second {
    desktop: VirtualDisplay,
    pipeline: Pipeline,
    size: (u32, u32),
    placing: tokio::task::JoinHandle<()>,
}

struct Live {
    /// The client's second display, while it has one.
    second: Option<Second>,
    /// Which of its displays the client's pointer is on (0, or 1 for the second).
    target: u8,
    desktop: VirtualDisplay,
    pipeline: Pipeline,
    size: (u32, u32),
    /// When this display's pointer was first put somewhere.
    pointed: Option<std::time::Instant>,
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
#[derive(Clone, Default)]
pub struct Allowed {
    pub files: Arc<std::sync::atomic::AtomicBool>,
    /// Sharing the clipboard both ways: a device could otherwise read whatever is copied here.
    pub clipboard: Arc<std::sync::atomic::AtomicBool>,
    /// What a paired device needs to get in while this computer is locked: nothing gets it
    /// in (`UNLOCK_OFF`), its user's password, or being paired alone. The session is unlocked
    /// for it and locked again when it leaves.
    pub unlock: Arc<std::sync::atomic::AtomicU8>,
    /// The client being served, for whoever asks how things stand (the control socket).
    pub now: Arc<std::sync::Mutex<Option<Now>>>,
    /// The way to the device being served, for sending it something from outside the
    /// session's own thread (a file, from the control socket).
    pub link: Arc<std::sync::Mutex<Option<Link>>>,
    /// What the device said became of the last file sent to it: how many have been answered,
    /// whether that one arrived, and where it put it or why not.
    pub sent: Arc<std::sync::Mutex<(u32, bool, String)>>,
}

/// The client being served, as the host's owner is shown it.
#[derive(Clone, serde::Serialize)]
pub struct Now {
    pub name: String,
    /// When it connected, in seconds since 1970.
    pub since: u64,
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    /// `extend`, `main` or `only`.
    pub placement: &'static str,
    /// What its picture is encoded as, e.g. "AV1 10-bit".
    pub picture: String,
    pub sound: bool,
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
    /// The microphone's packets so far: how many came, how many were missing in between, and
    /// the last one's number.
    mic_count: (u32, u32, Option<u32>),
    /// The client's camera, as one the desktop's programs can pick, and when it last had to
    /// ask the client for a keyframe.
    camera: Option<mpsc::SyncSender<remoteman_core::Frame>>,
    /// Camera frames that came before the message saying the camera is on (they travel
    /// apart, so the first frame can overtake it), with when each came.
    early: Vec<(std::time::Instant, remoteman_core::Frame)>,
    /// A mark of the last copy sent from the desktop to the client, to tell a repeat.
    last_copy: Arc<std::sync::Mutex<Option<(u64, std::time::Instant)>>>,
    /// Make the client's speaker the desktop's output while it is on (`--keep-output`: don't).
    take_output: bool,
    /// Counts what was copied here and sent, for the client to tell new from stale.
    copies: Arc<AtomicU32>,
    live: Option<Live>,
    /// The session was locked when this client came and was unlocked for it: this
    /// computer's own screens stay off while it is here, and the session is locked again
    /// when it goes.
    unlocked_for_client: bool,
    /// Wrong passwords for unlocking in a row, and until when no more are taken.
    wrong: (u32, Option<std::time::Instant>),
    /// A login screen whose client has gone, on its way to being ended.
    ending: Option<tokio::task::JoinHandle<()>>,
    /// For tests: how many clients have been told this computer is logging in.
    told_logging_in: u32,
}

/// How long a login screen's session is kept after its client has gone.
const LOGIN_SCREEN_LINGERS: std::time::Duration = std::time::Duration::from_secs(45);

pub const UNLOCK_OFF: u8 = 0;
pub const UNLOCK_WITH_PASSWORD: u8 = 1;
pub const UNLOCK_WITH_KEY: u8 = 2;
/// How many wrong passwords in a row before none are taken for a while. (The system's own
/// limit on wrong tries counts them too.)
const WRONG_TRIES: u32 = 5;
const WRONG_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// Whether the desktop this host serves is this user's own session, the one logind can lock
/// and unlock: it is if the session bus is the user's own. A host in a desktop made for a
/// test (a bus of its own) has nothing to do with that session's lock, and never touches it.
fn own_desktop() -> bool {
    let (Some(bus), Some(runtime)) = (std::env::var_os("DBUS_SESSION_BUS_ADDRESS"), std::env::var_os("XDG_RUNTIME_DIR")) else { return false };
    bus.to_string_lossy().split(',').next().is_some_and(|first| first == format!("unix:path={}/bus", runtime.to_string_lossy()))
}

/// For tests of what a locked computer does, with nothing locked or unlocked: with
/// `REMOTEMAN_TEST_LOCKED_PASSWORD` set, the host behaves as if its session were locked and
/// this were its user's password, and leaves the real session and PAM alone.
fn pretend_locked() -> Option<String> {
    std::env::var("REMOTEMAN_TEST_LOCKED_PASSWORD").ok()
}

/// A note, in this user's runtime folder, that the session is unlocked for a device and
/// has to be locked again.
pub fn unlocked_note() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("remoteman-unlocked-for-a-device")
}

/// Lock the session if a host that unlocked it for a device went without locking it again
/// (it was killed, or it crashed). Run when the host's service stops, however it stops.
pub fn lock_if_left_unlocked() {
    if !unlocked_note().exists() {
        return;
    }
    let locked = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(anyhow::Error::from).and_then(|rt| rt.block_on(remoteman_desktop::logind::set_locked(true)));
    match locked {
        Ok(()) => {
            let _ = std::fs::remove_file(unlocked_note());
            eprintln!("remoteman: the host went while this computer was unlocked for a device: it is locked again");
        }
        Err(e) => eprintln!("remoteman: THIS COMPUTER WAS LEFT UNLOCKED FOR A DEVICE AND COULD NOT BE LOCKED: {e:#}"),
    }
}

impl Drop for Agent {
    /// The host is stopping with a device still here.
    fn drop(&mut self) {
        self.relock();
    }
}

/// The lowest and highest bitrate a client may ask for, in bits a second.
const BITRATE: std::ops::RangeInclusive<u32> = 1_000_000..=500_000_000;

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

        Ok(Self { runtime, bitrate, refs, allowed, login_screen, clipboard_wanted: false, sound: None, microphone: None, mic_count: (0, 0, None), camera: None, early: Vec::new(), last_copy: Arc::default(), take_output, copies: Arc::default(), live: None, unlocked_for_client: false, wrong: (0, None), ending: None, told_logging_in: 0 })
    }

    /// What the picture is encoded at when the client's ceiling is `ceiling`.
    fn rate(&self, ceiling: u32) -> u32 {
        if ceiling == 0 { self.bitrate } else { ceiling.clamp(*BITRATE.start(), *BITRATE.end()) }
    }

    /// Make the client's display and start its picture. `tell`: the client already has a
    /// picture, and is told of the new one before its first frame (see [`Self::start_picture`]).
    #[allow(clippy::too_many_arguments)]
    fn start(&mut self, name: String, screen: Display, decoders: Vec<Mode>, cursor: bool, ceiling: u32, link: Link, tell: bool) -> Result<Accept, String> {
        let started = self.start_picture(name, screen, decoders, cursor, ceiling, link, tell);
        // Unlocked for a device that then got no picture: not left unlocked for nobody.
        if started.is_err() && self.live.is_none() {
            self.relock();
        }
        started
    }

    /// `tell`: the client is told of each mode tried just before its pipeline starts, so the
    /// first frame follows the news of the picture it belongs to (a new client hears of it in
    /// the `Accept` returned).
    #[allow(clippy::too_many_arguments)]
    fn start_picture(&mut self, name: String, screen: Display, decoders: Vec<Mode>, cursor: bool, ceiling: u32, link: Link, tell: bool) -> Result<Accept, String> {
        // The client's order of preference decides, among the modes the encoder opens for.
        let modes = common(&decoders);
        if modes.is_empty() {
            return Err("no video mode in common".into());
        }
        if screen.width % 2 != 0 || screen.height % 2 != 0 || screen.width < 64 || screen.height < 64 || screen.width > 8192 || screen.height > 8192 {
            return Err("display sizes must be even, from 64 to 8192".into());
        }
        let mut screen = screen;
        let mut refresh = screen.refresh.clamp(1, 240);
        // Unlocked for this device, in a room that may be empty: nothing of the desktop is
        // shown on the computer's own screens, so the device's display is the only one. The
        // exception is a device that asked to mirror: showing the computer's own screen is
        // what it asked for, and that screen has to be on for it.
        if self.unlocked_for_client && screen.placement != remoteman_core::Placement::Mirror {
            screen.placement = remoteman_core::Placement::Only;
            tracing::info!("unlocked for this device: this computer's own screens stay off while it is here");
        }
        let mirror = screen.placement == remoteman_core::Placement::Mirror;
        // (Just unlocked: the desktop takes a moment to let remote access back in.)
        let mut tries = if self.unlocked_for_client { 15 } else { 1 };
        let desktop = loop {
            tries -= 1;
            match self.runtime.block_on(VirtualDisplay::create(cursor, mirror)) {
                Ok(desktop) => break desktop,
                Err(e) if tries == 0 => return Err(format!("{e:#}")),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
            }
        };
        // Mirroring: the picture is the monitor's, at its size and scale, and no faster
        // than the client asked for.
        if let Some(m) = desktop.mirrored() {
            if m.width % 2 != 0 || m.height % 2 != 0 {
                self.runtime.block_on(desktop.close());
                return Err(format!("this computer's monitor is {} by {}: an odd size can't be sent", m.width, m.height));
            }
            (screen.width, screen.height, screen.scale) = (m.width, m.height, m.scale);
            refresh = refresh.min(m.refresh.clamp(1, 240));
            tracing::info!(width = m.width, height = m.height, refresh, "mirroring this computer's main monitor");
        }
        // (KDE: before the stream connects, or it is held to the monitor's first 60 Hz.)
        if let Err(e) = self.runtime.block_on(desktop.prepare(screen.width, screen.height, screen.scale, refresh)) {
            tracing::warn!("couldn't give the display its refresh rate and scale: {e:#}");
        }
        let (node, bitrate, refs) = (desktop.node(), self.rate(ceiling), self.refs);
        let opened = first_opened(&modes, |mode| {
            if tell {
                link.restart(Accept { mode, width: screen.width, height: screen.height, refresh, colour: "bt709".into(), sdr_white: 0, peak: 0 });
            }
            let settings = Settings {
                node,
                width: screen.width,
                height: screen.height,
                fps: refresh,
                scale: screen.scale.clamp(1.0, 4.0),
                mode,
                bitrate,
                cursor,
                refs,
            };
            Pipeline::start(settings, link.clone())
        });
        let (mode, pipeline) = match opened {
            Ok(opened) => opened,
            Err(e) => {
                self.runtime.block_on(desktop.close());
                return Err(e);
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
        self.live = Some(Live { second: None, target: 0, desktop, pipeline, size: (screen.width, screen.height), pointed: None, placing, name, link, screen, decoders, cursor, mode, ceiling, files: BTreeMap::new(), clipboard: None, copied: BTreeMap::new() });
        // The clipboard belongs to the display's session, so a new display shares it anew.
        self.share_clipboard(self.clipboard_wanted);
        *self.allowed.link.lock().unwrap() = self.live.as_ref().map(|l| l.link.clone());
        if let Some(l) = &self.live {
            let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let mut now = self.allowed.now.lock().unwrap();
            // A new picture for the same client keeps the time it came.
            let since = now.as_ref().filter(|n| n.name == l.name).map_or(since, |n| n.since);
            let placement = match screen.placement {
                remoteman_core::Placement::Extend => "extend",
                remoteman_core::Placement::Main => "main",
                remoteman_core::Placement::Only => "only",
                remoteman_core::Placement::Mirror => "mirror",
            };
            let picture = format!("{} {}-bit{}", if mode.codec == remoteman_core::Codec::Av1 { "AV1" } else { "HEVC" }, mode.depth, if mode.chroma == remoteman_core::Chroma::C444 { " 4:4:4" } else { "" });
            *now = Some(Now { name: l.name.clone(), since, width: screen.width, height: screen.height, refresh, placement, picture, sound: self.sound.is_some() });
        }
        Ok(Accept { mode, width: screen.width, height: screen.height, refresh, colour: "bt709".into(), sdr_white: 0, peak: 0 })
    }

    /// A device has come to a locked computer (GNOME lets nothing in from outside while it
    /// is locked): unlock it, if its owner allows that and the device has shown what the
    /// owner asks for. `Err` is what the device is told; one that starts "locked: " invites
    /// it to try again with the password.
    fn unlock_for(&mut self, client: &ClientInfo) -> Result<(), String> {
        // (For tests: a desktop that only says it is locked, with a password of the test's.)
        let pretend = pretend_locked();
        let locked = pretend.is_some() || (own_desktop() && self.runtime.block_on(remoteman_desktop::logind::locked()).unwrap_or(false));
        if self.login_screen.is_some() || !locked || self.unlocked_for_client {
            return Ok(());
        }
        match self.allowed.unlock.load(Ordering::Relaxed) {
            UNLOCK_WITH_PASSWORD => {
                let user = crate::pam::user().ok_or("this computer is locked, and its user couldn't be named")?;
                // A device that keeps guessing is kept waiting.
                if let Some(until) = self.wrong.1.filter(|until| *until > std::time::Instant::now()) {
                    let minutes = (until - std::time::Instant::now()).as_secs() / 60 + 1;
                    return Err(format!("this computer is locked, and after too many wrong passwords it takes no more for {minutes} min"));
                }
                let Some(password) = client.unlock.as_deref().filter(|p| !p.is_empty()) else {
                    return Err(format!("locked: This computer is locked. Enter the password of {user} on it."));
                };
                let checked = match &pretend {
                    Some(right) => Ok(password == right),
                    None => crate::pam::check(&user, password),
                };
                match checked {
                    Ok(true) => self.wrong = (0, None),
                    Ok(false) => {
                        self.wrong.0 += 1;
                        tracing::warn!(device = client.name, tries = self.wrong.0, "a wrong password for unlocking this computer");
                        if self.wrong.0 >= WRONG_TRIES {
                            self.wrong = (0, Some(std::time::Instant::now() + WRONG_WAIT));
                        }
                        return Err("locked: That password is not right.".into());
                    }
                    Err(e) => return Err(format!("this computer is locked, and the password couldn't be checked: {e}")),
                }
            }
            UNLOCK_WITH_KEY => {}
            _ => return Err("this computer is locked, and it is set not to be unlocked by devices".into()),
        }
        if pretend.is_none() {
            self.runtime.block_on(remoteman_desktop::logind::set_locked(false)).map_err(|e| format!("this computer is locked and couldn't be unlocked: {e:#}"))?;
        }
        tracing::info!(device = client.name, "this computer was locked: unlocked for a paired device; it locks again when the device leaves");
        self.unlocked_for_client = true;
        // Should this program die before it has locked the session again, what stops it
        // finds this note and locks it (`--lock-if-left-unlocked`, run by the service).
        if pretend.is_none() {
            let _ = std::fs::write(unlocked_note(), "");
        }
        Ok(())
    }

    /// The client has gone. `left` does the rest; this, last of all, locks a session that was
    /// unlocked for it.
    fn relock(&mut self) {
        if std::mem::take(&mut self.unlocked_for_client) {
            if pretend_locked().is_some() {
                tracing::info!("the device has left: this computer is locked again");
                return;
            }
            match self.runtime.block_on(remoteman_desktop::logind::set_locked(true)) {
                Ok(()) => {
                    let _ = std::fs::remove_file(unlocked_note());
                    tracing::info!("the device has left: this computer is locked again");
                }
                Err(e) => tracing::error!("THIS COMPUTER WAS UNLOCKED FOR A DEVICE AND COULD NOT BE LOCKED AGAIN: {e:#}"),
            }
        }
    }

    /// Give the client a second display beside its first, or take it away. It gets a monitor
    /// of its own on the side the client names (the right, unless it says) and a picture of its own, in the codec of
    /// the first; its pointer is drawn into the picture.
    fn second_display(&mut self, on: bool, display: Option<Display>, side: Side) {
        let Some(l) = self.live.as_mut() else { return };
        if let Some(old) = l.second.take() {
            old.placing.abort();
            drop(old.pipeline);
            self.runtime.block_on(old.desktop.close());
            l.target = 0;
            tracing::info!("the device's second display is gone");
        }
        let (true, Some(screen)) = (on, display) else { return };
        let link = l.link.clone();
        let gone = |why: String| link.send_message(Message::Display { index: 1, on: false, display: None, side: Default::default(), message: why });
        if self.login_screen.is_some() {
            return gone("a login screen has one display".into());
        }
        if screen.width % 2 != 0 || screen.height % 2 != 0 || screen.width < 64 || screen.height < 64 || screen.width > 8192 || screen.height > 8192 {
            return gone("display sizes must be even, from 64 to 8192".into());
        }
        let refresh = screen.refresh.clamp(1, 240);
        // The first display must be in its place before the second is made: asked for in the
        // session's first moments, the second would be taken for the first by whichever of
        // the two is placed, and neither would end up where it belongs.
        if !l.placing.is_finished() {
            let _ = self.runtime.block_on(async { tokio::time::timeout(std::time::Duration::from_secs(6), &mut l.placing).await });
        }
        let desktop = match self.runtime.block_on(VirtualDisplay::create_beside(false)) {
            Ok(d) => d,
            Err(e) => return gone(format!("{e:#}")),
        };
        let second = l.link.second();
        // Said before its first frame can be sent.
        second.restart(Accept { mode: l.mode, width: screen.width, height: screen.height, refresh, colour: "bt709".into(), sdr_white: 0, peak: 0 });
        if let Err(e) = self.runtime.block_on(desktop.prepare(screen.width, screen.height, screen.scale, refresh)) {
            tracing::warn!("couldn't give the second display its refresh rate and scale: {e:#}");
        }
        let settings = Settings { node: desktop.node(), width: screen.width, height: screen.height, fps: refresh, scale: screen.scale.clamp(1.0, 4.0), mode: l.mode, bitrate: self.bitrate.min(40_000_000), cursor: false, refs: self.refs };
        let pipeline = match Pipeline::start(settings, second) {
            Ok(p) => p,
            Err(e) => {
                self.runtime.block_on(desktop.close());
                return gone(format!("{e:#}"));
            }
        };
        let place = desktop.place_on(side);
        let placing = self.runtime.spawn(async move {
            if let Err(e) = place.await {
                tracing::warn!("couldn't place the second display: {e:#}");
            }
        });
        tracing::info!(width = screen.width, height = screen.height, refresh, ?side, "the device has a second display");
        l.second = Some(Second { desktop, pipeline, size: (screen.width, screen.height), placing });
    }

    /// Stop the picture and take the display away. Files half received are thrown away.
    fn stop(&mut self) -> Option<Live> {
        let mut l = self.live.take()?;
        *self.allowed.link.lock().unwrap() = None;
        // The second display goes with the first.
        if let Some(second) = l.second.take() {
            second.placing.abort();
            drop(second.pipeline);
            self.runtime.block_on(second.desktop.close());
        }
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
        self.drop_microphone();
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
        // The frames that overtook this message are kept for the camera it switches on.
        self.camera = None;
        if !on {
            self.early.clear();
        }
        let (Some(l), true, None) = (self.live.as_ref(), on, self.login_screen) else { return };
        if !codec.is_empty() && codec != "h264" {
            tracing::warn!(codec, "the device's camera is sent in a codec this host doesn't take; only h264 is");
            return;
        }
        let usual = remoteman_camera::Options::default();
        let or = |said: u32, usual: u32| if said == 0 { usual } else { said };
        let options = remoteman_camera::Options { width: or(width, usual.width), height: or(height, usual.height), fps: or(fps, usual.fps), ..usual };
        let (name, link) = (format!("{} camera (remoteman)", l.name), l.link.clone());
        // Decoding takes a few milliseconds a frame: on a thread of its own, so that it never
        // holds up the client's input, which comes through here too. A second of frames may
        // wait while the camera starts.
        let (tx, rx) = mpsc::sync_channel::<remoteman_core::Frame>(30);
        let started = std::thread::Builder::new().name("camera".into()).spawn(move || {
            let mut camera = match remoteman_camera::Camera::start_with(&name, options) {
                Ok(camera) => camera,
                Err(e) => return tracing::warn!("couldn't make a camera for the device: {e:#}"),
            };
            tracing::info!(node = camera.node_name(), loopback = ?camera.loopback_device(), "the device's camera is on the desktop");
            let mut t = CameraTally::default();
            while let Ok(frame) = rx.recv() {
                let began = std::time::Instant::now();
                if let Some(last) = t.last.replace(began) {
                    t.gap = t.gap.max(began - last);
                }
                t.frames += 1;
                t.keys += frame.key as u32;
                let pushed = camera.push(&frame.data, frame.key, frame.capture_us);
                t.decode = t.decode.max(began.elapsed());
                match pushed {
                    remoteman_camera::Pushed::Shown => t.shown += 1,
                    remoteman_camera::Pushed::Waiting => t.waiting += 1,
                    // A frame the decoder can't use: nothing is shown until the device sends
                    // a keyframe.
                    remoteman_camera::Pushed::Lost => {
                        t.lost += 1;
                        if t.asked_at.is_none_or(|at| at.elapsed() > std::time::Duration::from_millis(250)) {
                            t.asked_at = Some(std::time::Instant::now());
                            t.asked += 1;
                            link.send_message(Message::CameraKeyframe);
                        }
                    }
                }
            }
            // Switched off, or the client went: the camera leaves the desktop here.
            tracing::info!(
                frames = t.frames,
                keyframes = t.keys,
                shown = t.shown,
                undecodable = t.lost,
                skipped = t.waiting,
                asked_for_keyframe = t.asked,
                longest_gap_ms = t.gap.as_millis() as u64,
                longest_decode_us = t.decode.as_micros() as u64,
                "the device's camera is off the desktop"
            );
        });
        match started {
            Ok(_) => {
                // The frames that overtook the message, unless they are a camera's last ones
                // from before.
                for (came, frame) in self.early.drain(..) {
                    if came.elapsed() < std::time::Duration::from_millis(500) {
                        let _ = tx.try_send(frame);
                    }
                }
                self.camera = Some(tx);
            }
            Err(e) => tracing::warn!("couldn't start the camera's thread: {e}"),
        }
    }

    /// Take the device's microphone off the desktop, saying how it went.
    fn drop_microphone(&mut self) {
        if self.microphone.take().is_some() {
            let (packets, missing, _) = std::mem::replace(&mut self.mic_count, (0, 0, None));
            tracing::info!(packets, missing, "the device's microphone is off the desktop");
        }
    }

    /// Take the device's camera off the desktop (its thread says how it went and ends).
    fn drop_camera(&mut self) {
        self.camera = None;
        self.early.clear();
    }

    /// Share the clipboard with the client, both ways, or stop. Only where the computer's
    /// owner allowed it, and never at a login screen.
    fn share_clipboard(&mut self, on: bool) {
        let allowed = self.allowed.clipboard.load(Ordering::Relaxed) && self.login_screen.is_none();
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
        let still_allowed = self.allowed.clipboard.clone();
        // Whatever is copied on the desktop goes to the client: text if there is any, else a
        // picture.
        let watching = self.runtime.spawn(async move {
            while let Some(types) = changes.next().await {
                // Switched off by the computer's owner since: nothing more leaves, from this
                // very copy on (the sharing itself is taken down at the device's next sign
                // of life, see `clipboard_switched_off`).
                if !still_allowed.load(Ordering::Relaxed) {
                    continue;
                }
                let (read_as, send_as) = match clipboard::text_type(&types) {
                    Some(text) => (text.to_owned(), "text/plain;charset=utf-8"),
                    None if types.iter().any(|t| t == "image/png") => ("image/png".to_owned(), "image/png"),
                    None => continue,
                };
                match desktop.read(&read_as).await {
                    Ok(bytes) if !bytes.is_empty() => {
                        // The desktop announces one copy more than once (twice, as a rule):
                        // the same thing again within two seconds is not sent again.
                        let mark = {
                            use std::hash::{Hash, Hasher};
                            let mut h = std::collections::hash_map::DefaultHasher::new();
                            (send_as, &bytes).hash(&mut h);
                            h.finish()
                        };
                        let now = std::time::Instant::now();
                        let before = last_copy.lock().unwrap().replace((mark, now));
                        if before.is_some_and(|(m, at)| m == mark && now - at < std::time::Duration::from_secs(2)) {
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

    /// The computer's owner switched the clipboard off while it was being shared: stop, as if
    /// the device had. The standalone host is told so the moment it happens; the agent of a
    /// daemon has nobody to tell it, so it looks whenever the device does something (and
    /// until then the watcher above lets nothing out).
    fn clipboard_switched_off(&mut self) {
        if self.live.as_ref().is_some_and(|l| l.clipboard.is_some()) && !self.allowed.clipboard.load(Ordering::Relaxed) {
            self.clipboard_wanted = false;
            self.share_clipboard(false);
        }
    }

    /// A clipboard blob from the client: what was copied on the device becomes what the
    /// desktop pastes.
    fn copied(&mut self, event: &BlobEvent) {
        self.clipboard_switched_off();
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
        // The same modes in common give the same picture, and so does a client whose favourite
        // is the one it has. (Not merely the first in common: the encoder may have refused it.)
        let wanted = common(&decoders);
        if screen == l.screen && (wanted.first() == Some(&l.mode) || wanted == common(&l.decoders)) {
            l.decoders = decoders;
            return;
        }
        tracing::info!(?screen, ?decoders, "the client asked for another picture");
        // The old picture goes first, display and all; then the new one starts, and the client
        // is told of it before its first frame.
        let Some(old) = self.stop() else { return };
        let (link, cursor, ceiling, name) = (old.link.clone(), old.cursor, old.ceiling, old.name.clone());
        let (was_screen, was_decoders) = (old.screen, old.decoders.clone());
        drop(old.pipeline);
        self.runtime.block_on(old.desktop.close());
        // (Not `start`, which locks a computer that was unlocked for a device left with no
        // picture: this device is still here, and gets the picture it had back.)
        if let Err(e) = self.start_picture(name.clone(), screen, decoders, cursor, ceiling, link.clone(), true) {
            // Back to what worked, rather than leave the client with nothing.
            tracing::warn!("couldn't start that picture ({e}); going back to the one before");
            if let Err(e) = self.start(name, was_screen, was_decoders, cursor, ceiling, link, true) {
                tracing::error!("couldn't restore the picture: {e}");
            }
        }
    }

    /// A file blob from the client: written into the user's Downloads folder under a name of
    /// this host's choosing, and answered with where it went.
    fn file(&mut self, event: &BlobEvent) {
        let allowed = self.allowed.files.load(Ordering::Relaxed) && self.login_screen.is_none();
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
                let result = if whole { f.file.sync_all().and_then(|()| remoteman_core::receive::settle(&f.part, &f.done)).map_err(|e| e.to_string()) } else { Err("it didn't arrive whole".to_owned()) };
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
        // (For tests of what a client does with a computer that is still logging its user in:
        // with `REMOTEMAN_TEST_LOGGING_IN=N` the first N clients are told so and turned away.)
        if let Some(n) = std::env::var("REMOTEMAN_TEST_LOGGING_IN").ok().and_then(|n| n.parse::<u32>().ok()) {
            self.told_logging_in += 1;
            if self.told_logging_in <= n {
                return Err("logging in: this computer is logging in for you".into());
            }
        }
        // A login screen that was about to be ended has a client again.
        if let Some(ending) = self.ending.take() {
            ending.abort();
        }
        if let Err(why) = self.unlock_for(client) {
            // (Said here too: the device is told, but its owner looking at this log is not.)
            tracing::info!(device = client.name, "turned away: {}", why.strip_prefix("locked: ").unwrap_or(&why));
            return Err(why);
        }
        // Logged in on the computer's own login screen (the daemon says so, never the device):
        // the session opened on the monitors, unlocked, in a room that may be empty. As if it
        // had been unlocked for the device: the monitors are off while it is here, and the
        // session is locked when it leaves.
        if client.logged_in_here && self.login_screen.is_none() && !self.unlocked_for_client {
            tracing::info!(device = client.name, "this session was logged into from the device on this computer's login screen: it locks when the device leaves");
            self.unlocked_for_client = true;
            let _ = std::fs::write(unlocked_note(), "");
        }
        // Plasma Login's login screen: KDE's portal must let the host in without asking, and
        // the login screen's user has its own permission store, which exists only while the
        // login screen runs. Its agent writes the grant there itself, and only there; when a
        // device comes, since KWin may not have been up yet when the agent started.
        if self.login_screen == Some(false) && self.runtime.block_on(remoteman_desktop::kde::present()) {
            match self.runtime.block_on(remoteman_desktop::kde::allow_without_asking()) {
                Ok(()) => tracing::info!("the login screen lets this host in without asking"),
                Err(e) => tracing::warn!("couldn't let this host into the login screen without asking: {e:#}"),
            }
        }
        let mut screen = client.display;
        // The computer's own login screen (not one made for the device): it is shown as it is,
        // so that nothing about the monitors is changed by the login screen's user.
        if self.login_screen == Some(false) {
            screen.placement = remoteman_core::Placement::Mirror;
        }
        tracing::info!(name = %client.name, ?screen, decoders = ?client.decoders, cursor_apart = client.cursor, "client");
        self.start(client.name.clone(), screen, client.decoders.clone(), client.cursor, 0, link, false)
    }

    fn mic(&mut self, packet: &Packet) {
        if let Some(microphone) = &self.microphone {
            // A hole in the numbers is packets lost on the way (a pause leaves none).
            let (packets, missing, last) = &mut self.mic_count;
            *packets += 1;
            if let (Some(last), false) = (*last, packet.start) {
                let gap = packet.seq.wrapping_sub(last).wrapping_sub(1);
                if gap < 1000 {
                    *missing += gap;
                }
            }
            *last = Some(packet.seq);
            microphone.push(remoteman_audio::Packet { seq: packet.seq, capture_us: packet.time_us, data: packet.data.clone() });
        }
    }

    fn camera(&mut self, frame: &remoteman_core::Frame) {
        // If the camera's thread is a second behind, this frame is left out; what follows it
        // can't be decoded then, and the thread asks the device for a keyframe.
        match &self.camera {
            Some(camera) => {
                let _ = camera.try_send(frame.clone());
            }
            // Not on yet, as far as this side has heard: kept for half a second.
            None => {
                self.early.retain(|(came, _)| came.elapsed() < std::time::Duration::from_millis(500));
                if self.early.len() < 30 {
                    self.early.push((std::time::Instant::now(), frame.clone()));
                }
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
            Message::Sound { on } => {
                self.send_sound(*on);
                if let Some(now) = self.allowed.now.lock().unwrap().as_mut() {
                    now.sound = self.sound.is_some();
                }
            }
            Message::Mic { on, .. } => self.take_microphone(*on),
            Message::Display { index: 1, on, display, side, .. } => self.second_display(*on, *display, *side),
            Message::DisplayKeyframe { index: 1 } => {
                if let Some(second) = self.live.as_ref().and_then(|l| l.second.as_ref()) {
                    second.pipeline.request_keyframe();
                }
            }
            // The device's word on a file this computer sent it.
            Message::FileResult { ok, message, .. } => {
                tracing::info!(ok, said = message, "the device answered for a file sent to it");
                let mut sent = self.allowed.sent.lock().unwrap();
                *sent = (sent.0 + 1, *ok, message.clone());
            }
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
        self.clipboard_switched_off();
        let Some(l) = &mut self.live else { return };
        // The desktop takes a moment to notice where a new display's pointer has first been
        // put: a button pressed sooner lands on nothing (seen on GNOME Shell 51: a click
        // 80 ms after the first position is lost, one 190 ms after lands). A tap on a touch
        // device is exactly that, a position and a press at once, so the first press of a
        // session waits out that moment.
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(200);
        match &event {
            Event::PointerTo { .. } | Event::PointerBy { .. } if l.pointed.is_none() => l.pointed = Some(std::time::Instant::now()),
            Event::Button { down: true, .. } => {
                if let Some(wait) = l.pointed.and_then(|at| SETTLE.checked_sub(at.elapsed())) {
                    std::thread::sleep(wait);
                }
            }
            _ => {}
        }
        // Which of its displays the pointer is on from here on.
        if let Event::Display(index) = event {
            l.target = index;
            return;
        }
        // The pointer and touches go to the display they are on; keys to the desktop, by way
        // of the first.
        let pointing = matches!(event, Event::PointerTo { .. } | Event::PointerBy { .. } | Event::Button { .. } | Event::Scroll { .. } | Event::Touch { .. });
        let (desktop, size, scale) = match &l.second {
            Some(second) if l.target == 1 && pointing => (&second.desktop, second.size, f32::from_bits(second.pipeline.scale.load(std::sync::atomic::Ordering::Relaxed))),
            _ => (&l.desktop, l.size, f32::from_bits(l.pipeline.scale.load(std::sync::atomic::Ordering::Relaxed))),
        };
        if let Err(e) = self.runtime.block_on(desktop.inject(&event, size, scale)) {
            tracing::warn!("input: {e:#}");
        }
    }

    fn left(&mut self) {
        *self.allowed.now.lock().unwrap() = None;
        self.clipboard_wanted = false;
        self.drop_camera();
        self.drop_microphone();
        self.sound = None;
        if let Some(l) = self.stop() {
            drop(l.pipeline);
            self.runtime.block_on(l.desktop.close());
            // A headless login screen exists for one client; when that client has logged in
            // (and been moved to its session) or gone, nobody else will ever see it, and GDM
            // leaves ending it to us.
            // Not at once: a password typed a moment before the client went is still being
            // checked, and GDM drops the session it was about to start if the login screen
            // goes first (seen: a session opened 11 s after the password and lost when the
            // device left 7 s later). A client that comes back meanwhile keeps the screen.
            if self.login_screen == Some(true) {
                tracing::info!("the client has left this login screen: its session ends in {} s unless a client comes back", LOGIN_SCREEN_LINGERS.as_secs());
                self.ending = Some(self.runtime.spawn(async {
                    tokio::time::sleep(LOGIN_SCREEN_LINGERS).await;
                    tracing::info!("ending the login screen's session");
                    if let Err(e) = remoteman_desktop::logind::log_out().await {
                        tracing::warn!("couldn't end the login screen's session: {e:#}");
                    }
                }));
            }
        }
        self.relock();
    }
}

/// How a device's camera went, for the log when it is switched off.
#[derive(Default)]
struct CameraTally {
    frames: u32,
    keys: u32,
    shown: u32,
    /// Frames the decoder couldn't use, and frames skipped while waiting for a keyframe.
    lost: u32,
    waiting: u32,
    /// Times the device was asked for a keyframe, and when it last was.
    asked: u32,
    asked_at: Option<std::time::Instant>,
    last: Option<std::time::Instant>,
    /// The longest wait between two frames, and the longest a frame took to decode and show.
    gap: std::time::Duration,
    decode: std::time::Duration,
}

#[cfg(test)]
mod tests {
    use super::*;
    use remoteman_core::{Chroma, Codec};

    /// What the Mac viewer offers with sharp text off: AV1 first.
    fn offered() -> Vec<Mode> {
        let m = |codec, chroma| Mode { codec, chroma, depth: 10 };
        vec![m(Codec::Av1, Chroma::C420), m(Codec::Hevc, Chroma::C444), m(Codec::Hevc, Chroma::C420)]
    }

    #[test]
    fn a_mode_the_encoder_refuses_gives_way_to_the_next() {
        let mut tried = Vec::new();
        let opened = first_opened(&offered(), |mode| {
            tried.push(mode);
            if mode.codec == Codec::Av1 { anyhow::bail!("no 10-bit encode for this codec") } else { Ok(()) }
        });
        assert_eq!(opened.map(|(mode, _)| mode), Ok(offered()[1]));
        assert_eq!(tried, offered()[..2]);
    }

    #[test]
    fn the_favourite_is_taken_when_it_opens() {
        let mut tried = 0;
        let opened = first_opened(&offered(), |_| {
            tried += 1;
            Ok(())
        });
        assert_eq!(opened.map(|(mode, _)| mode), Ok(offered()[0]));
        assert_eq!(tried, 1);
    }

    #[test]
    fn when_every_mode_is_refused_each_reason_is_said() {
        let opened = first_opened(&offered(), |mode| -> anyhow::Result<()> { anyhow::bail!("refused {:?}", mode.codec) });
        let e = opened.unwrap_err();
        assert!(e.starts_with("no encoder opened: "), "{e}");
        assert_eq!(e.matches("refused Av1").count(), 1, "{e}");
        assert_eq!(e.matches("refused Hevc").count(), 2, "{e}");
    }

    #[test]
    fn nothing_to_try_is_an_error() {
        assert!(first_opened(&[], |_| Ok(())).is_err());
    }
}
