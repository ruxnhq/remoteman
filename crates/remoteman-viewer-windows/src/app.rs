//! The app: what is on the window now, what each press does to that, and connecting.
//!
//! The window (`window.rs`) hands every event here, to the main thread's one `App`. A press on
//! a control arrives as the [`Msg`] the control was described with, and [`App::update`] changes
//! the state; after that the whole window is described again (`pages.rs` and `session.rs`
//! do the describing) and handed to the draw thread. What only the window itself can do
//! (minimise, full screen, close) goes back to it as an [`Effect`].

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use remoteman_core::{Client, Display, Event, Message, Mode, Options, Placement, keys};

use crate::args::Args;
use crate::features::FEATURES;
use crate::gpu::Gpu;
use crate::status::{self, Problem, Reach};
use crate::store::{PLACEMENT_NAMES, Quality, SIZES, Store, WINDOW, fit_lines, host_part, short, with_port};
use crate::stream::{Scene, Shared, unpack};
use crate::ui::{self, Field, Fonts, Frame, Id, Input, Kind, Rect, Region, Ui, id, theme};

/// A computer to connect to.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    /// `host:port`.
    pub address: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Screen {
    Computers,
    Connecting,
    Problem(Target, Problem),
    Session,
    Settings,
    /// The settings of the computer at this address.
    Computer(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    /// `waiting`: who is being waited for; `problem`: what came of the last try.
    Pair { waiting: Option<String>, problem: Option<String> },
    /// The computer is asking its owner about this PC: both sides show this code, and its
    /// owner confirms there before `until`.
    Code { name: String, code: String, until: Instant },
    Paired { target: Target, key: String },
    /// The computer asks for a code from an authenticator app (`left`: tries left, after a wrong
    /// one). Answered with `DialogOk`; `remember` (the locked page's flag) is "don't ask again
    /// on this device for 30 days".
    SecondFactor { name: String, left: Option<u32> },
    Forget(String),
    Address { address: String, problem: Option<String> },
    About,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MenuKind {
    /// The "more" of one computer's card.
    Card(String),
    Keys,
    Display,
    Picture,
    /// The clipboard and files.
    Share,
    More,
    Choice(Choice),
}

/// The lists behind the drop-downs.
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    Controls,
    DefaultQuality,
    Placement(String),
    Quality(String),
    Bitrate(String),
    /// How large a picture to ask a computer for.
    Size(String),
    Scroll,
    /// The key that acts as the Windows key (Super, Command) besides the Windows key itself.
    WindowsKey,
    /// How a computer with more than one way in is entered.
    WayIn(String),
    /// Which side of the computer's displays a second display goes.
    Side(String),
    /// The pointer follows this one, or is captured.
    Pointer,
}

/// The switches of the settings pages.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Flag {
    MatchRefresh,
    CtrlAsCommand,
    Shortcuts,
    Sound,
    Clipboard,
    /// HDR where this monitor is in HDR, and variable refresh in full screen (the engine's).
    Hdr,
    VariableRefresh,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub kind: MenuKind,
    /// The control it hangs from.
    pub anchor: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Minimise,
    Maximise,
    Close,
    Back,
    OpenMenu(MenuKind, Rect),
    CloseMenu,
    /// Open the pairing dialog, with this address in it.
    Pair(Option<String>),
    /// The dialog's first button, and its other one.
    DialogOk,
    DialogCancel,
    Connect(String),
    CancelConnect,
    OpenSettings,
    OpenComputer(String),
    EditAddress(String),
    AskForget(String),
    About,
    Disconnect,
    /// Press these keys (Linux key codes) on the other computer, in order, and let them go.
    SendKeys(&'static [u16]),
    Place(Placement),
    Pick(Quality),
    /// A slider was moved to this part of its length.
    Slide(Id, f32),
    ToggleFigures,
    ToggleFullscreen,
    /// The session's bar: `true` for one that only shows at the top edge.
    BarMode(bool),
    StartFullscreen,
    Choose(Choice, usize),
    /// A switch in Settings, or (with an address) in one computer's settings.
    Toggle(Flag, Option<String>),
    ToggleSound,
    ToggleClipboard,
    /// Add a second display in a window of its own, or take it away.
    ToggleSecond,
    /// Choose a file to send to the computer.
    SendFile,
    /// Try the locked computer with the password that was typed.
    Unlock,
    /// Whether the password typed is to be kept on this PC if it works.
    ToggleRemember,
    /// Choose where files from a computer go.
    PickFolder,
    /// Lend the computer this PC's microphone or camera, or stop (`media.rs`).
    ToggleMicrophone,
    ToggleCamera,
    /// Flip a switch `App::set_option` knows (with an address: one computer's).
    Set(&'static str, Option<String>),
    /// Send this computer the waking packet, then connect (`nearby.rs`).
    Wake(String),
}

/// What only the window can do.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Minimise,
    Maximise,
    Close,
    Fullscreen(bool),
    /// The pointer reached the top edge of full screen: the app brings out its bar.
    BarEdge,
    Title(String),
    /// Let go of every key the other computer was told is down.
    ReleaseKeys,
    /// The host's cursor is another session's now.
    ForgetCursor,
    /// Make the window's client area this large, in the design's pixels.
    Size(f32, f32),
    /// Ask which file to send; the answer comes to `App::send_file`.
    PickFile,
    /// Put this text on this PC's clipboard.
    SetClipboard(String),
    /// Put this picture (a PNG) on this PC's clipboard.
    SetClipboardPicture(Vec<u8>),
    /// Read this PC's clipboard; text on it comes to `App::copied_here`.
    ReadClipboard,
    /// Ask for a folder; the answer comes to `App::set_folder`.
    PickFolder,
    /// Open the second display's window (with this title) on the session's second stream,
    /// or close it.
    OpenSecond(String),
    CloseSecond,
    Quit,
}

/// What the pointer should look like.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pointer {
    Arrow,
    Text,
    /// The other computer's.
    Theirs,
}

/// What the window's title bar is at a point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Client,
    Caption,
    Maximise,
}

pub struct Session {
    /// `None` for a made-up session, for pictures of the window.
    pub client: Option<Arc<Client>>,
    pub shared: Arc<Shared>,
    pub target: Target,
    pub features: Vec<String>,
    pub display: Display,
    pub quality: Quality,
    /// The most the picture may use, in megabits a second (0: the computer's choice).
    pub bitrate: u32,
    pub began: Instant,
    /// What the computer runs, for the keys that differ.
    pub os: String,
    /// The computer's sound is played here; the clipboard is shared.
    pub sound_on: bool,
    pub clipboard_on: bool,
    /// There is a second display, in a window of its own.
    pub second_on: bool,
    /// The computer is lent this PC's microphone, and its camera (`media.rs`).
    pub mic_on: bool,
    pub camera_on: bool,
    /// The text that last came from the computer's clipboard: not to be sent back. And the
    /// text last sent: Windows says "the clipboard changed" several times for one copy.
    from_computer: Option<String>,
    sent_copy: Option<String>,
    /// The display size last asked for, and the size the window's picture had a second ago.
    asked: Option<(u32, u32)>,
    wanted: (u32, u32),
}

impl Session {
    #[allow(clippy::too_many_arguments)]
    pub fn new(client: Option<Arc<Client>>, shared: Arc<Shared>, target: Target, features: Vec<String>, display: Display, quality: Quality, bitrate: u32, os: String) -> Self {
        Self { client, shared, target, features, display, quality, bitrate, began: Instant::now(), os, sound_on: false, clipboard_on: false, second_on: false, mic_on: false, camera_on: false, from_computer: None, sent_copy: None, asked: None, wanted: (0, 0) }
    }

    pub fn offers(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }

    /// The computer left `input` out of what it offers this device: the owner allows it to see
    /// the screen and nothing more. Nothing is sent to control it then (a made-up or played
    /// session has no computer and is never view-only).
    pub fn view_only(&self) -> bool {
        self.client.is_some() && !self.offers("input")
    }
}

/// A password on its way to a locked computer.
struct Unlocking {
    /// The computer's key fingerprint: what a kept password is filed under.
    computer: String,
    password: String,
    /// Keep it on this PC if it gets this PC in.
    remember: bool,
    /// It came from what is kept (so it isn't kept again, and is thrown away if wrong).
    kept: bool,
}

/// An attempt to connect that hasn't answered yet.
pub struct Attempt {
    pub target: Target,
    pub shared: Arc<Shared>,
    pub display: Display,
    /// For the pairing dialog, which shows the wait itself.
    pub pairing: bool,
}

/// The numbers over the picture, as they were a moment ago.
#[derive(Clone, Debug, Default)]
pub struct Figures {
    pub shown: u32,
    pub decoded: u32,
    pub decode_ms: f64,
    pub age_ms: f64,
    pub round_trip_ms: f64,
    pub megabits: f64,
    pub lost: u32,
    pub replaced: u32,
    pub decoder: String,
}

/// What a session came to, for the line printed at the end.
pub struct Summary {
    pub seconds: f64,
    pub decoded: u32,
    pub shown: u32,
    pub replaced: u32,
    pub lost: u32,
    pub keyframe_requests: u32,
    pub round_trip_ms: f64,
    pub decoder: String,
    pub ended: Option<String>,
}

/// The steps of the bitrate limit, in megabits a second; the last is no limit.
pub const BITRATES: [u32; 15] = [5, 10, 15, 20, 25, 30, 40, 50, 60, 80, 100, 150, 300, 500, 0];
/// The ones the drop-down of a computer's settings offers.
/// The same steps as the Linux viewer's "Bitrate, at most".
pub const BITRATE_CHOICES: [u32; 7] = [0, 20, 50, 100, 150, 300, 500];

pub struct App {
    pub args: Args,
    pub gpu: Gpu,
    pub scene: Arc<Scene>,
    /// What this card decodes of what the command line allows, most preferred first.
    pub decoders: Vec<Mode>,
    pub fonts: Fonts,
    pub store: Store,
    pub dark: bool,
    /// The window's backdrop is Windows' Mica, which shows wherever nothing is drawn: the
    /// pages then have no fill of their own.
    pub mica: bool,
    /// The window's client area in the design's pixels, and device pixels to one of those.
    pub size: (f32, f32),
    pub scale: f32,
    pub maximised: bool,
    pub fullscreen: bool,
    pub screen: Screen,
    pub dialog: Option<Dialog>,
    pub menu: Option<Menu>,
    pub fields: HashMap<Id, Field>,
    pub scroll: f32,
    /// How far the page can be scrolled, found while describing it.
    pub scroll_room: Cell<f32>,
    pub input: Input,
    pub regions: Vec<Region>,
    pub mouse: (f32, f32),
    hot_since: Instant,
    pub tip: Option<(String, Rect)>,
    pub session: Option<Session>,
    pub attempt: Option<Attempt>,
    attempt_id: u32,
    results: (mpsc::Sender<(u32, Result<Client, String>)>, mpsc::Receiver<(u32, Result<Client, String>)>),
    pub reach: HashMap<String, Reach>,
    /// The pointer is at the top edge or on the bar, so a hidden bar is out; when it is to go
    /// in again; and how far out it is drawn (0 to 1).
    pub bar_out: bool,
    bar_until: Option<Instant>,
    /// Full screen's bar and menus are the app's WinUI ones (in a window of their own at the top
    /// edge) rather than the kit's: the kit draws none of them, and the pointer reaching the top edge
    /// is told to the app (`Effect::BarEdge`). `edge`: it is at the top edge now.
    pub native_bar: bool,
    edge: bool,
    pub bar_slide: f32,
    pub figures: Figures,
    last_second: Instant,
    pub effects: Vec<Effect>,
    pub dirty: bool,
    serial: u64,
    started: Instant,
    blink: Instant,
    /// Nothing moves by itself: for pictures of the window.
    pub still: bool,
    /// The monitor's size in pixels and its refresh rate.
    pub monitor: (u32, u32, u32),
    /// The size the window had before a session made it larger.
    front_size: Option<(f32, f32)>,
    /// Go back to full screen when the session that is about to start has started.
    refull: bool,
    pub summary: Option<Summary>,
    /// Set when a session named on the command line with `--seconds` is over, whichever way.
    pub code: Option<i32>,
    /// The password to send with the next try, for a computer that said it is locked. It is
    /// kept nowhere else and never written anywhere.
    unlock: Option<String>,
    /// How many times in a row a computer has said it is logging its user in.
    logging_in: u32,
    /// When to try the computer on the problem page again by itself.
    pub retry_at: Option<Instant>,
    /// The window is up and the computer the command line names has been tried (`start`).
    started_on_host: bool,
    /// The stream's size and rate as the bar last said them.
    shown_format: Option<(u32, u32, u32)>,
    /// The password being tried on a locked computer, until it is known whether it worked.
    unlocking: Option<Unlocking>,
    /// "Remember it on this PC" is switched on, on the locked page.
    pub remember: bool,
    /// The pointer is held in the window (the window says so) and the computer's own is drawn
    /// over the picture where the computer says it is; with its image.
    pub captured: bool,
    pub remote_cursor: Option<Arc<remoteman_core::CursorShape>>,
    /// A line of news over the window, until it has been read.
    pub toast: Option<(String, Instant)>,
    /// The picture has a place of its own (the WinUI 3 app's panel, `engine.rs`) and the
    /// size the window is said to have is that place's: the bar is not in it.
    pub panel: bool,
    /// The computers seen on this network (`nearby.rs`).
    pub sightings: crate::nearby::Sightings,
}

