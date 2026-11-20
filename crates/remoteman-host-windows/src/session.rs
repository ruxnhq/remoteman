//! What the host does for a client: the monitor brought to its size, the picture out, its input
//! in, the cursor apart if it asks. The connection itself is the shared core's. Built like the
//! Linux agent's session, with less: no camera, no second display, no unlocking, and the
//! display is the driver's one monitor brought to the device's size, not a monitor of its own.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use remoteman_core::host::{Accept, ClientInfo, HostHandler, Link};
use remoteman_core::media::{BLOB_CLIPBOARD, BLOB_FILE, CLIPBOARD_LIMIT, ClipboardHeader};
use remoteman_core::{BlobEvent, Chroma, Codec, Display, Event, Message, Mode, Packet, Placement};

use crate::clipboard::Clipboard;
use crate::control::{Allowed, Now};
use crate::files::Files;
use crate::sound::{self, Microphone, Speaker};

use crate::cursor::Cursor;
use crate::display::{self, Area, Fit, Place, Screen};
use crate::encode;
use crate::input::Injector;
use crate::pipeline::{self, Pipeline};

/// The lowest and highest bitrate a client may ask for, in bits a second.
const BITRATE: std::ops::RangeInclusive<u32> = 1_000_000..=150_000_000;

/// A client being served.
struct Live {
    pipeline: Pipeline,
    /// Reporting the cursor, if the client draws it itself.
    cursor: Option<Cursor>,
    injector: Injector,
    /// What the client asked for, to start the picture afresh from when it changes its mind.
    link: Link,
    screen: Display,
    decoders: Vec<Mode>,
    cursor_apart: bool,
    /// The modes the picture was chosen from: the client's and this host's in common. (Which
    /// of them it became is the encoder's doing, and not always the first.)
    common: Vec<Mode>,
    /// The most the picture may use, as the client last said (0: this host's own figure).
    ceiling: u32,
    /// The client's name, for what the owner is shown.
    name: String,
    /// Files being received.
    files: Files,
    /// The clipboard shared with the client, while it is.
    clipboard: Option<Clipboard>,
    /// What the client copied, on its way in: its type and the content so far.
    copied: BTreeMap<u32, (String, Vec<u8>)>,
    /// What the picture is encoded as: a second display's picture is the same.
    mode: Mode,
    /// The client's second display, while it has one: its picture, and its pointer's way in.
    second: Option<(Pipeline, Injector, Option<Cursor>)>,
    /// Which of its displays the client's pointer is on (0, or 1 for the second).
    target: u8,
    /// HDR switched on for the client's monitor, put back as it was when this goes (held for
    /// that alone).
    _hdr: Option<crate::hdr::Switched>,
}

pub struct Host {
    screen: Screen,
    bitrate: u32,
    fit: Fit,
    allowed: Allowed,
    /// The client wants the clipboard shared (kept across a restart of the picture).
    clipboard_wanted: bool,
    /// Counts what was copied here and sent, for the client to tell new from stale.
    copies: Arc<AtomicU32>,
    /// The desktop's sound going to the client.
    sound: Option<Speaker>,
    /// The client's microphone, played into a virtual device, and its packets so far: how
    /// many came, how many were missing in between, and the last one's number.
    microphone: Option<Microphone>,
    mic_count: (u32, u32, Option<u32>),
    live: Option<Live>,
    /// The PC was locked when this client came and was unlocked with its password: it is
    /// locked again when the client goes.
    unlocked_for_client: bool,
    /// Wrong passwords for unlocking in a row, and until when no more are taken.
    wrong: (u32, Option<std::time::Instant>),
    /// The PC is kept awake while a device is connected (`awake.rs`).
    awake: Option<crate::awake::Awake>,
    /// The PC's speakers are switched to Steam's while the device listens (`speakers.rs`).
    switched: Option<crate::speakers::Switched>,
}

/// How many wrong passwords in a row before none are taken for a while (Windows' own limit on
/// wrong tries counts them too), and for how long.
const WRONG_TRIES: u32 = 5;
const WRONG_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

impl Host {
    pub fn new(screen: Screen, bitrate: u32, fit: Fit, allowed: Allowed) -> Self {
        Self { screen, bitrate, fit, allowed, clipboard_wanted: false, copies: Arc::default(), sound: None, microphone: None, mic_count: (0, 0, None), live: None, unlocked_for_client: false, wrong: (0, None), awake: None, switched: None }
    }

    /// A device has come to a locked PC (or one at its sign-in screen), which Windows shows on
    /// the PC's main screen only. As on the other hosts (docs/PROTOCOL.md, "A locked host"):
    /// the device is asked for the password (or PIN) of the PC's user, and with it the host
    /// types it into Windows' lock screen, as its owner would. `Err` is what the device is
    /// told; one that starts "locked: " invites it to try again with the password.
    fn unlock_for(&mut self, client: &ClientInfo) -> Result<(), String> {
        if !crate::desk::locked() {
            return Ok(());
        }
        if !crate::desk::is_system() {
            return Err("this computer is locked, and this host can't unlock it (it wasn't started by its service)".into());
        }
        // A device that keeps guessing is kept waiting.
        if let Some(until) = self.wrong.1.filter(|until| *until > std::time::Instant::now()) {
            let minutes = (until - std::time::Instant::now()).as_secs() / 60 + 1;
            return Err(format!("this computer is locked, and after too many wrong passwords it takes no more for {minutes} min"));
        }
        let user = crate::desk::user_name().map_or_else(|| "its user".to_string(), |u| u.to_string());
        let Some(password) = client.unlock.as_deref().filter(|p| !p.is_empty()) else {
            tracing::info!(device = client.name, "this computer is locked: the device is asked for the password");
            return Err(format!("locked: This computer is locked. Enter the password or PIN of {user} on it."));
        };
        match crate::desk::unlock_with(password) {
            Ok(true) => {
                self.wrong = (0, None);
                self.unlocked_for_client = true;
                tracing::info!(device = client.name, "this computer was locked: unlocked for a paired device; it locks again when the device leaves");
                Ok(())
            }
            Ok(false) => {
                self.wrong.0 += 1;
                tracing::warn!(device = client.name, tries = self.wrong.0, "a wrong password for unlocking this computer");
                if self.wrong.0 >= WRONG_TRIES {
                    self.wrong = (0, Some(std::time::Instant::now() + WRONG_WAIT));
                }
                Err("locked: That password is not right.".into())
            }
            Err(e) => Err(format!("this computer is locked and couldn't be unlocked: {e:#}")),
        }
    }

    /// What the picture is encoded at when the client's ceiling is `ceiling`.
    fn rate(&self, ceiling: u32) -> u32 {
        if ceiling == 0 { self.bitrate } else { ceiling.clamp(*BITRATE.start(), *BITRATE.end()) }
    }

    /// The modes this host encodes among those the client decodes, the client's favourite first.
    fn common(decoders: &[Mode]) -> Vec<Mode> {
        decoders.iter().copied().filter(|m| encode::MODES.contains(m)).collect()
    }