fn mode_word(mode: &Mode) -> &'static str {
    match mode.codec {
        remoteman_core::Codec::Av1 => "av1",
        remoteman_core::Codec::Hevc => "hevc",
    }
}

impl App {
    pub fn new(args: Args, gpu: Gpu, scene: Arc<Scene>, decoders: Vec<Mode>, dark: bool) -> anyhow::Result<Self> {
        let store = Store::load(&args.state);
        let now = Instant::now();
        // `remoteman connect` never shows the list, not even before its window is up.
        let screen = if args.given.is_some() { Screen::Connecting } else { Screen::Computers };
        Ok(Self {
            fonts: Fonts::new()?,
            store,
            dark,
            mica: false,
            size: (984.0, 644.0),
            scale: 1.0,
            maximised: false,
            fullscreen: false,
            screen,
            dialog: None,
            menu: None,
            fields: HashMap::new(),
            scroll: 0.0,
            scroll_room: Cell::new(0.0),
            input: Input { caret: true, ..Input::default() },
            regions: Vec::new(),
            mouse: (-1.0, -1.0),
            hot_since: now,
            tip: None,
            session: None,
            attempt: None,
            attempt_id: 0,
            results: mpsc::channel(),
            reach: HashMap::new(),
            bar_out: false,
            bar_until: None,
            native_bar: false,
            edge: false,
            bar_slide: 0.0,
            figures: Figures::default(),
            last_second: now,
            effects: Vec::new(),
            dirty: true,
            serial: 0,
            started: now,
            blink: now,
            still: args.ui.is_some(),
            monitor: (1920, 1080, 60),
            front_size: None,
            refull: false,
            summary: None,
            code: None,
            unlock: None,
            logging_in: 0,
            retry_at: None,
            started_on_host: false,
            unlocking: None,
            remember: false,
            captured: false,
            remote_cursor: None,
            toast: None,
            panel: false,
            shown_format: None,
            // Not for pictures of the window: what is really nearby isn't to be in them.
            sightings: crate::nearby::Sightings::start(args.ui.is_none().then(|| scene.clone()), args.browse.as_deref()),
            args,
            gpu,
            scene,
            decoders,
        })
    }

    pub fn theme(&self) -> ui::Theme {
        match (self.mica, self.dark) {
            (true, true) => theme::DARK_ON_MICA,
            (true, false) => theme::LIGHT_ON_MICA,
            (false, true) => theme::DARK,
            (false, false) => theme::LIGHT,
        }
    }

    /// Keys and the pointer go to the other computer: its picture is what the window shows,
    /// with nothing of this app's own open over it.
    pub fn forwarding(&self) -> bool {
        self.screen == Screen::Session && self.session.is_some() && self.dialog.is_none() && self.menu.is_none()
    }

    pub fn client(&self) -> Option<&Arc<Client>> {
        self.session.as_ref().and_then(|s| s.client.as_ref())
    }

    /// The client to send input to: none when the computer allows this device to view only.
    pub fn input_client(&self) -> Option<&Arc<Client>> {
        if self.session.as_ref().is_some_and(|s| s.view_only()) { None } else { self.client() }
    }