    /// Bring the monitor to the client's display and start its picture. `held`: nothing is
    /// sent until the pipeline is released (see [`Self::restart`]).
    fn start(&mut self, name: String, screen: Display, decoders: Vec<Mode>, cursor_apart: bool, ceiling: u32, link: Link, held: bool) -> Result<Accept, String> {
        let modes = Self::common(&decoders);
        if modes.is_empty() {
            return Err("no video mode in common".into());
        }
        if screen.width % 2 != 0 || screen.height % 2 != 0 || screen.width < 64 || screen.height < 64 || screen.width > 8192 || screen.height > 8192 {
            return Err("display sizes must be even, from 64 to 8192".into());
        }
        // The client's scale (how large the desktop is drawn) isn't acted on: Windows has no
        // documented way to set a monitor's scale for the user.
        let want = display::Mode { width: screen.width, height: screen.height, refresh: screen.refresh.clamp(1, 240) };
        // A device that asks to mirror is shown the monitor as it is, no faster than it asked.
        // Not on a PC with screens of its own: the driver's monitor isn't one of them, so there
        // is nothing there to mirror yet, and the device gets an extra display instead.
        let mirror = screen.placement == Placement::Mirror && !self.screen.pc();
        // Otherwise a monitor of its own where the driver has one to give, else the driver's
        // one monitor at the device's size.
        let place = match screen.placement {
            Placement::Main => Place::Main,
            Placement::Only => Place::Only,
            _ => Place::Beside,
        };
        let own = if mirror || self.fit == Fit::Keep { None } else { self.screen.give(want, place).inspect_err(|e| tracing::warn!("couldn't give the device a monitor of its own: {e:#}")).ok().flatten() };
        let got = match own {
            Some(mode) => mode,
            None => self.screen.fit(want, if mirror { Fit::Keep } else { self.fit }).map_err(|e| format!("{e:#}"))?,
        };
        let (_, at) = self.screen.now().map_err(|e| format!("{e:#}"))?;
        // A display that asked for HDR: the monitor shows it (if the encoder can say so), and
        // its picture is BT.2020 PQ. A mirrored monitor is left as its owner has it.
        let hdr_wanted = screen.hdr && self.allowed.hdr.load(Ordering::Relaxed);
        // A device's own monitor is brought to what the device asked, either way (a host stopped
        // in the middle of a session may have left it in HDR), and left with HDR off; the PC's
        // own monitor, used when there is no monitor to give, is switched only for a device
        // that asked, and put back as it was.
        let hdr = match (mirror, own.is_some()) {
            (true, _) => None,
            (false, true) => crate::hdr::Switched::to(self.screen.device(), hdr_wanted, Some(false)),
            (false, false) if hdr_wanted => crate::hdr::Switched::to(self.screen.device(), true, None),
            _ => None,
        };
        if hdr.is_some() {
            // Windows takes a moment to give the monitor its wide surface.
            std::thread::sleep(std::time::Duration::from_millis(700));
        }
        let sdr_white = crate::hdr::sdr_white(self.screen.device());
        let area = Arc::new(Mutex::new(at));
        // A client that doesn't draw the pointer gets it drawn into the picture.
        let drawn = Arc::new(crate::cursor::Drawn::default());
        let refresh = if mirror { got.refresh.min(want.refresh) } else { got.refresh }.clamp(1, 240);
        let (device, shared) = (self.screen.device().to_owned(), area.clone());
        let settings = pipeline::Settings {
            driver: self.screen.driver(),
            monitor: self.screen.monitor(),
            modes: modes.clone(),
            fps: refresh,
            bitrate: self.rate(ceiling),
            nudge_at: (at.left, at.top),
            expect: Some((at.width, at.height)),
            held,
            status: true,
            hdr: hdr_wanted,
            pointer: (!cursor_apart).then(|| drawn.clone()),
            sdr_white,
            // The monitor changed under the stream: input and the cursor follow it.
            resized: Box::new(move |width, height| {
                let now = display::current(&device).map(|(_, a)| a).unwrap_or(Area { width, height, ..*shared.lock().unwrap() });
                *shared.lock().unwrap() = now;
            }),
        };
        let (pipeline, started) = match Pipeline::start(settings, link.clone()) {
            Ok(p) => p,
            Err(e) => {
                // HDR back while the monitor is still on the desktop.
                drop(hdr);
                self.screen.release();
                return Err(format!("{e:#}"));
            }
        };
        // The picture is what the driver's channel holds, whatever Windows said of the mode.
        {
            let mut a = area.lock().unwrap();
            (a.width, a.height) = (started.width, started.height);
        }
        let cursor = Cursor::start(cursor_apart.then(|| link.clone()), area.clone(), (!cursor_apart).then(|| drawn.clone())).inspect_err(|e| tracing::warn!("couldn't start following the cursor: {e}")).ok();
        if !cursor_apart {
            tracing::info!("this client doesn't draw the pointer: it is drawn into the picture");
        }
        self.live = Some(Live { pipeline, cursor, injector: Injector::new(area), link, screen, decoders, cursor_apart, common: modes, ceiling, name: name.clone(), files: Files::default(), clipboard: None, copied: BTreeMap::new(), mode: started.mode, second: None, target: 0, _hdr: hdr });
        // A new picture shares the clipboard anew.
        self.share_clipboard(self.clipboard_wanted);
        {
            let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let mut now = self.allowed.now.lock().unwrap();
            // A new picture for the same client keeps the time it came.
            let since = now.as_ref().filter(|n| n.name == name).map_or(since, |n| n.since);
            let mode = started.mode;
            let picture = format!("{} {}-bit{}{}", if mode.codec == Codec::Av1 { "AV1" } else { "HEVC" }, mode.depth, if mode.chroma == Chroma::C444 { " 4:4:4" } else { "" }, if started.hdr.is_some() { " HDR" } else { "" });
            // (Every placement but `mirror` is, for now, the one monitor at the device's size.)
            *now = Some(Now { name, since, width: started.width, height: started.height, refresh, placement: match (mirror, own.is_some(), place) {
                (true, ..) => "mirror",
                (_, true, Place::Beside) => "extend",
                (_, true, Place::Main) => "main",
                // (Without a monitor of its own the device has the one monitor, resized.)
                _ => "only",
            }, picture, sound: self.sound.is_some() });
        }
        if let Some((white, peak)) = started.hdr {
            tracing::info!(sdr_white = white, peak, "the picture is HDR: BT.2020 PQ");
        }
        Ok(started.accept(refresh))
    }

    /// Stop the picture, let go of the client's keys, and put the monitor back.
    fn stop(&mut self) -> Option<Live> {
        let mut l = self.live.take()?;
        // The second display goes with the first.
        if let Some((pipeline, mut injector, _)) = l.second.take() {
            injector.release_all();
            drop(pipeline);
            self.screen.take_second();
        }
        l.injector.release_all();
        l.cursor = None;
        if l.clipboard.take().is_some() {
            tracing::info!("the clipboard is no longer shared with the device");
        }
        Some(l)
    }

    /// Give the client a second display beside everything else, or take it away. It gets a
    /// monitor of its own (the driver's third) and a picture of its own in the codec of the
    /// first, with the pointer drawn into it, as the protocol asks.
    fn second_display(&mut self, on: bool, display: Option<Display>, side: remoteman_core::Side) {
        let Some(l) = self.live.as_mut() else { return };
        if let Some((pipeline, mut injector, _)) = l.second.take() {
            injector.release_all();
            drop(pipeline);
            self.screen.take_second();
            l.target = 0;
        }
        let (true, Some(screen)) = (on, display) else { return };
        let link = l.link.clone();
        let gone = |why: String| {
            tracing::warn!("no second display for the device: {why}");
            link.send_message(Message::Display { index: 1, on: false, display: None, side: Default::default(), message: why })
        };
        if screen.width % 2 != 0 || screen.height % 2 != 0 || screen.width < 64 || screen.height < 64 || screen.width > 8192 || screen.height > 8192 {
            return gone("display sizes must be even, from 64 to 8192".into());
        }
        let want = display::Mode { width: screen.width, height: screen.height, refresh: screen.refresh.clamp(1, 240) };
        let (mode, at, monitor) = match self.screen.give_second(want, side) {
            Ok(got) => got,
            Err(e) => return gone(format!("{e:#}")),
        };
        let refresh = mode.refresh.clamp(1, 240);
        let second = l.link.second();
        // Said before its first frame can be sent.
        second.restart(Accept { mode: l.mode, width: mode.width, height: mode.height, refresh, colour: "bt709".into(), sdr_white: 0, peak: 0 });
        let area = Arc::new(Mutex::new(at));
        let drawn = Arc::new(crate::cursor::Drawn::default());
        let settings = pipeline::Settings {
            driver: self.screen.driver(),
            monitor,
            modes: vec![l.mode],
            fps: refresh,
            bitrate: self.bitrate.min(40_000_000),
            nudge_at: (at.left, at.top),
            expect: Some((at.width, at.height)),
            held: false,
            status: false,
            hdr: false,
            // The protocol has no cursor channel for a second display: it is drawn in.
            pointer: Some(drawn.clone()),
            sdr_white: crate::hdr::USUAL_SDR_WHITE,
            resized: Box::new(|_, _| {}),
        };
        match Pipeline::start(settings, second) {
            Ok((pipeline, _)) => {
                let cursor = Cursor::start(None, area.clone(), Some(drawn)).inspect_err(|e| tracing::warn!("couldn't follow the cursor for the second display: {e}")).ok();
                l.second = Some((pipeline, Injector::new(area), cursor));
            }
            Err(e) => {
                self.screen.take_second();
                gone(format!("{e:#}"))
            }
        }
    }

    /// Share the clipboard with the client, both ways, or stop. Only where the computer's
    /// owner allowed it.
    fn share_clipboard(&mut self, on: bool) {
        let allowed = self.allowed.clipboard.load(Ordering::Relaxed);
        let Some(l) = self.live.as_mut() else { return };
        if l.clipboard.take().is_some() {
            tracing::info!("the clipboard is no longer shared with the device");
        }
        if on && !allowed {
            tracing::info!("the device asked to share the clipboard, which isn't allowed here");
        }
        if !on || !allowed {
            return;
        }
        match Clipboard::start(l.link.clone(), self.allowed.clipboard.clone(), self.copies.clone()) {
            Ok(clipboard) => {
                l.clipboard = Some(clipboard);
                tracing::info!("the clipboard is shared with the device");
            }
            Err(e) => tracing::warn!("couldn't share the clipboard: {e:#}"),
        }
    }