    /// The session is view only (see [`Session::view_only`]).
    pub fn view_only(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.view_only())
    }

    /// The session's bar is not part of the window but comes out over the picture.
    pub fn bar_hidden(&self) -> bool {
        self.fullscreen || self.store.settings.hover_bar
    }

    /// Where the screen behind the settings pages is.
    fn home(&self) -> Screen {
        if self.session.is_some() { Screen::Session } else { Screen::Computers }
    }

    pub fn name_of(&self, address: &str) -> String {
        self.store.find(address).map_or_else(|| host_part(address).to_owned(), |c| c.name.clone())
    }

    pub fn target_of(&self, address: &str) -> Target {
        Target { address: address.to_owned(), name: self.name_of(address) }
    }

    /// What is known of a computer before trying it, as far as this build may say.
    pub fn reach_of(&self, address: &str) -> Option<Reach> {
        // Seen on this network now (DNS-SD): it is there, whatever the last try found.
        if FEATURES.presence && self.seen(address).is_some() {
            return Some(Reach::Online);
        }
        self.reach.get(address).copied().filter(|r| FEATURES.presence || *r == Reach::Unreachable)
    }

    /// A placement was picked for this computer: it wins from now on over one the command line
    /// gave.
    fn drop_given_placement(&mut self, address: &str) {
        if let Some(given) = self.args.given.as_mut().filter(|g| g.address == address) {
            given.placement = None;
        }
    }

    /// The computer's key fingerprint: the one `remoteman connect` gave for it, or the one kept
    /// from pairing. What a kept password is filed under.
    fn key_of(&self, address: &str) -> Option<String> {
        self.args.given_for(address).map(|g| g.expect.clone()).or_else(|| self.store.key(address))
    }

    /// How this display is to sit among a computer's own, which stream to ask it for, and the
    /// most that may use.
    pub fn choices(&self, address: &str) -> (Placement, Quality, u32) {
        let placement = placement(&self.args, &self.store, address);
        match self.store.find(address) {
            Some(c) => (placement, c.quality.unwrap_or(self.store.settings.quality), c.bitrate),
            None => (placement, self.store.settings.quality, 0),
        }
    }

    /// How many pixels the picture has to itself in the window: the window without its bar,
    /// or all of it where the bar floats over the picture.
    fn picture_pixels(&self) -> (u32, u32) {
        let (w, h) = match self.video_pixels() {
            // Connected: the picture's place as it is laid out.
            Some(area) => (area.2 as f32, area.3 as f32),
            // Not yet: the size the window is about to have (a small one is made room in when
            // the session starts, see `enter`), less its bar if that stays.
            None if self.fullscreen || self.store.settings.fullscreen || self.args.fullscreen => (self.monitor.0 as f32, self.monitor.1 as f32),
            None => {
                let (mut w, mut h) = self.size;
                if !self.maximised && (w < 1224.0 || h < 744.0) {
                    (w, h) = (1224.0, 744.0);
                }
                if !self.store.settings.hover_bar && !self.panel {
                    h -= crate::session::BAR;
                }
                (w * self.scale, h * self.scale)
            }
        };
        let pixels = |v: f32| ((v.round().max(1.0) as u32) & !1).clamp(320, 8192);
        (pixels(w), pixels(h))
    }

    /// The display to ask the computer at `address` for: this monitor's, or fewer pixels if
    /// that was chosen for it, or the window's own; unless the command line says otherwise.
    pub fn display_for(&self, address: Option<&str>, placement: Placement) -> Display {
        let (mut w, mut h, mut scale) = (self.monitor.0, self.monitor.1, self.scale);
        if let Some(size) = self.args.size {
            // A size from the command line is a test's: pixels as they are.
            (w, h, scale) = (size.0, size.1, 1.0);
        }
        // (A computer not paired yet gets what a new one is given: the window's size.)
        let most = address.and_then(|a| self.store.find(a)).map_or(WINDOW, |c| c.max_height);
        let (mut w, mut h, scale) = fit_lines(w, h, scale, most);
        // (A mirrored display is the computer's own size whatever is asked.)
        if most == WINDOW && self.args.size.is_none() && placement != Placement::Mirror {
            (w, h) = self.picture_pixels();
        }
        let refresh = if self.store.settings.match_refresh { self.monitor.2.clamp(30, 240) } else { 60 };
        // HDR where this screen shows it (or a test says so) and the switch is on; the host
        // then may send PQ.
        let hdr = FEATURES.hdr && self.store.settings.hdr && self.scene.screen_hdr.load(Ordering::Relaxed);
        let headroom = if hdr { f32::from_bits(self.scene.headroom.load(Ordering::Relaxed)) } else { 0.0 };
        Display { width: w & !1, height: h & !1, scale, refresh: self.args.refresh.unwrap_or(refresh), placement, hdr, headroom }
    }

    /// While connected with "The window's size": when the window has changed size and been
    /// still for a second, the computer is asked for a display of the new size. Looked at once
    /// a second. Compared with what was last asked for, never with what came: a computer may
    /// answer with another size, and that must not be asked for again and again.
    fn follow_window(&mut self) {
        let Some(s) = &self.session else { return };
        let (address, placement) = (s.target.address.clone(), s.display.placement);
        let most = self.store.find(&address).map_or(0, |c| c.max_height);
        // (Not with a second display on: a display made afresh takes the second away.)
        if most != WINDOW || self.args.size.is_some() || placement == Placement::Mirror || !s.offers("configure") || self.screen != Screen::Session || s.second_on {
            return;
        }
        let display = self.display_for(Some(&address), placement);
        let now = (display.width, display.height);
        let s = self.session.as_mut().unwrap();
        let Some(asked) = s.asked else { return s.asked = Some(now) };
        // Still for a second: the same on two looks running.
        if now != asked && std::mem::replace(&mut s.wanted, now) == now {
            s.asked = Some(now);
            if let Some(client) = &s.client {
                say!("display: asked for {} x {}, the window's new size", now.0, now.1);
                client.send_message(Message::Configure { display: Some(display), bitrate: None, decoders: None });
            }
        }
    }

    /// HDR is asked for while the switch is on and this monitor is in HDR. When that changes in
    /// a session (the switch, Windows' own HDR switch, the window taken to another monitor),
    /// the computer is asked again with `configure`, and its picture starts afresh. Not while
    /// there is a second display: a display made afresh takes it away; the change waits until
    /// it is gone (this is looked at once a second).
    pub fn follow_hdr(&mut self) {
        let Some(s) = self.session.as_ref().filter(|s| !s.second_on) else { return };
        let (address, placement) = (s.target.address.clone(), s.display.placement);
        let fresh = self.display_for(Some(&address), placement);
        let s = self.session.as_mut().unwrap();
        if fresh.hdr == s.display.hdr {
            return;
        }
        (s.display.hdr, s.display.headroom) = (fresh.hdr, fresh.headroom);
        if !s.offers("hdr") {
            // It sends SDR either way.
            return;
        }
        let Some(client) = s.client.as_ref().filter(|_| s.offers("configure")) else {
            say!("HDR {}: from the next session (this computer can't change its picture during one)", if fresh.hdr { "on" } else { "off" });
            return;
        };
        let mut display = s.display;
        if let Some((w, h)) = s.asked {
            (display.width, display.height) = (w, h);
        }
        say!("HDR {} asked for (headroom {:.1})", if display.hdr { "on" } else { "off" }, display.headroom);
        client.send_message(Message::Configure { display: Some(display), bitrate: None, decoders: None });
    }

    /// How fast this PC's processor is taken to decode 4:4:4: as measured, or the guess.
    pub fn cpu_444_rate(&self) -> (u32, bool) {
        match self.store.settings.cpu_444 {
            0 => (crate::store::CPU_444_GUESS, false),
            measured => (measured, true),
        }
    }

    /// Why 4:4:4 is left out of what is offered for `display` with `quality`, if it is: it is
    /// decoded on the processor here (this FFmpeg has no D3D11VA path for it), and only offered
    /// unasked where the processor keeps up. "Sharp text" asks for it whatever the speed.
    pub fn withholds_444(&self, display: &Display, quality: Quality) -> Option<String> {
        let mode = self.decoders.iter().find(|m| m.chroma == remoteman_core::Chroma::C444)?;
        if quality == Quality::Sharp || !crate::decode::on_processor(*mode) {
            return None;
        }
        let (rate, measured) = self.cpu_444_rate();
        if crate::store::cpu_keeps_up(display.width, display.height, display.refresh, rate) {
            return None;
        }
        Some(format!(
            "decoded on the processor here, which manages about {rate} million pixels a second ({}); {} x {} at {} Hz is {:.0}",
            if measured { "measured" } else { "a guess until measured" },
            display.width,
            display.height,
            display.refresh,
            display.width as f64 * display.height as f64 * display.refresh as f64 / 1e6
        ))
    }

    /// The usage to send for a key on this keyboard.
    pub fn key_for(&self, usage: u16) -> u16 {
        self.session.as_ref().map_or(usage, |s| self.store.settings.map_key(usage, &s.os))
    }

    pub fn say(&mut self, text: &str) {
        say!("said: {text}");
        self.toast = Some((text.to_owned(), Instant::now()));
        self.dirty = true;
    }

    // What the window tells the app.

    /// The window is up: connect to the computer named on the command line, if one was.
    pub fn start(&mut self) {
        if let Some(host) = self.args.host.clone() {
            let target = self.target_of(&with_port(&host));
            self.connect(target, self.args.pair);
            self.started_on_host = true;
        }
    }

    /// `remoteman connect` shows its one computer and never the list: where another run comes
    /// back to the list (Disconnect, Back from a problem page, Cancel while connecting), it ends.
    /// A computer that went away or can't be reached keeps its page, with Try again, until the
    /// window is closed or Back is pressed. Looked at each time the state is published.
    pub fn end_connect_run(&mut self) {
        if connect_run_over(self.args.given.is_some(), self.started_on_host, &self.screen) && self.code.is_none() {
            say!("connect: back at the list of computers, which this run doesn't show: leaving");
            self.dialog = None;
            self.menu = None;
            self.quit(0);
        }
    }

    /// The window's client area, in the design's pixels, and what one of those is in real ones.
    pub fn resized(&mut self, w: f32, h: f32, scale: f32, maximised: bool) {
        if (self.size, self.scale, self.maximised) != ((w, h), scale, maximised) {
            (self.size, self.scale, self.maximised) = ((w, h), scale, maximised);
            self.scroll = self.scroll.min(self.scroll_room.get());
            self.dirty = true;
        }
    }

    pub fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on;
        self.bar_out = false;
        self.bar_slide = 0.0;
        self.dirty = true;
    }

    pub fn set_dark(&mut self, dark: bool) {
        if self.dark != dark {
            self.dark = dark;
            self.dirty = true;
        }
    }

    /// A place in the client area (in pixels) as fractions of the other computer's display,
    /// allowing for the bar and for the bands that keep the picture's shape. `None` before the
    /// stream's size is known.
    pub fn fraction(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let s = self.session.as_ref()?;
        let (vw, vh) = unpack(s.shared.stream_size.load(Ordering::Relaxed));
        let (ax, ay, aw, ah) = self.video_pixels()?;
        if vw == 0 || vh == 0 || aw <= 0 || ah <= 0 {
            return None;
        }
        let (vw, vh, aw, ah) = (vw as f32, vh as f32, aw as f32, ah as f32);
        let fit = (aw / vw).min(ah / vh);
        let (dw, dh) = (vw * fit, vh * fit);
        // As the draw thread places it.
        let (ox, oy) = ((ax as f32 + (aw - dw) / 2.0).floor(), (ay as f32 + (ah - dh) / 2.0).floor());
        Some((((x - ox) / dw).clamp(0.0, 1.0), ((y - oy) / dh).clamp(0.0, 1.0)))
    }

    /// `--seconds` are up.
    pub fn time_up(&mut self) {
        if self.session.is_some() {
            self.stop(None);
        } else {
            self.give_up();
        }
        if self.code.is_none() {
            self.quit(1);
        }
    }

    pub fn quit(&mut self, code: i32) {
        self.code = Some(code);
        self.effects.push(Effect::Quit);
    }

    // Connecting.

    /// Try to connect, off this thread. The answer comes to `poll`.
    fn start_attempt(&mut self, target: Target, pair: bool, pairing: bool) {
        self.give_up();
        let (placement, quality, _) = self.choices(&target.address);
        let display = self.display_for(Some(&target.address), placement);
        let mut decoders = quality.order(&self.decoders);
        if let Some(why) = self.withholds_444(&display, quality) {
            say!("4:4:4 not offered: {why}");
            decoders.retain(|m| m.chroma != remoteman_core::Chroma::C444);
        }
        let (shared, feed) = match crate::stream::start(&self.gpu, &self.scene) {
            Ok(started) => started,
            Err(e) => return self.failed(&target, &format!("{e:#}"), pairing),
        };
        let options = Options {
            host: target.address.clone(),
            state: self.args.state.clone(),
            name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into()),
            pair,
            display,
            decoders,
            test_drop_one_in: None,
            test_delay_lost_ms: None,
            cursor: true,
            // Used once: a password that didn't work is typed again.
            unlock: self.unlock.take(),
            // "Getting in": only a session already running, never a login screen.
            attach_only: self.store.find(&target.address).is_some_and(|c| c.attach_only),
        };
        *shared.folder.lock().unwrap() = self.store.receive_folder();
        // `remoteman connect`: its keys for the computer it names, every time it is tried.
        let given = self.args.given_for(&target.address).map(crate::args::Given::core).unwrap_or_default();
        say!(
            "connecting to {} for {}x{} at {} Hz{}{}",
            target.address,
            display.width,
            display.height,
            display.refresh,
            if display.hdr { format!(", HDR (headroom {:.1})", display.headroom) } else { String::new() },
            if given.expect.is_some() { ", with the keys given" } else { "" }
        );
        let (id, tx, scene) = (self.attempt_id, self.results.0.clone(), self.scene.clone());
        let _ = std::thread::Builder::new().name("connect".into()).spawn(move || {
            let result = Client::connect_given(options, given, feed).map_err(|e| format!("{e:#}"));
            let _ = tx.send((id, result));
            scene.wake_window();
        });
        self.attempt = Some(Attempt { target, shared, display, pairing });
        self.dirty = true;
    }

    /// Stop waiting for the attempt under way; its answer will be thrown away.
    fn give_up(&mut self) {
        self.attempt_id = self.attempt_id.wrapping_add(1);
        self.attempt = None;
    }

    /// Connect to a computer: the connecting page, then its session or what went wrong.
    pub fn connect(&mut self, target: Target, pair: bool) {
        if self.session.is_some() {
            return;
        }
        self.dialog = None;
        self.menu = None;
        if self.reach_of(&target.address) == Some(Reach::Asleep) {
            // Known to be asleep: offer to wake it rather than wait for no answer.
            self.screen = Screen::Problem(target, Problem::Asleep);
            return;
        }
        self.start_attempt(target, pair, false);
        if self.attempt.is_some() {
            self.screen = Screen::Connecting;
        }
    }

    /// Look at what other threads have left: answers to attempts, and the session's end.
    pub fn poll(&mut self) {
        while let Ok((id, result)) = self.results.1.try_recv() {
            let Some(attempt) = self.attempt.take().filter(|_| id == self.attempt_id) else {
                // Nobody is waiting for this one any more.
                if let Ok(client) = result {
                    retire(Arc::new(client));
                }
                continue;
            };
            match result {
                Ok(client) => {
                    say!("host: {} ({}), features {:?}", client.host_name, client.host_os, client.host_features);
                    if attempt.pairing {
                        self.store.paired(&attempt.target.address, &client.host_name, &client.host_os);
                        let target = self.target_of(&attempt.target.address);
                        let key = crate::store::key_words(&client.host_fingerprint, 4);
                        // The connection that paired them is kept, so Connect shows it at once.
                        self.hold(client, attempt.shared, target.clone(), attempt.display);
                        self.dialog = Some(Dialog::Paired { target, key });
                    } else {
                        self.hold(client, attempt.shared, attempt.target, attempt.display);
                        self.enter();
                    }
                }
                Err(e) => self.failed(&attempt.target, &e, attempt.pairing),
            }
            self.dirty = true;
        }
        // The computer is asking its owner about this PC: show the code to compare.
        let asking = self.attempt.as_ref().filter(|a| a.pairing && FEATURES.pairing_code).and_then(|a| a.shared.pairing.lock().unwrap().take().map(|p| (a.target.clone(), p)));
        if let Some((target, (code, seconds))) = asking {
            say!("pairing: the computer is asking its owner; the code here is {code}, {seconds} s to confirm it there");
            self.dialog = Some(Dialog::Code { name: host_part(&target.address).to_owned(), code, until: Instant::now() + Duration::from_secs(seconds as u64) });
            self.dirty = true;
        }
        if let Some(left) = self.session.as_ref().and_then(|s| s.shared.second_factor.lock().unwrap().take()) {
            say!("the computer asks for a code from an authenticator app{}", left.map(|n| format!(" (wrong; {n} tries left)")).unwrap_or_default());
            let name = self.session.as_ref().map(|s| s.target.name.clone()).unwrap_or_default();
            self.dialog = Some(Dialog::SecondFactor { name, left });
            self.fields.insert(id("second.code"), Field::default());
            self.focus(id("second.code"), false);
            self.dirty = true;
        }
        if let Some(features) = self.session.as_ref().and_then(|s| s.shared.features.lock().unwrap().take()) {
            say!("the computer now offers {features:?}");
            self.session.as_mut().unwrap().features = features;
            self.follow_features();
            self.dirty = true;
        }
        self.arrivals();
        // The computer took the second display away, or couldn't make it.
        if let Some(why) = self.session.as_ref().and_then(|s| s.shared.second_gone.lock().unwrap().take()) {
            if self.session.as_ref().is_some_and(|s| s.second_on) {
                self.say(&if why.is_empty() { "The second display is gone".to_owned() } else { format!("The second display couldn't be made: {why}") });
                self.set_second(false, false);
            }
        }
        // A decoder that takes frames and gives no pictures gets a keyframe.
        if let Some(s) = &self.session {
            if s.shared.stuck.swap(false, Ordering::Relaxed) {
                say!("the decoder was given frames and gave no picture: asking the computer for a keyframe");
                if let Some(client) = &s.client {
                    client.send_message(Message::Keyframe);
                }
            }
        }
        // The bar says what the stream is, which may have changed.
        let format = self.format();
        if format != self.shown_format {
            self.shown_format = format;
            self.dirty = true;
        }
        // A picture: whatever the computer was doing before it let this PC in is over.
        if (self.logging_in != 0 || self.unlocking.is_some()) && self.session.as_ref().is_some_and(|s| s.shared.presented_total.load(Ordering::Relaxed) > 0) {
            self.logging_in = 0;
            // And a password that got this PC in is one worth keeping, if that was asked.
            if let Some(used) = self.unlocking.take() {
                if used.remember && !used.kept {
                    let name = self.session.as_ref().map(|s| s.target.name.clone()).unwrap_or_default();
                    match crate::creds::keep(&used.computer, &name, &used.password) {
                        Ok(()) => say!("the password is kept in Credential Manager"),
                        Err(e) => self.say(&format!("The password could not be kept on this PC: {e}")),
                    }
                }
            }
        }
        // A captured pointer is drawn where the computer says its own is.
        if self.captured && self.session.as_ref().is_some_and(|s| s.shared.cursor_moved.swap(false, Ordering::Relaxed)) {
            self.dirty = true;
        }
        let closed = self.session.as_ref().and_then(|s| s.shared.closed.lock().unwrap().clone());
        if let Some(reason) = closed {
            if matches!(self.dialog, Some(Dialog::Paired { .. })) {
                // The connection kept from pairing went before it was used: nothing to say.
                self.end(false);
            } else {
                self.stop(Some(&reason));
            }
        }
        if let Some(why) = self.scene.failed.lock().unwrap().take() {
            say!("failed: {why}");
            self.code = Some(2);
            self.effects.push(Effect::Quit);
        }
    }

    /// Keep a connection that has just answered.
    fn hold(&mut self, client: Client, shared: Arc<Shared>, target: Target, display: Display) {
        let (_, quality, bitrate) = self.choices(&target.address);
        let (features, os) = (client.host_features.clone(), client.host_os.clone());
        // A computer that wants a code from an authenticator app says so through this, at once if
        // it already has: `poll` shows the dialog.
        let asked = shared.clone();
        client.on_second_factor(move |left| {
            *asked.second_factor.lock().unwrap() = Some(left);
            asked.scene.wake_window();
        });
        self.session = Some(Session::new(Some(Arc::new(client)), shared, target, features, display, quality, bitrate, os));
    }

    /// What arrived from the computer: text for this clipboard, and word of files.
    fn arrivals(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        let received = std::mem::take(&mut *s.shared.received.lock().unwrap());
        let results = std::mem::take(&mut *s.shared.results.lock().unwrap());
        let copy = std::mem::take(&mut *s.shared.copies.lock().unwrap()).pop();
        let mut news = Vec::new();
        // Files the computer sent: it is told what became of each, and so is the person here.
        for (id, outcome) in received {
            if let Some(client) = &s.client {
                client.send_message(Message::FileResult { id, ok: outcome.is_ok(), message: match &outcome { Ok(at) => at.display().to_string(), Err(why) => why.clone() } });
            }
            news.push(match outcome {
                Ok(at) => format!("A file from the computer: {}", at.display()),
                Err(why) => format!("A file from the computer didn't arrive: {why}"),
            });
        }
        for (ok, said) in results {
            news.push(if ok { format!("Sent: it is at {said}") } else { format!("The file wasn't taken: {said}") });
        }
        // Only the newest copy matters.
        if let (Some((kind, bytes)), true) = (copy, s.clipboard_on) {
            if kind.starts_with("text/plain") {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                say!("clipboard: {} characters of text from the computer", text.chars().count());
                s.from_computer = Some(text.clone());
                s.sent_copy = None;
                self.effects.push(Effect::SetClipboard(text));
            } else if kind == "image/png" {
                say!("clipboard: a picture of {} bytes from the computer", bytes.len());
                s.sent_copy = None;
                self.effects.push(Effect::SetClipboardPicture(bytes));
            }
        }
        for line in news {
            self.say(&line);
        }
    }

    /// The computer's owner switched something on or off while this PC is connected: what is
    /// withdrawn stops.
    fn follow_features(&mut self) {
        let Some(s) = &self.session else { return };
        if s.sound_on && !s.offers("sound") {
            self.set_sound(false);
        }
        let Some(s) = self.session.as_mut() else { return };
        if s.clipboard_on && !s.offers("clipboard") {
            s.clipboard_on = false;
        }
        let (mic, camera) = (s.mic_on && !s.offers("mic"), s.camera_on && !s.offers("camera"));
        // The window says whether this device may control the computer.
        let title = format!("{}{} - remoteman", s.target.name, if s.view_only() { " (view only)" } else { "" });
        if mic {
            self.set_microphone(false);
        }
        if camera {
            self.set_camera(false);
        }
        self.effects.push(Effect::Title(title));
    }

    /// Play the computer's sound here, or stop.
    pub fn set_sound(&mut self, on: bool) {
        let Some(s) = self.session.as_mut() else { return };
        let Some(client) = s.client.clone() else {
            s.sound_on = on;
            return;
        };
        if on && s.offers("sound") {
            match crate::sound::Player::start() {
                Ok(player) => {
                    *s.shared.sound.lock().unwrap() = Some(player);
                    client.send_message(Message::Sound { on: true });
                    s.sound_on = true;
                    say!("sound: asked for");
                }
                Err(e) => self.say(&format!("The sound can't be played here: {e:#}")),
            }
        } else {
            if s.sound_on {
                client.send_message(Message::Sound { on: false });
            }
            *s.shared.sound.lock().unwrap() = None;
            s.sound_on = false;
        }
        self.dirty = true;
    }

    /// Add a second display to the session, or take it away. `tell`: the computer doesn't
    /// know yet.
    pub fn set_second(&mut self, on: bool, tell: bool) {
        let Some(s) = &self.session else { return };
        if on == s.second_on {
            return;
        }
        let (address, name, client, second) = (s.target.address.clone(), s.target.name.clone(), s.client.clone(), s.shared.second.clone());
        if on {
            if !s.offers("displays") {
                return self.say("This computer gives a device one display");
            }
            if second.is_none() {
                return;
            }
            // The size of this monitor, like the first; beside the computer's other displays.
            let display = Display { placement: Placement::Extend, ..self.display_for(Some(&address), Placement::Extend) };
            if let Some(client) = &client {
                say!("second display: asked for {}x{} at {} Hz", display.width, display.height, display.refresh);
                // On the side of the computer's other displays chosen for this computer.
                let side = self.store.find(&address).map_or_else(Default::default, |c| c.second_side);
                say!("second display: on the side {side:?}");
                client.send_message(Message::Display { index: 1, on: true, display: Some(display), side, message: String::new() });
            }
            self.effects.push(Effect::OpenSecond(format!("{name}: second display")));
        } else {
            if let (true, Some(client)) = (tell, &client) {
                say!("second display: given back");
                client.send_message(Message::Display { index: 1, on: false, display: None, side: Default::default(), message: String::new() });
            }
            self.effects.push(Effect::CloseSecond);
        }
        self.session.as_mut().unwrap().second_on = on;
        self.dirty = true;
    }

    /// Share the clipboard with the session's computer, or stop.
    pub fn set_clipboard(&mut self, on: bool) {
        let Some(s) = self.session.as_mut() else { return };
        let on = on && s.offers("clipboard");
        if let Some(client) = &s.client {
            if on != s.clipboard_on {
                client.send_message(Message::Clipboard { on });
                say!("clipboard: sharing {}", if on { "on" } else { "off" });
            }
        }
        s.clipboard_on = on;
        if on {
            // What is on this clipboard now goes over, as the computer's comes here.
            (s.from_computer, s.sent_copy) = (None, None);
            self.effects.push(Effect::ReadClipboard);
        }
        self.dirty = true;
    }

    /// This PC's clipboard holds this text now (it changed, or sharing has just begun). What
    /// came from the computer is not sent back.
    pub fn copied_here(&mut self, text: String) {
        use remoteman_core::media::{BLOB_CLIPBOARD, CLIPBOARD_LIMIT, ClipboardHeader};
        let Some(s) = self.session.as_mut() else { return };
        if !s.clipboard_on || !s.offers("clipboard") || text.is_empty() || text.len() as u64 > CLIPBOARD_LIMIT || s.from_computer.as_deref() == Some(text.as_str()) || s.sent_copy.as_deref() == Some(text.as_str()) {
            return;
        }
        s.sent_copy = Some(text.clone());
        let Some(client) = &s.client else { return };
        let header = ClipboardHeader { seq: s.shared.copy_seq.fetch_add(1, Ordering::Relaxed) + 1, mime: "text/plain;charset=utf-8".into(), size: text.len() as u64 };
        let blob = client.open_blob(BLOB_CLIPBOARD, serde_json::to_string(&header).unwrap_or_default());
        say!("clipboard: {} characters of text sent to the computer", text.chars().count());
        // Writing waits while the network is behind, which the window mustn't.
        let _ = std::thread::Builder::new().name("copy".into()).spawn(move || blob.write(text.as_bytes()) && blob.finish());
    }

    /// Files from a computer go into this folder from now on.
    pub fn set_folder(&mut self, folder: &Path) {
        self.store.settings.folder = folder.display().to_string();
        self.store.save_settings();
        if let Some(s) = &self.session {
            *s.shared.folder.lock().unwrap() = self.store.receive_folder();
        }
        say!("received files go to {}", folder.display());
        self.dirty = true;
    }

    /// This PC's clipboard holds a picture and no text: it goes to the computer as a PNG.
    pub fn copied_picture(&mut self, png: Vec<u8>) {
        use remoteman_core::media::{BLOB_CLIPBOARD, CLIPBOARD_LIMIT, ClipboardHeader};
        let Some(s) = self.session.as_mut() else { return };
        if !s.clipboard_on || !s.offers("clipboard") || png.is_empty() || png.len() as u64 > CLIPBOARD_LIMIT {
            return;
        }
        s.sent_copy = None;
        let Some(client) = &s.client else { return };
        let header = ClipboardHeader { seq: s.shared.copy_seq.fetch_add(1, Ordering::Relaxed) + 1, mime: "image/png".into(), size: png.len() as u64 };
        let blob = client.open_blob(BLOB_CLIPBOARD, serde_json::to_string(&header).unwrap_or_default());
        say!("clipboard: a picture of {} bytes sent to the computer", png.len());
        let _ = std::thread::Builder::new().name("copy".into()).spawn(move || blob.write(&png) && blob.finish());
    }

    /// Send a file to the session's computer, which decides where it goes and answers when it
    /// has it.
    pub fn send_file(&mut self, path: &Path) {
        use remoteman_core::media::{BLOB_FILE, FileHeader};
        use std::io::Read;
        let Some(s) = &self.session else { return };
        let Some(client) = s.client.as_ref().filter(|_| s.offers("files")) else { return };
        let opened = std::fs::File::open(path).and_then(|f| f.metadata().map(|m| (f, m.len())));
        let (mut file, size) = match opened {
            Ok(opened) => opened,
            Err(e) => return self.say(&format!("{} can't be sent: {e}", path.display())),
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let header = FileHeader { id: s.shared.file_seq.fetch_add(1, Ordering::Relaxed) + 1, name: name.clone(), size };
        let blob = client.open_blob(BLOB_FILE, serde_json::to_string(&header).unwrap_or_default());
        let (shared, said) = (s.shared.clone(), name.clone());
        let _ = std::thread::Builder::new().name("file".into()).spawn(move || {
            let mut piece = vec![0u8; 256 * 1024];
            loop {
                match file.read(&mut piece) {
                    Ok(0) => {
                        blob.finish();
                        return;
                    }
                    Ok(n) if blob.write(&piece[..n]) => {}
                    // The connection went: given up (dropping the blob tells the computer).
                    Ok(_) => return,
                    Err(e) => {
                        shared.results.lock().unwrap().push((false, format!("{said} could not be read: {e}")));
                        shared.scene.wake_window();
                        return;
                    }
                }
            }
        });
        self.say(&format!("Sending {name} ({size} bytes)…"));
    }

    /// Show why there is no session, and do what that reason asks for by itself.
    fn trouble(&mut self, target: Target, problem: Problem) {
        self.retry_at = None;
        match &problem {
            Problem::LoggingIn(said) => {
                // Tried again by itself every few seconds, for half a minute; then it is the
                // person's turn.
                self.logging_in += 1;
                say!("the computer is logging its user in ({said}): try {}", self.logging_in);
                if self.logging_in <= status::LOGGING_IN_TRIES {
                    self.retry_at = Some(Instant::now() + Duration::from_secs(3));
                } else {
                    self.logging_in = 0;
                }
            }
            Problem::Locked(said) => {
                say!("the computer is locked: {said}");
                self.fields.insert(id("locked.password"), Field::default());
                self.focus(id("locked.password"), false);
                let computer = self.key_of(&target.address).unwrap_or_default();
                // What became of the password that was just tried, if one was: one kept here
                // that the computer says is wrong is out of date.
                let tried = self.unlocking.take();
                if tried.as_ref().is_some_and(|t| t.kept) && said.to_ascii_lowercase().contains("not right") {
                    say!("the password kept on this PC is not right any more: forgotten");
                    crate::creds::forget(&computer);
                }
                // Asked for the first time in this try: a password kept for this computer is
                // sent without anyone typing it.
                if let Some(password) = (tried.is_none() && !computer.is_empty()).then(|| crate::creds::find(&computer)).flatten() {
                    say!("unlocking with the password kept on this PC");
                    self.unlock = Some(password.clone());
                    self.unlocking = Some(Unlocking { computer, password, remember: false, kept: true });
                    self.retry_at = Some(Instant::now());
                }
                // A test's password, typed by nobody, once.
                if let Some(typed) = self.args.password_from.take().and_then(|p| std::fs::read_to_string(p).ok()) {
                    self.unlock = typed.lines().next().map(str::to_owned);
                    self.retry_at = Some(Instant::now());
                }
            }
            _ => self.logging_in = 0,
        }
        self.screen = Screen::Problem(target, problem);
        self.dirty = true;
    }

    /// An attempt to connect came to nothing.
    fn failed(&mut self, target: &Target, error: &str, pairing: bool) {
        say!("failed: {error}");
        let problem = status::classify(error);
        if pairing {
            let host = host_part(&target.address);
            let text = match problem {
                Problem::Unreachable(detail) => format!("{host} did not answer. {detail}."),
                Problem::NotPaired => format!("{host} is not in pairing mode. Start pairing there and try again."),
                Problem::Changed => format!("{host} is not the computer this PC was paired with at that address. Forget that one first."),
                Problem::Refused(said) | Problem::LoggingIn(said) | Problem::Locked(said) => format!("{host} said: {said}"),
                _ => error.to_owned(),
            };
            self.dialog = Some(Dialog::Pair { waiting: None, problem: Some(text) });
            self.input.focus = id("pair.address");
            return;
        }
        if matches!(problem, Problem::Unreachable(_)) {
            self.reach.insert(target.address.clone(), Reach::Unreachable);
        }
        if self.refull {
            self.refull = false;
            self.effects.push(Effect::Fullscreen(false));
        }
        let waits = matches!(problem, Problem::LoggingIn(_) | Problem::Locked(_));
        self.trouble(target.clone(), problem);
        if self.args.host.is_some() && self.args.seconds.is_some() && !(waits && self.retry_at.is_some()) {
            // A test's session: there is nobody to read the page.
            self.code = Some(2);
            self.effects.push(Effect::Quit);
        }
    }

    /// The session that is held starts being shown.
    pub fn enter(&mut self) {
        let Some(s) = &self.session else { return };
        let (address, display, quality) = (s.target.address.clone(), s.display, s.quality);
        if let Some(client) = &s.client {
            self.store.used(&address, &client.host_name, &client.host_os);
            if s.bitrate > 0 && s.offers("configure") {
                client.send_message(Message::Configure { display: None, bitrate: Some(s.bitrate * 1_000_000), decoders: None });
            }
        }
        // A placement given on the command line is that connection's, not the one last used.
        if self.args.given_for(&address).is_none_or(|g| g.placement.is_none()) {
            self.store.settings.placement = display.placement;
        }
        self.store.save_settings();
        self.reach.remove(&address);
        let name = self.name_of(&address);
        let starts = self.store.find(&address).map_or((false, false), |c| (c.sound, c.clipboard));
        let s = self.session.as_mut().unwrap();
        s.target.name = name.clone();
        s.quality = quality;
        s.began = Instant::now();
        s.asked = Some((display.width, display.height));
        s.wanted = (display.width, display.height);
        *self.scene.session.lock().unwrap() = Some(s.shared.clone());
        self.scene.poke();
        self.effects.push(Effect::ForgetCursor);
        let view_only = self.session.as_ref().is_some_and(|s| s.view_only());
        self.effects.push(Effect::Title(format!("{name}{} - remoteman", if view_only { " (view only)" } else { "" })));
        // Room for the picture, if the list's window was small.
        if !self.maximised && !self.fullscreen && self.args.host.is_none() && (self.size.0 < 1224.0 || self.size.1 < 744.0) && self.front_size.is_none() {
            self.front_size = Some(self.size);
            self.effects.push(Effect::Size(1224.0, 744.0));
        }
        self.dialog = None;
        self.menu = None;
        self.screen = Screen::Session;
        self.figures = Figures::default();
        self.last_second = Instant::now();
        self.bar_out = false;
        self.bar_slide = 0.0;
        if (self.refull || self.store.settings.fullscreen || self.args.fullscreen) && !self.fullscreen {
            self.effects.push(Effect::Fullscreen(true));
        }
        self.refull = false;
        if self.args.input_demo {
            if let Some(s) = &self.session {
                if let Some(client) = &s.client {
                    crate::demo::start(s.shared.clone(), client.clone());
                }
            }
        }
        // What this computer's sessions start with, as its settings say (the switches in the
        // bar are then this session's own).
        if self.args.sound || starts.0 {
            self.set_sound(true);
        }
        if starts.1 {
            self.set_clipboard(true);
        }
        if self.args.second {
            self.set_second(true, true);
        }
        self.dirty = true;
    }

    /// End the session and go back to the list, or, with a reason, to the page that gives it.
    pub fn stop(&mut self, reason: Option<&str>) {
        let Some(s) = &self.session else { return };
        let (target, shown) = (s.target.clone(), s.shared.presented_total.load(Ordering::Relaxed) > 0);
        if matches!(self.dialog, Some(Dialog::SecondFactor { .. })) {
            self.dialog = None;
        }
        self.end(false);
        self.summary.as_mut().unwrap().ended = reason.map(str::to_owned);
        match reason {
            Some(reason) => {
                say!("the session ended: {reason}");
                self.trouble(target, status::ended(reason, shown));
            }
            None => self.screen = Screen::Computers,
        }
        if self.args.host.is_some() && self.args.seconds.is_some() && self.retry_at.is_none() {
            self.code = Some(if shown { 0 } else { 1 });
            self.effects.push(Effect::Quit);
        }
    }

    /// End the session. `stay`: the window keeps its size and full screen, because another
    /// session is about to start.
    fn end(&mut self, stay: bool) {
        let Some(s) = self.session.take() else { return };
        *self.scene.session.lock().unwrap() = None;
        self.scene.poke();
        let stats = s.client.as_ref().map(|c| c.stats()).unwrap_or_default();
        self.summary = Some(Summary {
            seconds: s.began.elapsed().as_secs_f64(),
            decoded: s.shared.decoded_total.load(Ordering::Relaxed),
            shown: s.shared.presented_total.load(Ordering::Relaxed),
            replaced: s.shared.replaced_total.load(Ordering::Relaxed),
            lost: stats.lost,
            keyframe_requests: stats.keyframe_requests,
            round_trip_ms: stats.round_trip_us as f64 / 1000.0,
            decoder: s.shared.decoder.lock().unwrap().clone(),
            ended: None,
        });
        *s.shared.sound.lock().unwrap() = None;
        crate::media::stop_all(&s.shared);
        if s.second_on {
            if let Some(second) = &s.shared.second {
                say!("second display: {} decoded, {} presented", second.decoded_total.load(Ordering::Relaxed), second.presented_total.load(Ordering::Relaxed));
            }
            self.effects.push(Effect::CloseSecond);
        }
        if let Some(client) = s.client {
            retire(client);
        }
        self.effects.push(Effect::ReleaseKeys);
        self.effects.push(Effect::Title("remoteman".into()));
        if stay {
            self.refull = self.fullscreen;
        } else {
            if self.fullscreen {
                self.effects.push(Effect::Fullscreen(false));
            }
            if let Some((w, h)) = self.front_size.take() {
                self.effects.push(Effect::Size(w, h));
            }
        }
        self.menu = None;
        self.bar_out = false;
        if self.screen == Screen::Session {
            self.screen = Screen::Computers;
        }
        self.dirty = true;
    }

    /// Start the session again, after a choice that the computer only takes at the start.
    fn reconnect(&mut self) {
        let Some(target) = self.session.as_ref().map(|s| s.target.clone()) else { return };
        self.end(true);
        self.connect(target, false);
    }

    /// Leaving: nothing may be left connected.
    pub fn closing(&mut self) {
        self.give_up();
        if self.session.is_some() {
            self.end(false);
        }
    }

    // What presses do.

    pub fn update(&mut self, msg: Msg) {
        self.dirty = true;
        self.tip = None;
        match msg {
            Msg::Minimise => self.effects.push(Effect::Minimise),
            Msg::Maximise => self.effects.push(Effect::Maximise),
            Msg::Close => self.effects.push(Effect::Close),
            Msg::Back => self.back(),
            Msg::OpenMenu(kind, anchor) => {
                // The button that opened a menu closes it again.
                self.menu = if self.menu.as_ref().is_some_and(|m| m.kind == kind) { None } else { Some(Menu { kind, anchor }) };
                if self.menu.is_some() {
                    self.effects.push(Effect::ReleaseKeys);
                }
            }
            Msg::CloseMenu => self.menu = None,
            Msg::Pair(address) => {
                self.menu = None;
                self.fields.insert(id("pair.address"), Field::with(address.as_deref().map(short).unwrap_or_default()));
                self.dialog = Some(Dialog::Pair { waiting: None, problem: None });
                self.focus(id("pair.address"), false);
                if matches!(self.screen, Screen::Problem(..)) {
                    self.screen = Screen::Computers;
                }
            }
            Msg::DialogOk => self.dialog_ok(),
            Msg::DialogCancel => self.dialog_cancel(),
            Msg::Connect(address) => {
                let target = self.target_of(&address);
                self.connect(target, false);
            }
            Msg::CancelConnect => {
                self.give_up();
                self.screen = Screen::Computers;
            }
            Msg::Unlock => {
                let typed = self.fields.insert(id("locked.password"), Field::default()).map(|f| f.string()).unwrap_or_default();
                if let (false, Screen::Problem(target, _)) = (typed.is_empty(), self.screen.clone()) {
                    let computer = self.key_of(&target.address).unwrap_or_default();
                    self.unlocking = Some(Unlocking { remember: self.remember && !computer.is_empty(), computer, password: typed.clone(), kept: false });
                    self.unlock = Some(typed);
                    self.connect(target, false);
                }
            }
            Msg::ToggleRemember => self.remember = !self.remember,
            Msg::PickFolder => self.effects.push(Effect::PickFolder),
            Msg::ToggleSound => {
                let on = self.session.as_ref().is_some_and(|s| s.sound_on);
                self.set_sound(!on);
            }
            Msg::ToggleSecond => {
                self.menu = None;
                let on = self.session.as_ref().is_some_and(|s| s.second_on);
                self.set_second(!on, true);
            }
            Msg::ToggleClipboard => {
                self.menu = None;
                let on = self.session.as_ref().is_some_and(|s| s.clipboard_on);
                self.set_clipboard(!on);
            }
            Msg::SendFile => {
                self.menu = None;
                self.effects.push(Effect::PickFile);
            }
            Msg::Toggle(flag, None) => {
                let s = &mut self.store.settings;
                let field = match flag {
                    Flag::MatchRefresh => &mut s.match_refresh,
                    Flag::CtrlAsCommand => &mut s.ctrl_as_command,
                    Flag::Shortcuts => &mut s.shortcuts,
                    Flag::Sound => &mut s.sound,
                    Flag::Clipboard => &mut s.clipboard,
                    Flag::Hdr => &mut s.hdr,
                    Flag::VariableRefresh => &mut s.variable_refresh,
                };
                *field = !*field;
                let now = *field;
                self.store.save_settings();
                say!("setting: {} is {}", match flag {
                    Flag::MatchRefresh => "match refresh",
                    Flag::CtrlAsCommand => "Ctrl as Command",
                    Flag::Shortcuts => "system shortcuts",
                    Flag::Sound => "sound",
                    Flag::Clipboard => "clipboard",
                    Flag::Hdr => "HDR",
                    Flag::VariableRefresh => "variable refresh rate",
                }, if now { "on" } else { "off" });
                if flag == Flag::Hdr {
                    self.follow_hdr();
                }
            }
            Msg::Toggle(flag, Some(address)) => self.store.update(&address, |c| match flag {
                Flag::Sound => c.sound = !c.sound,
                Flag::Clipboard => c.clipboard = !c.clipboard,
                _ => {}
            }),
            Msg::OpenSettings => {
                self.menu = None;
                self.scroll = 0.0;
                self.screen = Screen::Settings;
            }
            Msg::OpenComputer(address) => {
                self.menu = None;
                if let Some(c) = self.store.find(&address) {
                    self.fields.insert(id("computer.name"), Field::with(&c.name));
                    self.scroll = 0.0;
                    self.screen = Screen::Computer(address);
                }
            }
            Msg::EditAddress(address) => {
                self.menu = None;
                if self.store.find(&address).is_some() {
                    if !matches!(self.screen, Screen::Computer(_)) {
                        self.update(Msg::OpenComputer(address.clone()));
                    }
                    self.fields.insert(id("address.new"), Field::with(short(&address)));
                    self.dialog = Some(Dialog::Address { address, problem: None });
                    self.focus(id("address.new"), false);
                }
            }
            Msg::AskForget(address) => {
                self.menu = None;
                self.dialog = Some(Dialog::Forget(address));
            }
            Msg::About => {
                self.menu = None;
                self.dialog = Some(Dialog::About);
            }
            Msg::Disconnect => self.stop(None),
            Msg::SendKeys(codes) => {
                self.menu = None;
                if let Some(client) = self.input_client() {
                    let usages: Vec<u16> = codes.iter().filter_map(|c| keys::linux_to_hid(*c)).collect();
                    for usage in &usages {
                        client.send(Event::Key { usage: *usage, down: true });
                    }
                    for usage in usages.iter().rev() {
                        client.send(Event::Key { usage: *usage, down: false });
                    }
                    say!("sent keys: usages {usages:02x?}");
                }
            }
            Msg::Place(placement) => {
                self.menu = None;
                let Some(s) = &self.session else { return };
                if s.display.placement != placement {
                    let address = s.target.address.clone();
                    self.store.update(&address, |c| c.placement = placement);
                    self.drop_given_placement(&address);
                    say!("placement: {} asked for, by connecting again", crate::store::placement_key(placement));
                    self.reconnect();
                }
            }
            Msg::Pick(quality) => {
                self.menu = None;
                let Some(s) = &self.session else { return };
                if s.quality != quality {
                    let address = s.target.address.clone();
                    self.store.update(&address, |c| c.quality = Some(quality));
                    say!("picture quality: {} asked for, by connecting again", quality.key());
                    self.reconnect();
                }
            }
            Msg::Slide(_, part) => {
                let step = BITRATES[((part * (BITRATES.len() - 1) as f32).round() as usize).min(BITRATES.len() - 1)];
                self.set_bitrate(step);
            }
            Msg::ToggleFigures => {
                self.store.settings.figures = !self.store.settings.figures;
                self.store.save_settings();
            }
            Msg::ToggleFullscreen => {
                self.menu = None;
                self.effects.push(Effect::Fullscreen(!self.fullscreen));
            }
            Msg::BarMode(hover) => {
                self.menu = None;
                self.store.settings.hover_bar = hover;
                self.store.save_settings();
                self.bar_out = false;
                self.bar_slide = 0.0;
            }
            Msg::StartFullscreen => {
                self.store.settings.fullscreen = !self.store.settings.fullscreen;
                self.store.save_settings();
            }
            Msg::Choose(choice, n) => {
                self.menu = None;
                self.choose(choice, n);
            }
            Msg::ToggleMicrophone => {
                let on = self.session.as_ref().is_some_and(|s| s.mic_on);
                self.set_microphone(!on);
            }
            Msg::ToggleCamera => {
                let on = self.session.as_ref().is_some_and(|s| s.camera_on);
                self.set_camera(!on);
            }
            Msg::Set(what, address) => {
                self.set_option(what, None, address.as_deref());
            }
            Msg::Wake(address) => self.wake(&address),
        }
    }

    fn set_bitrate(&mut self, megabits: u32) {
        let Some(s) = self.session.as_mut() else { return };
        if s.bitrate == megabits {
            return;
        }
        s.bitrate = megabits;
        if let (Some(client), true) = (&s.client, s.offers("configure")) {
            // 0 gives the choice back to the computer.
            client.send_message(Message::Configure { display: None, bitrate: Some(megabits * 1_000_000), decoders: None });
            say!("bitrate limit: {megabits} Mb/s asked for");
        }
        let address = s.target.address.clone();
        self.store.update(&address, |c| c.bitrate = megabits);
    }

    /// What a drop-down offers: each choice, whether it can be chosen, and which is chosen now.
    pub fn options(&self, choice: &Choice) -> (Vec<(String, bool)>, usize) {
        let decodes = |word: &str| self.decoders.iter().any(|m| mode_word(m) == word);
        let full = self.decoders.iter().any(|m| m.chroma == remoteman_core::Chroma::C444);
        let qualities = [(Quality::Automatic, true), (Quality::Sharp, FEATURES.sharp_text && full), (Quality::Hevc, decodes("hevc")), (Quality::Av1, decodes("av1"))];
        match choice {
            Choice::Controls => (vec![("Title bar".into(), true), ("Show the bar on hover".into(), true)], self.store.settings.hover_bar as usize),
            Choice::DefaultQuality => (qualities.iter().map(|(q, on)| (q.name().to_owned(), *on)).collect(), qualities.iter().position(|(q, _)| *q == self.store.settings.quality).unwrap_or(0)),
            Choice::Placement(address) => {
                let now = self.store.find(address).map_or(Placement::Extend, |c| c.placement);
                (PLACEMENT_NAMES.iter().map(|p| (p.1.to_owned(), p.0 != Placement::Mirror || FEATURES.mirror)).collect(), PLACEMENT_NAMES.iter().position(|p| p.0 == now).unwrap_or(0))
            }
            Choice::Size(address) => {
                let now = self.store.find(address).map_or(0, |c| c.max_height);
                (SIZES.iter().map(|s| (s.1.to_owned(), true)).collect(), SIZES.iter().position(|s| s.0 == now).unwrap_or(0))
            }
            Choice::Scroll => (vec![("Same as this PC".into(), true), ("Reversed".into(), true)], self.store.settings.scroll_reversed as usize),
            Choice::WindowsKey => (crate::store::Stand::ALL.iter().map(|s| (s.2.to_owned(), true)).collect(), crate::store::Stand::ALL.iter().position(|s| s.0 == self.store.settings.super_key).unwrap_or(0)),
            Choice::WayIn(address) => (vec![("The computer decides".into(), true), ("Only a session that is already running".into(), true)], self.store.find(address).is_some_and(|c| c.attach_only) as usize),
            Choice::Pointer => (vec![("Follows this pointer".into(), true), ("Captured".into(), true)], self.store.settings.capture as usize),
            Choice::Side(address) => {
                let now = self.store.find(address).map_or(remoteman_core::Side::Right, |c| c.second_side);
                (crate::store::SIDES.iter().map(|s| (s.2.to_owned(), true)).collect(), crate::store::SIDES.iter().position(|s| s.0 == now).unwrap_or(0))
            }
            Choice::Quality(address) => {
                let now = self.store.find(address).and_then(|c| c.quality);
                let mut list = vec![("Same as Settings".to_owned(), true)];
                list.extend(qualities.iter().map(|(q, on)| (q.name().to_owned(), *on)));
                (list, now.and_then(|n| qualities.iter().position(|(q, _)| *q == n)).map_or(0, |i| i + 1))
            }
            Choice::Bitrate(address) => {
                let now = self.store.find(address).map_or(0, |c| c.bitrate);
                let name = |m: &u32| if *m == 0 { "Automatic".to_owned() } else { format!("{m} Mb/s") };
                // One set with the session's slider, which has more steps, is shown as it is.
                let mut list: Vec<(String, bool)> = BITRATE_CHOICES.iter().map(|m| (name(m), true)).collect();
                let at = BITRATE_CHOICES.iter().position(|m| *m == now).unwrap_or_else(|| {
                    list.push((name(&now), true));
                    list.len() - 1
                });
                (list, at)
            }
        }
    }

    fn choose(&mut self, choice: Choice, n: usize) {
        const QUALITIES: [Quality; 4] = [Quality::Automatic, Quality::Sharp, Quality::Hevc, Quality::Av1];
        match choice {
            Choice::Controls => self.update(Msg::BarMode(n == 1)),
            Choice::DefaultQuality => {
                self.store.settings.quality = QUALITIES[n.min(3)];
                self.store.save_settings();
            }
            Choice::Placement(address) => {
                self.store.update(&address, |c| c.placement = PLACEMENT_NAMES[n.min(3)].0);
                self.drop_given_placement(&address);
            }
            Choice::Size(address) => self.store.update(&address, |c| c.max_height = SIZES[n.min(3)].0),
            Choice::Scroll => {
                self.store.settings.scroll_reversed = n == 1;
                self.store.save_settings();
            }
            Choice::WindowsKey => {
                self.store.settings.super_key = crate::store::Stand::ALL[n.min(3)].0;
                self.store.save_settings();
            }
            Choice::WayIn(address) => self.store.update(&address, |c| c.attach_only = n == 1),
            Choice::Pointer => {
                self.store.settings.capture = n == 1;
                self.store.save_settings();
            }
            Choice::Side(address) => self.store.update(&address, |c| c.second_side = crate::store::SIDES[n.min(3)].0),
            Choice::Quality(address) => self.store.update(&address, |c| c.quality = n.checked_sub(1).map(|i| QUALITIES[i.min(3)])),
            Choice::Bitrate(address) => {
                if let Some(megabits) = BITRATE_CHOICES.get(n).copied() {
                    if self.session.as_ref().is_some_and(|s| s.target.address == address) {
                        self.set_bitrate(megabits);
                    } else {
                        self.store.update(&address, |c| c.bitrate = megabits);
                    }
                }
            }
        }
    }

    fn back(&mut self) {
        if self.menu.take().is_some() {
            return;
        }
        if self.dialog.is_some() {
            return self.dialog_cancel();
        }
        match self.screen {
            Screen::Computer(_) => {
                self.save_name();
                self.screen = self.home();
            }
            Screen::Settings => self.screen = self.home(),
            Screen::Problem(..) => {
                (self.retry_at, self.logging_in, self.unlock) = (None, 0, None);
                self.screen = Screen::Computers;
            }
            Screen::Connecting => self.update(Msg::CancelConnect),
            Screen::Computers | Screen::Session => {}
        }
        self.scroll = 0.0;
    }

    fn dialog_ok(&mut self) {
        match self.dialog.clone() {
            Some(Dialog::Pair { waiting: None, .. }) => {
                let typed = self.fields.get(&id("pair.address")).map(|f| f.string()).unwrap_or_default();
                let typed = typed.trim();
                if typed.is_empty() {
                    self.dialog = Some(Dialog::Pair { waiting: None, problem: Some("Enter the computer's address first.".into()) });
                    return;
                }
                let address = with_port(typed);
                let target = self.target_of(&address);
                self.dialog = Some(Dialog::Pair { waiting: Some(host_part(&address).to_owned()), problem: None });
                self.start_attempt(target, true, true);
            }
            Some(Dialog::Pair { .. }) | Some(Dialog::Code { .. }) => {}
            Some(Dialog::SecondFactor { .. }) => {
                let typed = self.fields.get(&id("second.code")).map(|f| f.string()).unwrap_or_default();
                let code: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
                // Six digits, or the dialog stays as it is.
                if code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()) {
                    if let Some(client) = self.session.as_ref().and_then(|s| s.client.clone()) {
                        client.second_factor(&code, self.remember);
                    }
                    self.dialog = None;
                    self.remember = false;
                }
            }
            Some(Dialog::Paired { .. }) => {
                // If the connection has gone meanwhile, `poll` has already let it go.
                self.dialog = None;
                if self.session.is_some() {
                    self.enter();
                }
            }
            Some(Dialog::Forget(address)) => {
                self.dialog = None;
                if self.session.as_ref().is_some_and(|s| s.target.address == address) {
                    self.stop(None);
                }
                if let Some(computer) = self.store.key(&address) {
                    crate::creds::forget(&computer);
                }
                self.store.forget(&address);
                self.reach.remove(&address);
                if matches!(self.screen, Screen::Computer(_) | Screen::Problem(..)) {
                    self.screen = Screen::Computers;
                }
            }
            Some(Dialog::Address { address, .. }) => {
                let typed = self.fields.get(&id("address.new")).map(|f| f.string()).unwrap_or_default();
                let new = with_port(&typed);
                if host_part(&new).is_empty() {
                    return;
                }
                match self.store.readdress(&address, &new) {
                    Ok(()) => {
                        // Nothing is known about the new address.
                        self.reach.remove(&address);
                        self.dialog = None;
                        if matches!(self.screen, Screen::Computer(_)) {
                            self.screen = Screen::Computer(new);
                        }
                    }
                    Err(why) => self.dialog = Some(Dialog::Address { address, problem: Some(why) }),
                }
            }
            Some(Dialog::About) => self.dialog = None,
            None => {}
        }
    }

    fn dialog_cancel(&mut self) {
        match self.dialog.take() {
            Some(Dialog::Pair { waiting: Some(_), .. }) | Some(Dialog::Code { .. }) => self.give_up(),
            // The connection kept from pairing isn't wanted now.
            Some(Dialog::Paired { .. }) => self.end(false),
            // Not giving the code: the session is over.
            Some(Dialog::SecondFactor { .. }) => {
                self.remember = false;
                self.stop(None);
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// The name typed in a computer's settings is kept when the field is left.
    fn save_name(&mut self) {
        let Screen::Computer(address) = &self.screen else { return };
        let address = address.clone();
        let new = self.fields.get(&id("computer.name")).map(|f| f.string().trim().to_owned()).unwrap_or_default();
        if !new.is_empty() && self.store.find(&address).is_some_and(|c| c.name != new) {
            self.store.update(&address, |c| c.name = new.clone());
            if let Some(s) = self.session.as_mut().filter(|s| s.target.address == address) {
                s.target.name = new;
            }
        }
    }

    // The pointer.

    fn region_at(&self, x: f32, y: f32) -> Option<&Region> {
        self.regions.iter().rev().find(|r| r.rect.contains(x, y))
    }

    pub fn over_interface(&self, x: f32, y: f32) -> bool {
        self.region_at(x, y).is_some()
    }

    pub fn hit(&self, x: f32, y: f32) -> Hit {
        match self.region_at(x, y).map(|r| r.kind) {
            Some(Kind::Drag) => Hit::Caption,
            Some(Kind::Maximise) => Hit::Maximise,
            _ => Hit::Client,
        }
    }

    pub fn pointer(&self, x: f32, y: f32) -> Pointer {
        match self.region_at(x, y) {
            Some(r) if r.kind == Kind::Field && r.enabled => Pointer::Text,
            Some(_) => Pointer::Arrow,
            None if self.forwarding() => Pointer::Theirs,
            None => Pointer::Arrow,
        }
    }

    fn focus(&mut self, to: Id, keys: bool) {
        if self.input.focus != to {
            if self.input.focus == id("computer.name") {
                self.save_name();
            }
            self.input.focus = to;
            self.blink = Instant::now();
            self.input.caret = true;
        }
        self.input.keys = keys;
        self.dirty = true;
    }

    /// The pointer moved; says whether the interface took it (if not it is the picture's).
    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.mouse = (x, y);
        let over = self.region_at(x, y).map(|r| (r.id, r.kind, r.rect));
        let hot = over.map_or(0, |o| o.0);
        if hot != self.input.hot {
            self.input.hot = hot;
            self.hot_since = Instant::now();
            self.tip = None;
            self.dirty = true;
        }
        // A press that is held acts on what it went down on.
        if self.input.down != 0 {
            if let Some(r) = self.regions.iter().find(|r| r.id == self.input.down) {
                let (kind, rect, rid) = (r.kind, r.rect, r.id);
                match kind {
                    Kind::Slider => self.update(Msg::Slide(rid, ((x - rect.x) / rect.w).clamp(0.0, 1.0))),
                    Kind::Field => {
                        let at = self.caret_at(rid, rect, x);
                        if let Some(f) = self.fields.get_mut(&rid) {
                            f.go(at, true);
                            self.dirty = true;
                        }
                    }
                    _ => {}
                }
            }
            return true;
        }
        if self.session.is_some() && self.screen == Screen::Session && self.bar_hidden() {
            self.bar_watch(y, over.is_some());
        }
        over.is_some()
    }

    /// A hidden bar comes out when the pointer reaches the top edge and goes two seconds after
    /// the pointer has left it.
    fn bar_watch(&mut self, y: f32, over: bool) {
        if self.native_bar && self.fullscreen {
            // The app's bar does the rest (it comes out, and goes when it is let go of).
            let at = y < 2.0;
            if at && !self.edge {
                self.effects.push(Effect::BarEdge);
            }
            self.edge = at;
            return;
        }
        if !self.bar_out && y < 2.0 {
            self.bar_out = true;
            self.dirty = true;
        }
        if self.bar_out {
            self.bar_until = if over || y < 2.0 || self.menu.is_some() { None } else { self.bar_until.or(Some(Instant::now() + Duration::from_secs(2))) };
        }
    }

    pub fn mouse_leave(&mut self) {
        self.mouse = (-1.0, -1.0);
        self.edge = false;
        if self.input.hot != 0 {
            self.input.hot = 0;
            self.tip = None;
            self.dirty = true;
        }
        if self.bar_out && self.bar_until.is_none() && self.menu.is_none() {
            self.bar_until = Some(Instant::now() + Duration::from_secs(2));
        }
    }

    fn caret_at(&self, field: Id, r: Rect, x: f32) -> usize {
        let Some(f) = self.fields.get(&field) else { return 0 };
        // A password is drawn as dots, so that is what the pointer is over.
        let shown = if field == id("locked.password") { vec![ui::DOT; f.text.len()] } else { f.text.clone() };
        let inner = r.w - 20.0;
        let shift = (self.fonts.caret_x(&shown, ui::Font::Body, f.caret) - inner + 2.0).max(0.0);
        self.fonts.caret_at(&shown, ui::Font::Body, x - r.x - 10.0 + shift)
    }

    /// The left button went down; says whether the interface took it.
    pub fn mouse_down(&mut self, x: f32, y: f32) -> bool {
        self.mouse_move(x, y);
        self.tip = None;
        let Some(r) = self.region_at(x, y) else {
            self.focus(0, false);
            return false;
        };
        let (rid, kind, rect, enabled, focusable, msg) = (r.id, r.kind, r.rect, r.enabled, r.focus, r.msg.clone());
        self.dirty = true;
        if !enabled {
            return true;
        }
        self.focus(if focusable { rid } else { 0 }, false);
        match kind {
            Kind::Block => {
                if let Some(msg) = msg {
                    self.update(msg);
                }
            }
            Kind::Slider => {
                self.input.down = rid;
                self.update(Msg::Slide(rid, ((x - rect.x) / rect.w).clamp(0.0, 1.0)));
            }
            Kind::Field => {
                self.input.down = rid;
                let at = self.caret_at(rid, rect, x);
                if let Some(f) = self.fields.get_mut(&rid) {
                    f.go(at, false);
                }
            }
            Kind::Button | Kind::Maximise => self.input.down = rid,
            Kind::Drag => {}
        }
        true
    }

    pub fn mouse_up(&mut self, x: f32, y: f32) {
        let down = std::mem::take(&mut self.input.down);
        if down == 0 {
            return;
        }
        self.dirty = true;
        let pressed = self.region_at(x, y).filter(|r| r.id == down && r.enabled && matches!(r.kind, Kind::Button | Kind::Maximise)).and_then(|r| r.msg.clone());
        if let Some(msg) = pressed {
            self.update(msg);
        }
        self.mouse_move(x, y);
    }

    /// The wheel turned by `notches` (away from the user positive) over the interface.
    pub fn wheel(&mut self, notches: f32) {
        if self.dialog.is_none() && self.menu.is_none() {
            let to = (self.scroll - notches * 48.0).clamp(0.0, self.scroll_room.get());
            if to != self.scroll {
                self.scroll = to;
                self.dirty = true;
            }
        }
    }

    // The keyboard, when it is this app's own.

    /// A key went down, by its Windows virtual-key code; says whether it did anything.
    pub fn key_down(&mut self, vk: u32, shift: bool, ctrl: bool) -> bool {
        const TAB: u32 = 0x09;
        const ENTER: u32 = 0x0D;
        const ESCAPE: u32 = 0x1B;
        const SPACE: u32 = 0x20;
        let focus = self.input.focus;
        let field = self.regions.iter().any(|r| r.id == focus && r.kind == Kind::Field);
        if field && self.field_key(focus, vk, shift, ctrl) {
            self.blink = Instant::now();
            self.input.caret = true;
            self.dirty = true;
            return true;
        }
        match vk {
            TAB | 0x26 | 0x28 if vk == TAB || self.menu.is_some() => {
                // Only the top sheet's controls: what is under a dialog is out of reach.
                let top = self.regions.iter().map(|r| r.layer).max().unwrap_or(0);
                let list = |app: &Self| -> Vec<Id> { app.regions.iter().filter(|r| r.focus && r.layer == top).map(|r| r.id).collect() };
                let mut stops = list(self);
                let back = (vk == TAB && shift) || vk == 0x26;
                // Past the last control in sight (or before the first): what is scrolled out
                // of sight comes in, so the keyboard reaches all of a long page.
                let at = stops.iter().position(|s| *s == focus);
                let edge = if back { at == Some(0) } else { at.is_some_and(|i| i + 1 == stops.len()) };
                if edge && top == 0 {
                    let to = if back { (self.scroll - 220.0).max(0.0) } else { (self.scroll + 220.0).min(self.scroll_room.get()) };
                    if to != self.scroll {
                        self.scroll = to;
                        self.build();
                        let before = std::mem::replace(&mut stops, list(self));
                        // On to the first control that has come into sight, if the one
                        // that had the keyboard has gone out of it.
                        if !stops.contains(&focus) {
                            let new = |id: &&Id| !before.contains(id);
                            let to = if back { stops.iter().rev().find(new) } else { stops.iter().find(new) };
                            if let Some(to) = to.copied() {
                                self.focus(to, true);
                                return true;
                            }
                        }
                    }
                }
                if !stops.is_empty() {
                    let at = stops.iter().position(|s| *s == focus);
                    let next = match (at, back) {
                        (Some(i), false) => (i + 1) % stops.len(),
                        (Some(i), true) => (i + stops.len() - 1) % stops.len(),
                        (None, false) => 0,
                        (None, true) => stops.len() - 1,
                    };
                    self.focus(stops[next], true);
                }
                true
            }
            ENTER | SPACE => {
                if field && vk == ENTER {
                    if focus == id("computer.name") {
                        self.save_name();
                        self.dirty = true;
                    } else if focus == id("locked.password") {
                        self.update(Msg::Unlock);
                    } else {
                        self.update(Msg::DialogOk);
                    }
                    return true;
                }
                let msg = self.regions.iter().find(|r| r.id == focus && r.enabled && r.kind == Kind::Button).and_then(|r| r.msg.clone());
                match msg {
                    Some(msg) => {
                        self.update(msg);
                        self.input.keys = true;
                    }
                    None if vk == ENTER && self.dialog.is_some() => self.update(Msg::DialogOk),
                    None => return false,
                }
                true
            }
            ESCAPE => {
                self.update(Msg::Back);
                true
            }
            _ => false,
        }
    }

    fn field_key(&mut self, field: Id, vk: u32, shift: bool, ctrl: bool) -> bool {
        let Some(f) = self.fields.get_mut(&field) else { return false };
        match vk {
            0x25 => f.go(f.caret.saturating_sub(1), shift),
            0x27 => f.go(f.caret + 1, shift),
            0x24 => f.go(0, shift),
            0x23 => f.go(usize::MAX, shift),
            0x08 => f.erase(false),
            0x2E => f.erase(true),
            0x41 if ctrl => {
                f.anchor = 0;
                f.caret = f.text.len();
            }
            // A password doesn't leave its field.
            0x43 | 0x58 if ctrl && field == id("locked.password") => {}
            0x43 | 0x58 if ctrl => {
                crate::window::clipboard_set(&f.selected());
                if vk == 0x58 {
                    f.insert(&[]);
                }
            }
            0x56 if ctrl => {
                if let Some(text) = crate::window::clipboard_get() {
                    let line: Vec<u16> = text.encode_utf16().filter(|c| *c >= 0x20).collect();
                    f.insert(&line);
                }
            }
            _ => return false,
        }
        true
    }

    /// A character was typed.
    pub fn char(&mut self, c: u16) {
        let focus = self.input.focus;
        if c < 0x20 || c == 0x7F || !self.regions.iter().any(|r| r.id == focus && r.kind == Kind::Field) {
            return;
        }
        if let Some(f) = self.fields.get_mut(&focus) {
            f.insert(&[c]);
            self.blink = Instant::now();
            self.input.caret = true;
            self.dirty = true;
        }
    }

    /// The window lost the keyboard.
    pub fn blur(&mut self) {
        if self.input.keys {
            self.input.keys = false;
            self.dirty = true;
        }
    }

    // Time.

    /// Something on the window is moving, so `tick` wants to be called at every frame.
    pub fn animating(&self) -> bool {
        if self.still {
            return false;
        }
        let ring = self.screen == Screen::Connecting || self.retry_at.is_some() || matches!(self.dialog, Some(Dialog::Pair { waiting: Some(_), .. }));
        let target = if self.bar_out { 1.0 } else { 0.0 };
        ring || self.bar_slide != target
    }

    pub fn tick(&mut self) {
        self.nearby_tick();
        self.media_tick();
        let now = Instant::now();
        if self.animating() {
            let target = if self.bar_out { 1.0f32 } else { 0.0 };
            if self.bar_slide != target {
                // A sixth of a second from one end to the other.
                let step = 0.1;
                self.bar_slide = if target > self.bar_slide { (self.bar_slide + step).min(1.0) } else { (self.bar_slide - step).max(0.0) };
            }
            self.dirty = true;
        }
        if self.bar_out && self.menu.is_none() && self.bar_until.is_some_and(|t| now >= t) {
            self.bar_out = false;
            self.bar_until = None;
            self.dirty = true;
        }
        // The caret blinks twice a second, only while a field is being typed in.
        if !self.still && self.regions.iter().any(|r| r.id == self.input.focus && r.kind == Kind::Field) {
            let shown = (now - self.blink).as_millis() % 1060 < 530;
            if shown != self.input.caret {
                self.input.caret = shown;
                self.dirty = true;
            }
        }
        // A control that has something to say says it once the pointer has rested on it.
        if self.tip.is_none() && self.input.hot != 0 && self.input.down == 0 && now - self.hot_since > Duration::from_millis(600) {
            self.show_tip();
        }
        if now - self.last_second >= Duration::from_secs(1) {
            let seconds = (now - self.last_second).as_secs_f64();
            self.last_second = now;
            self.second(seconds);
            self.follow_window();
            // The seconds left to confirm the code count down.
            if matches!(self.dialog, Some(Dialog::Code { .. })) {
                self.dirty = true;
            }
        }
        if self.toast.as_ref().is_some_and(|t| now - t.1 > Duration::from_secs(5)) {
            self.toast = None;
            self.dirty = true;
        }
        // A computer that is logging in, or a password waiting to be tried: again.
        if self.retry_at.is_some_and(|at| now >= at) {
            self.retry_at = None;
            if let Screen::Problem(target, Problem::LoggingIn(_) | Problem::Locked(_)) = self.screen.clone() {
                self.connect(target, false);
            }
        }
    }

    /// Show what the control under the pointer has to say, if anything.
    pub fn show_tip(&mut self) {
        if let Some(r) = self.regions.iter().find(|r| r.id == self.input.hot && !r.tip.is_empty()) {
            self.tip = Some((r.tip.clone(), r.rect));
            self.dirty = true;
        }
    }

    /// About once a second: the figures.
    fn second(&mut self, seconds: f64) {
        let Some(s) = &self.session else { return };
        let stats = s.client.as_ref().map(|c| c.stats()).unwrap_or_default();
        let ms = |us: u64| us as f64 / 1000.0;
        let (decoded, shown, replaced) = (s.shared.decoded.swap(0, Ordering::Relaxed), s.shared.presented.swap(0, Ordering::Relaxed), s.shared.replaced.swap(0, Ordering::Relaxed));
        let decode_max = s.shared.decode_us_max.swap(0, Ordering::Relaxed);
        let megabits = s.shared.bytes.swap(0, Ordering::Relaxed) as f64 * 8.0 / 1e6 / seconds;
        let figures = Figures {
            shown: (shown as f64 / seconds).round() as u32,
            decoded: (decoded as f64 / seconds).round() as u32,
            decode_ms: decode_max as f64 / 1000.0,
            age_ms: ms(s.shared.age_presented_us.load(Ordering::Relaxed)),
            round_trip_ms: stats.round_trip_us as f64 / 1000.0,
            megabits,
            lost: stats.lost,
            replaced: s.shared.replaced_total.load(Ordering::Relaxed),
            decoder: s.shared.decoder.lock().unwrap().clone(),
        };
        if self.args.stats && s.client.is_some() {
            let or = |v: f64| if v == 0.0 { "?".to_owned() } else { format!("{v:.1}") };
            say!(
                "stats: decoded {decoded}, presented {shown}, replaced unseen {replaced}, age {} ms when decoded and {} ms when handed to the display, decode {:.1} ms at most, {megabits:.1} Mb/s, lost {}, {}",
                or(ms(s.shared.age_decoded_us.load(Ordering::Relaxed))),
                or(figures.age_ms),
                decode_max as f64 / 1000.0,
                stats.lost,
                figures.decoder,
            );
        }
        self.figures = figures;
        // What the processor managed with 4:4:4, kept for what is offered from now on.
        let rate = self.session.as_ref().map_or(0, |s| s.shared.cpu_444_rate.swap(0, Ordering::Relaxed));
        let kept = crate::store::kept_cpu_444(self.store.settings.cpu_444, rate);
        if rate > 0 && kept != self.store.settings.cpu_444 {
            say!("4:4:4 on the processor: {rate} million pixels a second measured; {kept} kept");
            self.store.settings.cpu_444 = kept;
            self.store.save_settings();
        }
        if self.screen == Screen::Session && (self.store.settings.figures || self.menu.as_ref().is_some_and(|m| m.kind == MenuKind::Picture)) {
            self.dirty = true;
        }
    }

    // Describing the window.

    /// The stream's size and rate, once the computer has said.
    pub fn format(&self) -> Option<(u32, u32, u32)> {
        let s = self.session.as_ref()?;
        let (w, h) = unpack(s.shared.stream_size.load(Ordering::Relaxed));
        (w > 0).then(|| (w, h, s.shared.refresh.load(Ordering::Relaxed)))
    }

    /// Where the picture goes, in the design's pixels: under the bar, or the whole window.
    pub fn video_area(&self) -> Option<Rect> {
        if self.screen != Screen::Session || self.session.is_none() {
            return None;
        }
        let top = if self.bar_hidden() || self.panel { 0.0 } else { crate::session::BAR };
        Some(ui::rect(0.0, top, self.size.0, (self.size.1 - top).max(1.0)))
    }

    /// The same in device pixels.
    pub fn video_pixels(&self) -> Option<(i32, i32, i32, i32)> {
        self.video_area().map(|r| {
            let k = self.scale;
            let (x, y) = ((r.x * k).round() as i32, (r.y * k).round() as i32);
            let (cw, ch) = unpack(self.scene.client_size.load(Ordering::Relaxed));
            (x, y, cw as i32 - x, ch as i32 - y)
        })
    }

    /// Describe the window as it is now and hand that to the draw thread.
    pub fn build(&mut self) -> u64 {
        self.dirty = false;
        let turn = if self.still { 0.0 } else { (self.started.elapsed().as_secs_f32() / 1.1).fract() };
        let mut ui = Ui::new(self.theme(), &self.fonts, self.input, self.size.0, self.size.1, turn);
        match &self.screen {
            Screen::Session if self.session.is_some() => crate::session::view(self, &mut ui),
            _ => crate::pages::view(self, &mut ui),
        }
        if let Some(dialog) = &self.dialog {
            ui.t = self.theme();
            ui.raise();
            crate::pages::dialog(self, &mut ui, dialog);
        }
        if let Some((text, by)) = &self.tip {
            ui.t = self.theme();
            crate::pages::tip(&mut ui, text, *by);
        }
        if let Some((text, _)) = &self.toast {
            crate::session::toast(&mut ui, text);
        }
        let Ui { ops, regions, .. } = ui;
        self.regions = regions;
        // What is under the pointer may be something else now.
        let hot = self.region_at(self.mouse.0, self.mouse.1).map_or(0, |r| r.id);
        if hot != self.input.hot && self.input.down == 0 {
            self.input.hot = hot;
            self.hot_since = Instant::now();
            self.dirty = true;
        }
        self.serial += 1;
        let backdrop = if self.dark { theme::DARK.bg } else { theme::LIGHT.bg };
        let frame = Frame { ops, scale: self.scale, video: self.video_pixels(), serial: self.serial, backdrop };
        *self.scene.frame.lock().unwrap() = Some(Arc::new(frame));
        self.scene.poke();
        self.serial
    }

    /// After an event: describe the window again if anything changed, and hand back what the
    /// window itself has to do.
    pub fn finish(&mut self) -> Vec<Effect> {
        // Twice at most: the second time for a control that turned out to be under the pointer.
        for _ in 0..2 {
            if self.dirty {
                self.build();
            }
        }
        std::mem::take(&mut self.effects)
    }
}

/// Let a connection go without making the window wait: saying goodbye can take a second.
fn retire(client: Arc<Client>) {
    let _ = std::thread::Builder::new().name("goodbye".into()).spawn(move || drop(client));
}

/// How the display for the computer at `address` is to sit among its own: as `remoteman
/// connect --placement` says for the computer it names, else as was chosen for that computer;
/// one not in the list gets the placement last used if the command line names it, and one being
/// paired starts as an extra display.
fn placement(args: &Args, store: &Store, address: &str) -> Placement {
    if let Some(given) = args.given_for(address).and_then(|g| g.placement) {
        return given;
    }
    match store.find(address) {
        Some(c) => c.placement,
        None if args.host.is_some() => store.settings.placement,
        None => Placement::Extend,
    }
}

/// Whether a `remoteman connect` run (`given`) is over: started, and on the list of computers.
fn connect_run_over(given: bool, started: bool, screen: &Screen) -> bool {
    given && started && *screen == Screen::Computers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connect_run_ends_where_others_show_the_list() {
        let problem = Screen::Problem(Target { address: "172.29.1.2:7791".into(), name: "guest".into() }, Problem::Ended("gone".into()));
        // Before the window is up the screen is the list's; then connecting, the session, a
        // problem page (with Try again) all stay; the list ends it.
        assert!(!connect_run_over(true, false, &Screen::Computers));
        for screen in [Screen::Connecting, Screen::Session, problem.clone(), Screen::Settings] {
            assert!(!connect_run_over(true, true, &screen), "{screen:?}");
        }
        assert!(connect_run_over(true, true, &Screen::Computers));
        // Any other run goes back to the list as before.
        assert!(!connect_run_over(false, true, &Screen::Computers));
    }

    #[test]
    fn the_placement_given_wins_over_the_computers_own() {
        let dir = std::env::temp_dir().join(format!("remoteman-placement-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("viewer-computers"), "[computer]\nname=guest\naddress=172.29.1.2:7791\nplacement=extend\n[computer]\nname=other\naddress=10.0.0.9:7791\nplacement=only\n").unwrap();
        std::fs::write(dir.join("viewer-settings"), "placement=mirror\n").unwrap();
        let store = Store::load(&dir);
        let words = |w: &str| w.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let mut args = crate::args::parse_from(words(&format!("172.29.1.2:7791 --state {}", dir.display()))).unwrap();
        // As it was: the computer's own, and the last used for one the command line names.
        assert_eq!(placement(&args, &store, "172.29.1.2:7791"), Placement::Extend);
        assert_eq!(placement(&args, &store, "10.0.0.9:7791"), Placement::Only);
        assert_eq!(placement(&args, &store, "10.0.0.10:7791"), Placement::Mirror);
        // Given for the computer named: that, for it alone.
        args.given = Some(crate::args::Given { address: "172.29.1.2:7791".into(), expect: "ab".repeat(32), cert: Vec::new(), key: Vec::new(), placement: Some(Placement::Main) });
        assert_eq!(placement(&args, &store, "172.29.1.2:7791"), Placement::Main);
        assert_eq!(placement(&args, &store, "10.0.0.9:7791"), Placement::Only);
        // Picked in the session: the computer's own again.
        args.given.as_mut().unwrap().placement = None;
        assert_eq!(placement(&args, &store, "172.29.1.2:7791"), Placement::Extend);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