    /// A clipboard blob from the client: what was copied on the device becomes what the
    /// desktop pastes: text, or a picture as a PNG.
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
                let (Some((mime, bytes)), Some(clipboard), true) = (l.copied.remove(id), l.clipboard.as_ref(), *complete) else { return };
                let mime = if mime.starts_with("text/plain") {
                    crate::clipboard::TEXT
                } else if mime == crate::clipboard::PNG {
                    crate::clipboard::PNG
                } else {
                    return tracing::info!(kind = mime, "the device copied something this host doesn't take (text and PNG only)");
                };
                clipboard.offer(crate::clipboard::Copied { mime, bytes });
            }
        }
    }

    /// Send the desktop's sound to the client, or stop.
    fn send_sound(&mut self, on: bool) {
        self.sound = None;
        self.switched = None;
        let (Some(l), true) = (self.live.as_ref(), on) else { return };
        // The PC's own speakers go quiet while the device listens (if the owner allows it and
        // Steam's speakers exist); the sound is then recorded from those.
        if self.allowed.speakers_follow.load(Ordering::Relaxed) {
            self.switched = crate::speakers::Switched::engage();
        }
        l.link.send_message(Message::SoundFormat { codec: "opus".into(), rate: sound::SAMPLE_RATE, channels: 2, frame_us: sound::FRAME_US as u32 });
        match Speaker::start(l.link.clone()) {
            Ok(speaker) => self.sound = Some(speaker),
            Err(e) => tracing::warn!("couldn't send the desktop's sound: {e:#}"),
        }
    }

    /// Play the client's microphone into the virtual device, or stop.
    fn take_microphone(&mut self, on: bool) {
        if self.microphone.take().is_some() {
            let (packets, missing, _) = std::mem::replace(&mut self.mic_count, (0, 0, None));
            tracing::info!(packets, missing, "the device's microphone: packets that came and that were lost on the way");
        }
        if self.live.is_none() || !on {
            return;
        }
        match Microphone::start() {
            Ok(microphone) => self.microphone = Some(microphone),
            Err(e) => tracing::warn!("couldn't take the device's microphone: {e:#}"),
        }
    }

    /// The client changed what it wants: a new ceiling for the bitrate takes effect in the
    /// running picture; another display or other decoders start the picture afresh.
    fn configure(&mut self, display: Option<Display>, bitrate: Option<u32>, decoders: Option<Vec<Mode>>) {
        let own = self.bitrate;
        let Some(l) = self.live.as_mut() else { return };
        if let Some(b) = bitrate {
            l.ceiling = b;
            l.pipeline.set_bitrate(if b == 0 { own } else { b.clamp(*BITRATE.start(), *BITRATE.end()) });
        }
        let screen = display.unwrap_or(l.screen);
        let decoders = decoders.unwrap_or_else(|| l.decoders.clone());
        // Another picture only if the display differs, or the modes to choose from do.
        if screen == l.screen && Self::common(&decoders) == l.common {
            l.decoders = decoders;
            return;
        }
        tracing::info!(?screen, ?decoders, "the client asked for another picture");
        // The old picture goes first; then the monitor changes, the client is told, and the
        // new picture starts.
        let Some(old) = self.stop() else { return };
        let (link, cursor_apart, ceiling, name) = (old.link.clone(), old.cursor_apart, old.ceiling, old.name.clone());
        let (was_screen, was_decoders) = (old.screen, old.decoders.clone());
        drop(old);
        if let Err(e) = self.restart(name.clone(), screen, decoders, cursor_apart, ceiling, link.clone()) {
            // Back to what worked, rather than leave the client with nothing.
            tracing::warn!("couldn't start that picture ({e}); going back to the one before");
            self.screen.release();
            if let Err(e) = self.restart(name, was_screen, was_decoders, cursor_apart, ceiling, link) {
                tracing::error!("couldn't restore the picture: {e}");
            }
        }
    }

    /// Start a picture for a client that already has one. The core numbers frames from 0 again
    /// when the client is told of the new picture, and what the picture is (its real size) is
    /// only known once it has started; so it starts held back, the client is told, and then
    /// it is let go.
    fn restart(&mut self, name: String, screen: Display, decoders: Vec<Mode>, cursor_apart: bool, ceiling: u32, link: Link) -> Result<(), String> {
        let accept = self.start(name, screen, decoders, cursor_apart, ceiling, link.clone(), true)?;
        link.restart(accept);
        if let Some(l) = &self.live {
            l.pipeline.release();
        }
        Ok(())
    }
}

impl HostHandler for Host {
    fn connected(&mut self, client: &ClientInfo, link: Link) -> Result<Accept, String> {
        self.end(false);
        if self.allowed.paused.load(Ordering::Relaxed) {
            tracing::info!(name = %client.name, "a device came while sharing is off: turned away");
            return Err("sharing is switched off on this computer".into());
        }
        self.unlock_for(client)?;
        let screen = client.display;
        tracing::info!(name = %client.name, ?screen, decoders = ?client.decoders, cursor_apart = client.cursor, "client");
        let accepted = self.start(client.name.clone(), screen, client.decoders.clone(), client.cursor, 0, link, false).inspect_err(|e| tracing::error!("couldn't start the picture: {e}"))?;
        self.awake = crate::awake::Awake::new();
        Ok(accepted)
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
            // The device's word on a file this computer sent it.
            Message::FileResult { ok, message, .. } => {
                tracing::info!(ok, said = message, "the device answered for a file sent to it");
                let mut sent = self.allowed.sent.lock().unwrap();
                *sent = (sent.0 + 1, *ok, message.clone());
            }
            Message::Display { index: 1, on, display, side, .. } => self.second_display(*on, *display, *side),
            Message::DisplayKeyframe { index: 1 } => {
                if let Some((pipeline, ..)) = self.live.as_ref().and_then(|l| l.second.as_ref()) {
                    pipeline.request_keyframe();
                }
            }
            _ => {}
        }
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
            microphone.push(&packet.data);
        }
    }

    fn blob(&mut self, event: &BlobEvent) {
        // A blob of a kind that isn't taken never gets an entry, so its content and its end
        // fall through.
        let (is_file, is_copy) = match event {
            BlobEvent::Open { kind, .. } => (*kind == BLOB_FILE, *kind == BLOB_CLIPBOARD),
            other => self.live.as_ref().map_or((false, false), |l| (l.files.has(other.id()), l.copied.contains_key(&other.id()))),
        };
        if is_file {
            let allowed = self.allowed.files.load(Ordering::Relaxed);
            if let Some(l) = self.live.as_mut() {
                l.files.event(event, allowed, &l.link);
            }
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
        let Some(l) = self.live.as_mut() else { return };
        // Which of its displays the pointer is on from here on.
        if let Event::Display(index) = event {
            l.target = index;
            return;
        }
        // The pointer and touches go to the display they are on; keys to the desktop.
        let pointing = matches!(event, Event::PointerTo { .. } | Event::PointerBy { .. } | Event::Button { .. } | Event::Scroll { .. } | Event::Touch { .. });
        match &mut l.second {
            Some((_, injector, _)) if l.target == 1 && pointing => injector.inject(&event),
            _ => l.injector.inject(&event),
        }
    }

    fn left(&mut self) {
        self.end(true);
    }
}

impl Host {
    /// The session ends. `last`: no other device is about to take its place (then the setting
    /// "lock when a device disconnects" applies).
    fn end(&mut self, last: bool) {
        *self.allowed.now.lock().unwrap() = None;
        self.clipboard_wanted = false;
        self.take_microphone(false);
        self.sound = None;
        // The speakers back as they were, after the sound has stopped recording from Steam's.
        self.switched = None;
        self.awake = None;
        let was_up = self.live.is_some();
        if let Some(l) = self.stop() {
            drop(l);
            self.screen.release();
            tracing::info!("the client is gone");
        }
        // Unlocked for that device: locked again now that it has gone.
        if std::mem::take(&mut self.unlocked_for_client) {
            let locked = crate::desk::lock_now();
            tracing::info!(locked, "the device that unlocked this computer has gone: locked again");
        } else if last && was_up && self.allowed.lock_on_leave.load(Ordering::Relaxed) && !crate::desk::locked() {
            let locked = crate::desk::lock_now();
            tracing::info!(locked, "the device has gone: this computer is locked (the owner's setting)");
        }
    }
}
