//! The engine's interface, for the app (`crates/remoteman-viewer-app-windows`, WinUI 3 through
//! windows-reactor), which links this crate as a library and calls it on its UI thread, the
//! one that owns its window. Every call comes on that thread.
//!
//! The engine drives the same [`App`] the old program does: what is on screen, what each
//! press does and the sessions are its, unchanged; the app shows its state and hands it what
//! people do. Between them:
//! - [`state`] gives the whole state as one JSON document. When it has changed, the engine
//!   calls the app's `notify` (on the app's thread, from inside one of these calls or from the
//!   engine's own window's messages): the app must only take note there and read the state
//!   later, never call back into the engine from inside `notify`;
//! - [`command`] takes what people did that isn't the picture's, as JSON (`Engine::command`
//!   lists the words); the state's `effects` are what only the app can do;
//! - the picture's input comes through typed calls ([`pointer`] and the rest), in the panel's
//!   pixels; [`attach`] makes the swap chain the app's `SwapChainPanel` shows.
//!
//! The engine has a window of its own that nobody sees (a message-only window on the app's
//! thread): other threads wake it, it ticks sixty times a second, it hears the clipboard
//! change and, while the pointer is captured, the mouse's raw movement. The keyboard hook that
//! takes the system's shortcuts during a session is the old program's, on the same thread.
//! In full screen the picture is presented from a top-level window of the engine's own
//! (`full.rs`), where a monitor can vary its refresh to follow the frames.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use remoteman_core::{Button, Event, Mode};
use serde_json::{Value, json};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, GetClipboardSequenceNumber};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyState, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, w};

use crate::app::{App, Choice, Dialog, Effect, Flag, Msg, Screen};
use crate::features::FEATURES;
use crate::status::{Problem, Reach};
use crate::store::{PLACEMENT_NAMES, Quality, key_words, os_name, placement_key, when};
use crate::stream::{Scene, Shared, pack, pack_scale, unpack};
use crate::ui::{Field, id};

const WM_WAKE: u32 = WM_APP + 1;
/// The wheel's notch in the protocol's pixels, as the old window has it.
const NOTCH: f32 = 40.0;

/// Called on the app's thread when the state has changed.
pub type Notify = Box<dyn Fn()>;

pub(crate) struct Engine {
    pub(crate) app: RefCell<App>,
    hwnd: isize,
    /// The app's window, and the panel's place in it: x, y, width, height in pixels.
    pub(crate) host: Cell<isize>,
    panel: Cell<(i32, i32, i32, i32)>,
    /// The panel's scale (pixels to the app's units), as last told.
    panel_scale: Cell<(f32, f32)>,
    notify: Notify,
    /// The state as last told about.
    told: RefCell<String>,
    /// What only the app can do, until it has read the state.
    effects: RefCell<Vec<Value>>,
    pub(crate) drawing: RefCell<Option<JoinHandle<()>>>,
    /// The panel's swap chain, kept for drawing into again after full screen.
    chain: RefCell<Option<windows::Win32::Graphics::Dxgi::IDXGISwapChain1>>,
    /// The engine's own full-screen window while there is one (`full.rs`).
    pub(crate) full: RefCell<Option<Rc<crate::full::Full>>>,
    deadline: Cell<Option<Instant>>,
    // Input, as the old window keeps it.
    held: RefCell<HashSet<u16>>,
    pub(crate) buttons: Cell<u8>,
    released: Cell<bool>,
    chord: Cell<bool>,
    captured: Cell<bool>,
    pointer_on: Cell<u8>,
    own_copy: Cell<u32>,
    hook: Cell<isize>,
    second: RefCell<Option<(isize, JoinHandle<()>, Arc<Shared>)>>,
    /// The host's cursor, made into a Windows one (worn in full screen).
    pub(crate) cursor: Cell<Option<HCURSOR>>,
    pub(crate) got_shape: Cell<bool>,
    /// When the monitor under the window was last looked at (`watch_monitor`).
    watched: Cell<Option<Instant>>,
    /// Fingers on the touchscreen that the computer knows of (`touch.rs`).
    fingers: RefCell<crate::touch::Fingers>,
    /// How busy this thread is with input and the state (`Busy`).
    pub(crate) busy: RefCell<Busy>,
}

/// For finding what holds this thread up during a session (with `--stats` or `--watch-vblank`),
/// in the log every five seconds while the pointer moves: how many pointer moves went to the
/// computer, how often the state was looked at and how long that took, and how often the app
/// was told. (Measured on this PC: about 0.16 ms a look; 500 moves a second in full screen
/// cost the thread about 8 % and told the app nothing more.)
#[derive(Default)]
pub(crate) struct Busy {
    since: Option<Instant>,
    pointers: u32,
    publishes: u32,
    publish_us: u64,
    longest_us: u64,
    told: u32,
}

thread_local! {
    static ENGINE: RefCell<Option<Rc<Engine>>> = const { RefCell::new(None) };
}

pub(crate) fn engine() -> Option<Rc<Engine>> {
    ENGINE.with(|e| e.borrow().clone())
}

// The interface.

/// Start the engine: `words` are the command line's words, as the old program takes them;
/// `notify` is called on this thread when the state has changed.
pub fn start(words: Vec<String>, notify: Notify) -> Result<()> {
    start_engine(words, notify).inspect_err(|e| say!("failed: {e:#}"))
}

/// The state as JSON, with the effects the app has to carry out since it last asked
/// (`effects`). `None` before the engine has started (or while it is busy, which a call from
/// inside `notify` would find).
pub fn state() -> Option<Value> {
    let e = engine()?;
    let mut v = e.state_value()?;
    *e.told.borrow_mut() = v.to_string();
    v["effects"] = Value::Array(e.effects.borrow_mut().drain(..).collect());
    Some(v)
}

/// Something people did, as JSON (see `Engine::command`). Whether it was understood.
pub fn command(v: &Value) -> bool {
    let Some(e) = engine() else { return false };
    let ok = e.command(v);
    e.publish();
    ok
}

/// A swap chain (`IDXGISwapChain1`) for the app's `SwapChainPanel`, as a raw COM pointer the
/// app now owns one reference of (it hands it to the panel and lets its own go). The engine's
/// `windows` crate and the app's WinUI library are different versions of windows-rs, so it
/// crosses as a pointer.
pub struct PanelChain(pub *mut c_void);

/// Give the engine the app's window (its handle) and get the swap chain for its panel; the
/// engine draws into it from now on. Called once the panel has a size ([`panel`] first).
pub fn attach(window: isize) -> Result<PanelChain> {
    let e = engine().context("the engine hasn't started")?;
    e.attach(window).inspect_err(|err| say!("the panel can't show pictures: {err:#}"))
}

/// Where the panel is in the app's window and how large, in pixels, and its scale.
pub fn panel(x: i32, y: i32, width: i32, height: i32, scale_x: f32, scale_y: f32) {
    let Some(e) = engine() else { return };
    e.panel.set((x, y, width, height));
    e.panel_scale.set((scale_x, scale_y));
    e.resync(scale_x, scale_y);
    e.publish();
}

/// The pointer is at this place in the panel, in pixels.
pub fn pointer(x: i32, y: i32) {
    let Some(e) = engine() else { return };
    if !e.captured.get() && e.forwarding() {
        e.pointer(x, y);
    }
}

/// A mouse button: 0 left, 1 right, 2 middle.
pub fn button(button: i32, down: bool, x: i32, y: i32) {
    let Some(e) = engine() else { return };
    e.button(button, down, Some((x, y)));
}

/// The wheel turned by this many notches (positive away from the user, or to the right).
pub fn wheel(notches: f32, horizontal: bool) {
    let Some(e) = engine() else { return };
    if e.forwarding() {
        e.wheel(notches, horizontal);
    }
}

/// A key on the picture, by where it is on the keyboard (set 1 scancode and the E0 bit).
/// Returns whether it went to the computer (the app then does nothing else with it).
pub fn key(scancode: u32, extended: bool, down: bool, repeat: bool) -> bool {
    let Some(e) = engine() else { return false };
    e.picture_key(scancode as u16, extended, down, repeat)
}

/// A finger on the touchscreen over the panel: `id` is Windows' pointer id for the contact,
/// `phase` 0 down, 1 move, 2 up (also for a cancelled contact or a lost capture), at this place
/// in the panel in pixels. Returns whether it went to the computer as a touch; if not, the app
/// treats it as the pointer, as before.
pub fn touch(id: u32, phase: i32, x: i32, y: i32) -> bool {
    let Some(e) = engine() else { return false };
    e.touch(id, crate::touch::phase(phase), x, y)
}

/// The panel got the keyboard, or lost it.
pub fn focus(focused: bool) {
    let Some(e) = engine() else { return };
    if !focused {
        e.hold_pointer(false);
        e.release_keys();
    }
}

/// Leaving: nothing may be left connected. Returns what the process should end with.
pub fn stop() -> i32 {
    let Some(e) = ENGINE.with(|x| x.borrow_mut().take()) else { return 2 };
    e.release_keys();
    e.close_second();
    crate::full::leave(&e, false);
    e.hold_pointer(false);
    let hook = e.hook.replace(0);
    // SAFETY: our own hook and window.
    unsafe {
        if hook != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(hook as *mut _));
        }
        let _ = KillTimer(Some(HWND(e.hwnd as *mut _)), 1);
    }
    let scene = {
        let mut app = e.app.borrow_mut();
        app.closing();
        app.scene.clone()
    };
    scene.quit.store(true, Ordering::Relaxed);
    e.stop_drawing(&scene);
    let app = e.app.borrow();
    if let Some(s) = &app.summary {
        say!(
            "in {:.1} s: decoded {} ({:.1} a second), presented {} ({:.1} a second), replaced unseen {}, lost {}, keyframe requests {}, round trip {:.1} ms, {}",
            s.seconds,
            s.decoded,
            s.decoded as f64 / s.seconds.max(0.001),
            s.shown,
            s.shown as f64 / s.seconds.max(0.001),
            s.replaced,
            s.lost,
            s.keyframe_requests,
            s.round_trip_ms,
            s.decoder,
        );
        if let Some(reason) = &s.ended {
            say!("the session ended: {reason}");
        }
    }
    app.code.unwrap_or(0)
}

/// Whether Windows is set to dark apps.
pub fn system_dark() -> bool {
    crate::window::system_dark()
}

/// For tests: lines in the engine's log (`--log`) from the app, marked as the view's.
pub fn say(line: &str) {
    say!("view: {line}");
}

// Starting.

fn start_engine(words: Vec<String>, notify: Notify) -> Result<()> {
    let args = crate::args::parse_from(words)?;
    if let Some(path) = &args.log {
        crate::log::to_file(path)?;
    }
    // A panic: say why first, since a window app has no stderr to see it on.
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        say!("panic: {info}");
        default(info);
    }));
    let gpu = crate::gpu::Gpu::open()?;
    say!("engine: gpu {}", gpu.name);
    let mut decoders: Vec<Mode> = Vec::new();
    let forced = std::env::var("REMOTEMAN_SOFTWARE").as_deref() == Ok("1");
    for mode in &args.decoders {
        let on_card = if forced { Err(anyhow::anyhow!("REMOTEMAN_SOFTWARE=1")) } else { gpu.decodes(*mode) };
        match on_card {
            Ok(()) => decoders.push(*mode),
            // 4:4:4 on the CPU leaves the codec's 4:2:0 where it was.
            Err(e) if mode.chroma == remoteman_core::Chroma::C444 => match crate::decode::can_software(mode.codec) {
                Ok(()) => {
                    say!("decoder {:?} 4:4:4: {e:#}; on the CPU", mode.codec);
                    decoders.push(*mode);
                }
                Err(soft) => say!("decoder {:?} 4:4:4 left out: {e:#}; {soft:#}", mode.codec),
            },
            Err(e) => match crate::decode::use_software(mode.codec) {
                Ok(()) => {
                    say!("decoder {:?}: not on this card ({e:#}); on the CPU instead", mode.codec);
                    decoders.push(*mode);
                }
                Err(soft) => say!("decoder {:?} left out: {e:#}; {soft:#}", mode.codec),
            },
        }
    }
    decoders.sort_by_key(|m| crate::decode::is_software(m.codec));
    say!("decoders offered: {}", decoders.iter().map(|m| format!("{:?}{}", m.codec, if m.chroma == remoteman_core::Chroma::C444 { "444" } else { "" }).to_lowercase()).collect::<Vec<_>>().join(", "));
    let scene = Arc::new(Scene::default());
    let dark = args.dark.unwrap_or_else(crate::window::system_dark);
    let seconds = args.seconds;
    let mut app = App::new(args, gpu, scene.clone(), decoders, dark)?;
    app.panel = true;
    app.mica = true;
    // SAFETY: Win32 calls with structures that live through each call.
    let hwnd = unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW { lpfnWndProc: Some(procedure), hInstance: instance.into(), lpszClassName: w!("remoteman-engine"), ..Default::default() };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("remoteman-engine"), w!("remoteman-engine"), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(instance.into()), None).context("the engine's window")?;
        let _ = AddClipboardFormatListener(hwnd);
        SetTimer(Some(hwnd), 1, 16, None);
        hwnd
    };
    scene.hwnd.store(hwnd.0 as isize, Ordering::Relaxed);
    let e = Rc::new(Engine {
        app: RefCell::new(app),
        hwnd: hwnd.0 as isize,
        host: Cell::new(0),
        panel: Cell::new((0, 0, 0, 0)),
        panel_scale: Cell::new((1.0, 1.0)),
        notify,
        told: RefCell::new(String::new()),
        effects: RefCell::new(Vec::new()),
        drawing: RefCell::new(None),
        chain: RefCell::new(None),
        full: RefCell::new(None),
        deadline: Cell::new(seconds.map(|s| Instant::now() + Duration::from_secs(s))),
        held: RefCell::new(HashSet::new()),
        buttons: Cell::new(0),
        released: Cell::new(false),
        chord: Cell::new(false),
        captured: Cell::new(false),
        pointer_on: Cell::new(0),
        own_copy: Cell::new(0),
        hook: Cell::new(0),
        second: RefCell::new(None),
        cursor: Cell::new(None),
        got_shape: Cell::new(false),
        watched: Cell::new(None),
        fingers: RefCell::new(crate::touch::Fingers::default()),
        busy: RefCell::new(Busy::default()),
    });
    ENGINE.with(|x| *x.borrow_mut() = Some(e.clone()));
    // The Windows key, Alt+Tab and the like, while a session has the keyboard.
    // SAFETY: a hook on this thread's own procedure.
    unsafe {
        if let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(GetModuleHandleW(None)?.into()), 0) {
            e.hook.set(hook.0 as isize);
        }
    }
    say!("engine: started");
    Ok(())
}

impl Engine {
    fn attach(&self, window: isize) -> Result<PanelChain> {
        self.host.set(window);
        // Before anything connects: the hello says whether this monitor is in HDR.
        self.watch_monitor();
        let gpu = self.app.borrow().gpu.clone();
        let (_, _, w, h) = self.panel.get();
        let swap = crate::draw::panel_chain(&gpu, w.max(8) as u32, h.max(8) as u32)?;
        *self.chain.borrow_mut() = Some(swap.clone());
        self.draw_panel()?;
        say!("engine: the panel shows the picture");
        if self.app.borrow().args.watch_vblank {
            let scene = self.app.borrow().scene.clone();
            scene.measure.store(true, Ordering::Relaxed);
            crate::monitor::watch_vblanks(window, scene);
        }
        // (Taken out first: the app mustn't stay borrowed while the player starts.)
        let play = self.app.borrow().args.play.clone();
        if let Some(path) = play {
            self.play(&path)?;
        } else {
            // Now there is a window: the computer named on the command line, if one was.
            self.app.borrow_mut().start();
            self.publish();
        }
        // The app's reference: it hands it to its panel.
        Ok(PanelChain(swap.into_raw()))
    }

    /// Draw into the panel's swap chain (again, after full screen).
    pub(crate) fn draw_panel(&self) -> Result<()> {
        if self.drawing.borrow().is_some() {
            return Ok(());
        }
        let Some(swap) = self.chain.borrow().clone() else { return Ok(()) };
        struct Send1(windows::Win32::Graphics::Dxgi::IDXGISwapChain1);
        // SAFETY: the swap chain is free-threaded; it is only used by the draw thread from
        // here on (the panel only shows it).
        unsafe impl Send for Send1 {}
        let swap = Send1(swap);
        let (gpu, scene, shot) = {
            let app = self.app.borrow();
            (app.gpu.clone(), app.scene.clone(), app.args.shot.clone())
        };
        let drawing = crate::draw::start_on(gpu, scene, move || {
            let swap = swap;
            crate::draw::Surface::Panel(swap.0)
        }, shot)?;
        *self.drawing.borrow_mut() = Some(drawing);
        Ok(())
    }

    /// Stop the draw thread there is (the panel's or full screen's), and wait for it.
    pub(crate) fn stop_drawing(&self, scene: &Scene) {
        scene.stop_draw.store(true, Ordering::Relaxed);
        scene.poke();
        if let Some(drawing) = self.drawing.borrow_mut().take() {
            let _ = drawing.join();
        }
        scene.stop_draw.store(false, Ordering::Relaxed);
    }

    /// The panel's size and scale, told to the draw thread and the app again (after full
    /// screen, which had its own).
    pub(crate) fn repanel(&self) {
        let (sx, sy) = self.panel_scale.get();
        self.resync(sx, sy);
    }

    /// The window that has the keyboard when the session has it: the app's, or the engine's
    /// own in full screen.
    pub(crate) fn front(&self) -> bool {
        // SAFETY: a plain query.
        let front = unsafe { GetForegroundWindow() }.0 as isize;
        front == self.host.get() || self.full.borrow().as_ref().is_some_and(|f| f.hwnd == front)
    }

    /// A mouse button on the picture (0 left, 1 right, 2 middle), at this place in pixels when
    /// the pointer isn't captured.
    pub(crate) fn button(&self, button: i32, down: bool, at: Option<(i32, i32)>) {
        let (button, bit) = match button {
            1 => (Button::Right, 2),
            2 => (Button::Middle, 4),
            _ => (Button::Left, 1),
        };
        if !self.forwarding() && !(!down && self.buttons.get() & bit != 0) {
            return;
        }
        if let (false, Some((x, y))) = (self.captured.get(), at) {
            self.pointer(x, y);
        }
        if down {
            self.send(Event::Button { button, down: true });
            self.buttons.set(self.buttons.get() | bit);
        } else if self.buttons.get() & bit != 0 {
            self.send(Event::Button { button, down: false });
            self.buttons.set(self.buttons.get() & !bit);
        }
    }

    pub(crate) fn wheel(&self, notches: f32, horizontal: bool) {
        let k = if self.app.borrow().store.settings.scroll_reversed { -NOTCH } else { NOTCH };
        // The wheel counts away from the user positive; the protocol counts towards the end of
        // the document positive.
        self.send(if horizontal { Event::Scroll { dx: notches * k, dy: 0.0 } } else { Event::Scroll { dx: 0.0, dy: -notches * k } });
    }

    /// A key on the picture: F11 and Ctrl+Alt+Shift+I are the window's own; the rest goes to
    /// the computer while it has the keyboard. Whether it was taken.
    pub(crate) fn picture_key(&self, scancode: u16, extended: bool, down: bool, repeat: bool) -> bool {
        const F11: u16 = 0x57;
        const I: u16 = 0x17;
        if !self.forwarding() {
            // A key let go that the computer was told is down still goes.
            if !down {
                self.key(scancode, extended, false);
            }
            return false;
        }
        let held = |vk: i32| unsafe { GetKeyState(vk) } < 0;
        if down && scancode == F11 && !extended {
            // The window's own, as in the Linux viewer.
            if !repeat {
                self.app.borrow_mut().update(Msg::ToggleFullscreen);
                self.publish();
            }
            return true;
        }
        if down && scancode == I && held(0x10) && held(0x11) && held(0x12) {
            self.chord.set(false);
            if !repeat {
                self.app.borrow_mut().update(Msg::ToggleFigures);
                self.publish();
            }
            return true;
        }
        self.key(scancode, extended, down);
        self.publish();
        true
    }

    /// For tests: a made-up session whose picture is a recorded HEVC stream, played at 60 a
    /// second over and over through the same decoder and drawing as a real one.
    fn play(&self, path: &std::path::Path) -> Result<()> {
        use remoteman_core::{Handler, StreamInfo};
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        // Access units start at each delimiter (HEVC NAL type 35).
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 4 < bytes.len() {
            if bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 1 {
                if (bytes[i + 3] >> 1) & 0x3f == 35 {
                    starts.push(if i > 0 && bytes[i - 1] == 0 { i - 1 } else { i });
                }
                i += 3;
            } else {
                i += 1;
            }
        }
        anyhow::ensure!(!starts.is_empty(), "no access unit delimiters in {}", path.display());
        starts.push(bytes.len());
        let units: Vec<Vec<u8>> = starts.windows(2).map(|w| bytes[w[0]..w[1]].to_vec()).collect();
        let mut app = self.app.borrow_mut();
        let (shared, mut feed) = crate::stream::start(&app.gpu, &app.scene)?;
        let colour = app.args.play_colour.clone();
        let (w, h) = app.args.size.unwrap_or((1920, 1080));
        let (mode, rate) = (app.args.play_mode, app.args.play_rate);
        say!("play: {} access units of {}, {mode:?}, {colour}, {}", units.len(), path.display(), if rate == 0 { "as fast as they are decoded".to_owned() } else { format!("{rate} a second") });
        let display = remoteman_core::Display { width: w, height: h, scale: 1.0, refresh: rate.max(1), placement: remoteman_core::Placement::Extend, hdr: colour != "bt709", headroom: 0.0 };
        let target = crate::app::Target { address: "test.invalid:7791".into(), name: format!("{} (played)", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()) };
        // A played HDR stream stands for a computer that sends HDR; `--play-features` adds more.
        let extra = if display.hdr { Some("hdr".to_owned()) } else { None };
        let features = std::iter::once("video".to_owned()).chain(extra).chain(app.args.play_features.iter().cloned()).collect();
        app.session = Some(crate::app::Session::new(None, shared, target, features, display, crate::store::Quality::Automatic, 0, "linux".into()));
        app.enter();
        drop(app);
        std::thread::Builder::new().name("play".into()).spawn(move || {
            let info = StreamInfo { epoch: 1, mode, width: w, height: h, refresh: rate.max(1), colour, sdr_white: 203, peak: 1000 };
            feed.stream(&info);
            let mut number = 0u32;
            // Each frame at its time, not after the one before plus a sleep: the rate is the
            // rate asked for, however long feeding takes.
            let started = Instant::now();
            loop {
                for (n, data) in units.iter().enumerate() {
                    let frame = remoteman_core::Frame { key: n == 0, recovery: false, epoch: 1, number, capture_us: 0, data: data.clone() };
                    feed.frame(&frame, None);
                    number += 1;
                    if rate > 0 {
                        let due = started + Duration::from_secs_f64(number as f64 / rate as f64);
                        std::thread::sleep(due.saturating_duration_since(Instant::now()));
                    }
                }
            }
        })?;
        self.publish();
        Ok(())
    }

    /// The panel's size or scale changed, or the monitor under it.
    pub(crate) fn resync(&self, sx: f32, sy: f32) {
        let (_, _, w, h) = self.panel.get();
        // In full screen the picture's place is the engine's own window, which says its size.
        if w <= 0 || h <= 0 || self.full.borrow().is_some() {
            return;
        }
        let host = HWND(self.host.get() as *mut _);
        let (mw, mh, hz, ..) = crate::window::monitor_of(host);
        self.watch_monitor();
        let mut app = self.app.borrow_mut();
        app.scene.client_size.store(pack(w as u32, h as u32), Ordering::Relaxed);
        app.scene.panel_scale.store(pack_scale(sx, sy), Ordering::Relaxed);
        app.scene.poke();
        app.monitor = (mw, mh, hz);
        let maximised = unsafe { IsZoomed(host).as_bool() };
        app.resized(w as f32 / sx, h as f32 / sy, sx, maximised);
    }

    /// The monitor under the window, as it is now: whether it is in HDR (the hello says so, and
    /// a session is asked again when that changes), its refresh, and whether it varies its
    /// refresh. Looked at when the panel changes and once a second, since Windows' HDR switch
    /// and the window's monitor can change without the panel's size.
    pub(crate) fn watch_monitor(&self) {
        let host = HWND(self.host.get() as *mut _);
        if host.0.is_null() {
            return;
        }
        self.watched.set(Some(Instant::now()));
        let Ok(mut app) = self.app.try_borrow_mut() else { return };
        // (A test may say it for it.)
        let light = crate::gpu::screen_light(host);
        let found = light.hdr;
        let (hdr, headroom) = match app.args.screen_hdr {
            Some(forced) => (forced, if forced { light.headroom().max(1000.0 / 203.0) } else { 0.0 }),
            None => (found, light.headroom()),
        };
        let changed = app.scene.screen_hdr.swap(hdr, Ordering::Relaxed) != hdr;
        let (white, peak) = (light.white.to_bits(), light.peak.to_bits());
        let lighter = app.scene.screen_white.swap(white, Ordering::Relaxed) != white || app.scene.screen_peak.swap(peak, Ordering::Relaxed) != peak;
        if changed || lighter || app.scene.headroom.load(Ordering::Relaxed) == 0 {
            say!(
                "screen: {} (Windows says {}){}",
                if hdr { "HDR" } else { "SDR" },
                if found { "HDR" } else { "SDR" },
                if found { format!(", SDR white {:.0} nits, brightest {:.0}, headroom {headroom:.2}", light.white, light.peak) } else { String::new() }
            );
            // The draw thread fits an HDR picture to them anew.
            app.scene.poke();
        }
        app.scene.headroom.store(headroom.to_bits().max(1), Ordering::Relaxed);
        let (mw, mh, hz, _, _) = crate::window::monitor_of(host);
        app.scene.refresh_hz.store(hz, Ordering::Relaxed);
        // (Also before the panel has a size: what a computer named on the command line is
        // asked for.)
        app.monitor = (mw, mh, hz);
        *app.scene.vrr_range.lock().unwrap() = crate::monitor::vrr_of(host);
        if changed {
            app.scene.poke();
        }
        // In a session, the computer is asked again if that changed what is asked of it (or
        // when a second display that held the change back has gone).
        app.follow_hdr();
        let took = self.watched.get().map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
        if took > 2.0 {
            say!("screen: looking at the monitor took {took:.1} ms");
        }
        drop(app);
        self.decide_refresh();
    }

    /// Whether pictures are presented as they arrive (variable refresh) or at the display's
    /// refresh: the switch, full screen, the monitor, and DXGI.
    pub(crate) fn decide_refresh(&self) {
        let Ok(app) = self.app.try_borrow() else { return };
        let scene = &app.scene;
        let range = *scene.vrr_range.lock().unwrap();
        let (on, why) = crate::monitor::variable_refresh(FEATURES.variable_refresh && app.store.settings.variable_refresh, app.fullscreen, range, scene.tearing_allowed.load(Ordering::Relaxed));
        if scene.variable_refresh.swap(on, Ordering::Relaxed) != on {
            say!("variable refresh: {}", if on { "on: each picture is presented as it arrives" } else { why });
            scene.poke();
        }
        drop(app);
        // Full screen presents one way or the other (`full::varies`).
        crate::full::follow_refresh(self);
    }

    pub(crate) fn forwarding(&self) -> bool {
        self.app.try_borrow().map(|a| a.forwarding()).unwrap_or(false)
    }

    pub(crate) fn send(&self, event: Event) {
        let Ok(app) = self.app.try_borrow() else { return };
        if let Some(client) = app.client().cloned() {
            drop(app);
            client.send(event);
        } else if app.args.play.is_some() && app.session.is_some() {
            // For tests: a played session has no computer to send to; what would have gone is
            // in the log (the pointer's place at most once a second).
            static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
            if matches!(event, Event::PointerTo { .. } | Event::PointerBy { .. }) {
                let mut last = LAST.lock().unwrap();
                if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
                    return;
                }
                *last = Some(Instant::now());
            }
            say!("input (a played session, not sent): {event:?}");
        }
    }

    pub(crate) fn point_at(&self, index: u8) {
        if self.pointer_on.replace(index) != index {
            self.send(Event::Display(index));
        }
    }

    pub(crate) fn pointer(&self, x: i32, y: i32) {
        self.busy.borrow_mut().pointers += 1;
        self.point_at(0);
        let at = self.app.borrow().fraction(x as f32, y as f32);
        if let Some((x, y)) = at {
            self.send(Event::PointerTo { x, y });
        }
    }

    /// A key by where it is on the keyboard (as the old window's).
    pub(crate) fn key(&self, scancode: u16, extended: bool, down: bool) {
        let Some(own) = crate::keys::usage(scancode, extended) else { return };
        let modifier = (0xE0..=0xE7).contains(&own);
        let usage = self.app.borrow().key_for(own);
        if down {
            self.held.borrow_mut().insert(usage);
            self.send(Event::Key { usage, down: true });
            let down = |vk: i32| unsafe { GetKeyState(vk) } < 0;
            self.chord.set(modifier && down(0x11) && down(0x12) && down(0x10) && self.held.borrow().iter().all(|u| (0xE0..=0xE7).contains(u)));
        } else {
            if self.held.borrow_mut().remove(&usage) {
                self.send(Event::Key { usage, down: false });
            }
            if modifier && self.chord.replace(false) {
                let released = !self.released.get();
                self.released.set(released);
                self.app.borrow_mut().say(if released { "System shortcuts are this PC's again. Ctrl+Alt+Shift sends them to the computer" } else { "System shortcuts go to the computer. Ctrl+Alt+Shift gives them back to this PC" });
            }
        }
    }

    pub(crate) fn release_keys(&self) {
        let held: Vec<u16> = self.held.borrow_mut().drain().collect();
        for usage in held {
            self.send(Event::Key { usage, down: false });
        }
        let buttons = self.buttons.replace(0);
        for (bit, button) in [(1, Button::Left), (2, Button::Right), (4, Button::Middle)] {
            if buttons & bit != 0 {
                self.send(Event::Button { button, down: false });
            }
        }
        // And every finger the computer thinks is down.
        let fingers = self.fingers.borrow_mut().lift_all();
        for (slot, x, y) in fingers {
            say!("touch: up, slot {slot}, at {x:.3},{y:.3} (the picture lost the input)");
            self.send(Event::Touch { phase: remoteman_core::TouchPhase::Up, slot, x, y });
        }
        self.chord.set(false);
    }

    /// A finger on the panel: sent as a touch if that is wanted and the picture is the
    /// computer's, and whether it was.
    fn touch(&self, id: u32, phase: remoteman_core::TouchPhase, x: i32, y: i32) -> bool {
        use remoteman_core::TouchPhase;
        let wanted = FEATURES.touch && self.forwarding() && self.app.try_borrow().is_ok_and(|a| a.store.settings.touch);
        // A finger that went down as a touch is lifted as one, whatever has changed since.
        if !wanted && !(phase == TouchPhase::Up && self.fingers.borrow().has(id)) {
            return false;
        }
        let Some(slot) = self.fingers.borrow_mut().slot(id, phase) else { return true };
        let at = self.app.try_borrow().ok().and_then(|a| a.fraction(x as f32, y as f32));
        if at.is_none() && phase != TouchPhase::Up {
            return true;
        }
        let (fx, fy) = at.unwrap_or_else(|| self.fingers.borrow().last(slot));
        self.fingers.borrow_mut().place(slot, fx, fy);
        self.point_at(0);
        self.send(Event::Touch { phase, slot, x: fx, y: fy });
        match phase {
            TouchPhase::Down => say!("touch: down, slot {slot}, at {fx:.3},{fy:.3} (pointer {id})"),
            TouchPhase::Up => say!("touch: up, slot {slot}, at {fx:.3},{fy:.3} after {} moves", self.fingers.borrow().moves(slot)),
            TouchPhase::Move => {}
        }
        true
    }

    /// A key seen before Windows acts on it: whether it is the computer's (as the old window).
    fn grabs(&self, key: &KBDLLHOOKSTRUCT, down: bool) -> bool {
        const TAB: u32 = 0x09;
        const ESCAPE: u32 = 0x1B;
        const LWIN: u32 = 0x5B;
        const RWIN: u32 = 0x5C;
        let extended = key.flags.0 & LLKHF_EXTENDED.0 != 0 || matches!(key.vkCode, LWIN | RWIN);
        let Some(usage) = crate::keys::usage(key.scanCode as u16, extended) else { return false };
        if !down {
            let usage = self.app.try_borrow().map(|a| a.key_for(usage)).unwrap_or(usage);
            let ours = self.held.borrow().contains(&usage) && matches!(key.vkCode, TAB | ESCAPE | LWIN | RWIN);
            if ours {
                self.key(key.scanCode as u16, extended, false);
                say!("shortcut: key {usage:#x} let go, sent to the computer");
            }
            return ours;
        }
        // SAFETY: plain queries.
        let (front, ctrl) = (self.front(), unsafe { GetAsyncKeyState(0x11) } < 0);
        let alt = key.flags.0 & LLKHF_ALTDOWN.0 != 0;
        let system = match key.vkCode {
            LWIN | RWIN => true,
            TAB => alt,
            ESCAPE => alt || ctrl,
            _ => false,
        };
        if !system || !front || self.released.get() || !self.app.try_borrow().map(|a| a.forwarding() && a.store.settings.shortcuts).unwrap_or(false) {
            return false;
        }
        self.key(key.scanCode as u16, extended, true);
        true
    }

    /// Hold the pointer in the panel and send its raw movement, or let it go.
    pub(crate) fn hold_pointer(&self, hold: bool) {
        use windows::Win32::UI::Input::{RAWINPUTDEVICE, RAWINPUTDEVICE_FLAGS, RIDEV_INPUTSINK, RIDEV_REMOVE, RegisterRawInputDevices};
        if self.captured.replace(hold) == hold {
            return;
        }
        // SAFETY: structures that live through each call.
        unsafe {
            let device = RAWINPUTDEVICE {
                usUsagePage: 1,
                usUsage: 2,
                dwFlags: if hold { RIDEV_INPUTSINK } else { RIDEV_REMOVE },
                hwndTarget: if hold { HWND(self.hwnd as *mut _) } else { HWND::default() },
            };
            let _ = RAWINPUTDEVICE_FLAGS(0);
            let _ = RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32);
            if hold {
                // The panel in the app's window, or the picture in full screen's.
                let full = self.full.borrow().as_ref().map(|f| f.hwnd);
                let ((x, y, w, h), window) = match full {
                    Some(hwnd) => (self.app.borrow().video_pixels().unwrap_or((0, 0, 1, 1)), hwnd),
                    None => (self.panel.get(), self.host.get()),
                };
                let mut at = POINT { x, y };
                let _ = windows::Win32::Graphics::Gdi::ClientToScreen(HWND(window as *mut _), &mut at);
                let _ = ClipCursor(Some(&RECT { left: at.x, top: at.y, right: at.x + w, bottom: at.y + h }));
            } else {
                let _ = ClipCursor(None);
            }
        }
        say!("pointer: {}", if hold { "captured: its raw movement goes to the computer" } else { "let go" });
        let mut app = self.app.borrow_mut();
        app.captured = hold;
        app.dirty = true;
    }

    /// This PC's clipboard changed: text, else a picture, goes to the computer.
    pub(crate) fn copied(&self) {
        // SAFETY: a plain query.
        if unsafe { GetClipboardSequenceNumber() } == self.own_copy.get() {
            return;
        }
        match crate::window::clipboard_get().filter(|t| !t.is_empty()) {
            Some(text) => self.app.borrow_mut().copied_here(text),
            None => {
                if let Some(png) = crate::window::clipboard_get_picture() {
                    self.app.borrow_mut().copied_picture(png);
                }
            }
        }
    }

    /// Take the host's newest cursor shape, if there is one.
    pub(crate) fn cursor_changed(&self) {
        let shape = self.app.borrow().session.as_ref().and_then(|s| s.shared.cursor.lock().unwrap().take());
        let Some(shape) = shape else { return };
        let made = crate::cursor::make(&shape);
        if shape.width == 0 {
            say!("cursor: the host has none now");
        } else if made.is_none() {
            say!("cursor: Windows wouldn't make a {}x{} cursor; none is shown", shape.width, shape.height);
        } else {
            say!("cursor: wearing the host's ({}x{}, hot spot {},{})", shape.width, shape.height, shape.hot_x, shape.hot_y);
        }
        {
            let mut app = self.app.borrow_mut();
            app.remote_cursor = (shape.width > 0).then(|| Arc::new(shape));
        }
        let old = self.cursor.replace(made);
        self.got_shape.set(true);
        crate::full::set_cursor(self);
        if let Some(old) = old {
            // The window may still be wearing it for a moment: it is let go a little later.
            let old = old.0 as isize;
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(2));
                // SAFETY: ours, and nobody's cursor by now.
                unsafe {
                    let _ = DestroyCursor(HCURSOR(old as *mut _));
                }
            });
        }
    }

    fn tick(&self) {
        {
            let mut app = self.app.borrow_mut();
            app.poll();
            app.tick();
        }
        self.cursor_changed();
        if self.watched.get().is_some_and(|t| t.elapsed() >= Duration::from_secs(1)) {
            self.watch_monitor();
        }
        self.decide_refresh();
        // A captured pointer: while the app's window has the keyboard and the picture is the
        // computer's, and Ctrl+Alt+Shift hasn't given it back.
        // SAFETY: a plain query.
        let front = self.front();
        let want = front && !self.released.get() && self.app.borrow().forwarding() && self.app.borrow().store.settings.capture && self.panel.get().2 > 0;
        self.hold_pointer(want);
        if self.deadline.get().is_some_and(|d| Instant::now() >= d) {
            self.deadline.set(None);
            self.app.borrow_mut().time_up();
        }
        self.publish();
        self.report_busy();
    }

    /// Do what the app's state asks of the engine itself, keep what is the app's, and tell the
    /// app if what it shows has changed.
    pub(crate) fn publish(&self) {
        let began = Instant::now();
        let told = self.publish_now();
        let took = began.elapsed().as_micros() as u64;
        let mut busy = self.busy.borrow_mut();
        busy.publishes += 1;
        busy.publish_us += took;
        busy.longest_us = busy.longest_us.max(took);
        busy.told += told as u32;
    }

    /// Every five seconds in a session while the pointer has moved: what `Busy` counted.
    fn report_busy(&self) {
        let mut busy = self.busy.borrow_mut();
        let since = *busy.since.get_or_insert_with(Instant::now);
        if since.elapsed() < Duration::from_secs(5) {
            return;
        }
        let wanted = self.app.try_borrow().is_ok_and(|a| a.session.is_some() && (a.args.stats || a.scene.measure.load(Ordering::Relaxed)));
        if wanted && busy.pointers > 0 {
            say!(
                "input: {} pointer moves sent in {:.1} s ({}); the state looked at {} times, {:.1} ms in all, {:.2} at most; the app told {} times",
                busy.pointers,
                since.elapsed().as_secs_f64(),
                if self.full.borrow().is_some() { "full screen" } else { "in the window" },
                busy.publishes,
                busy.publish_us as f64 / 1000.0,
                busy.longest_us as f64 / 1000.0,
                busy.told
            );
        }
        *busy = Busy { since: Some(Instant::now()), ..Busy::default() };
    }

    /// `publish`'s work; whether the app was told.
    fn publish_now(&self) -> bool {
        let full = self.full.borrow().is_some();
        let effects = {
            let Ok(mut app) = self.app.try_borrow_mut() else { return false };
            if full {
                // In full screen the engine's own window shows the kit's bar, menus and news.
                app.finish()
            } else {
                // Nothing is drawn by the kit here: what it would draw is the app's to show.
                app.dirty = false;
                std::mem::take(&mut app.effects)
            }
        };
        for effect in effects {
            self.apply(effect);
        }
        let changed = self.state_value().is_some_and(|v| *self.told.borrow() != v.to_string());
        let tell = changed || !self.effects.borrow().is_empty();
        if tell {
            (self.notify)();
        }
        tell
    }

    pub(crate) fn apply(&self, effect: Effect) {
        // What a file dialog belongs to: the app's window, or full screen's.
        let host = HWND(self.full.borrow().as_ref().map_or(self.host.get(), |f| f.hwnd) as *mut _);
        let out = |v: Value| self.effects.borrow_mut().push(v);
        match effect {
            Effect::Minimise => out(json!({"do": "minimise"})),
            Effect::Maximise => out(json!({"do": "maximise"})),
            Effect::Close => out(json!({"do": "close"})),
            Effect::Quit => out(json!({"do": "quit", "code": self.app.borrow().code.unwrap_or(0)})),
            Effect::Fullscreen(on) => {
                // Full screen is a window of the engine's own over the monitor (`full.rs`),
                // where the monitor can vary its refresh; the app's window waits behind it.
                if on {
                    if let Err(e) = crate::full::enter(self) {
                        say!("full screen: no window of its own: {e:#}");
                        self.app.borrow_mut().set_fullscreen(false);
                    }
                } else {
                    crate::full::leave(self, true);
                }
                out(json!({"do": "fullscreen", "on": self.full.borrow().is_some()}));
            }
            Effect::Title(title) => {
                crate::full::title(self, &title);
                out(json!({"do": "title", "text": title}));
            }
            Effect::Size(w, h) => out(json!({"do": "size", "width": w, "height": h})),
            Effect::ReleaseKeys => self.release_keys(),
            Effect::ForgetCursor => {
                // Let go a little later, as in `cursor_changed`: a window may still wear it.
                if let Some(old) = self.cursor.take() {
                    let old = old.0 as isize;
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(2));
                        // SAFETY: ours, and nobody's cursor by now.
                        unsafe {
                            let _ = DestroyCursor(HCURSOR(old as *mut _));
                        }
                    });
                }
                self.got_shape.set(false);
            }
            Effect::SetClipboard(text) => {
                crate::window::clipboard_set(&text);
                // SAFETY: a plain query.
                self.own_copy.set(unsafe { GetClipboardSequenceNumber() });
            }
            Effect::SetClipboardPicture(png) => {
                if crate::window::clipboard_set_picture(&png) {
                    // SAFETY: a plain query.
                    self.own_copy.set(unsafe { GetClipboardSequenceNumber() });
                }
            }
            Effect::ReadClipboard => self.copied(),
            Effect::PickFile => {
                self.release_keys();
                if let Some(path) = crate::window::pick_file(host) {
                    self.app.borrow_mut().send_file(&path);
                }
            }
            Effect::PickFolder => {
                if let Some(path) = crate::window::pick_folder(host) {
                    self.app.borrow_mut().set_folder(&path);
                }
            }
            Effect::OpenSecond(title) => {
                let shared = self.app.borrow().session.as_ref().and_then(|s| s.shared.second.clone());
                if let Some(shared) = shared {
                    if let Err(e) = self.open_second(shared, &title) {
                        say!("second display: no window for it: {e:#}");
                    }
                }
            }
            Effect::CloseSecond => self.close_second(),
        }
    }

    // The second display's window: the old program's, plain Win32.

    fn open_second(&self, shared: Arc<Shared>, title: &str) -> Result<()> {
        let gpu = self.app.borrow().gpu.clone();
        // SAFETY: Win32 calls with structures that live through each call.
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(second_procedure),
                hInstance: instance.into(),
                lpszClassName: w!("remoteman-engine-second"),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassW(&class);
            let mut r = RECT { left: 0, top: 0, right: 1280, bottom: 720 };
            let _ = AdjustWindowRectEx(&mut r, WS_OVERLAPPEDWINDOW, false, WS_EX_NOREDIRECTIONBITMAP);
            let hwnd = CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP, w!("remoteman-engine-second"), &windows::core::HSTRING::from(title), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, r.right - r.left, r.bottom - r.top, None, None, Some(instance.into()), None).context("create the second window")?;
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            let scene = shared.scene.clone();
            scene.quit.store(false, Ordering::Relaxed);
            scene.client_size.store(pack(client.right.max(1) as u32, client.bottom.max(1) as u32), Ordering::Relaxed);
            *scene.session.lock().unwrap() = Some(shared.clone());
            let drawing = crate::draw::start(gpu, scene, hwnd, None)?;
            *self.second.borrow_mut() = Some((hwnd.0 as isize, drawing, shared));
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        Ok(())
    }

    fn close_second(&self) {
        let Some((hwnd, drawing, shared)) = self.second.borrow_mut().take() else { return };
        shared.scene.quit.store(true, Ordering::Relaxed);
        shared.scene.poke();
        let _ = drawing.join();
        *shared.scene.session.lock().unwrap() = None;
        // SAFETY: our own window.
        unsafe {
            let _ = DestroyWindow(HWND(hwnd as *mut _));
        }
        self.point_at(0);
    }

    // What people did that isn't the picture's.

    /// `{"do": WHAT, ...}`. Text typed into a field comes with the command it is for.
    fn command(&self, v: &Value) -> bool {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
        let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        let what = s("do");
        let mut app = self.app.borrow_mut();
        let field = |app: &mut App, name: &str, text: &str| {
            app.fields.insert(id(name), Field::with(text));
        };
        let msg = match what.as_str() {
            "back" => Msg::Back,
            "pair" => Msg::Pair(v.get("address").and_then(Value::as_str).map(str::to_owned)),
            "pair-go" => {
                field(&mut app, "pair.address", &s("address"));
                Msg::DialogOk
            }
            "ok" => Msg::DialogOk,
            "cancel" => Msg::DialogCancel,
            "connect" => Msg::Connect(s("address")),
            "cancel-connect" => Msg::CancelConnect,
            "settings" => Msg::OpenSettings,
            "computer" => Msg::OpenComputer(s("address")),
            "rename" => {
                // The name typed in a computer's settings, kept as the old page keeps it.
                let address = s("address");
                let name = s("name").trim().to_owned();
                if !name.is_empty() {
                    app.store.update(&address, |c| c.name = name.clone());
                    if let Some(sess) = app.session.as_mut().filter(|x| x.target.address == address) {
                        sess.target.name = name;
                    }
                }
                return true;
            }
            "edit-address" => Msg::EditAddress(s("address")),
            "address-go" => {
                field(&mut app, "address.new", &s("address"));
                Msg::DialogOk
            }
            "forget" => Msg::AskForget(s("address")),
            "about" => Msg::About,
            "disconnect" => Msg::Disconnect,
            "keys" => {
                // A chord from the Send keys menu, by its name.
                let Some(codes) = chord(&s("keys")) else { return false };
                Msg::SendKeys(codes)
            }
            "place" => match PLACEMENT_NAMES.iter().find(|p| placement_key(p.0) == s("placement")) {
                Some(p) => Msg::Place(p.0),
                None => return false,
            },
            "pick" => match [Quality::Automatic, Quality::Sharp, Quality::Hevc, Quality::Av1].into_iter().find(|q| q.key() == s("quality")) {
                Some(q) => Msg::Pick(q),
                None => return false,
            },
            "bitrate" => {
                // A step of the slider, 0 to 11.
                let steps = (crate::app::BITRATES.len() - 1) as f32;
                Msg::Slide(0, n("step") as f32 / steps)
            }
            "figures" => Msg::ToggleFigures,
            "fullscreen" => Msg::ToggleFullscreen,
            "now-fullscreen" => {
                // The app's window went into full screen, or out of it.
                app.set_fullscreen(b("on"));
                return true;
            }
            "bar-mode" => Msg::BarMode(b("hover")),
            "start-fullscreen" => Msg::StartFullscreen,
            "choose" => {
                let address = s("address");
                let choice = match s("choice").as_str() {
                    "controls" => Choice::Controls,
                    "default-quality" => Choice::DefaultQuality,
                    "scroll" => Choice::Scroll,
                    "pointer" => Choice::Pointer,
                    "placement" => Choice::Placement(address),
                    "quality" => Choice::Quality(address),
                    "bitrate" => Choice::Bitrate(address),
                    "size" => Choice::Size(address),
                    "side" => Choice::Side(address),
                    _ => return false,
                };
                Msg::Choose(choice, n("index"))
            }
            "toggle" => {
                let flag = match s("flag").as_str() {
                    "match-refresh" => Flag::MatchRefresh,
                    "ctrl-as-command" => Flag::CtrlAsCommand,
                    "shortcuts" => Flag::Shortcuts,
                    "sound" => Flag::Sound,
                    "clipboard" => Flag::Clipboard,
                    "hdr" => Flag::Hdr,
                    "variable-refresh" => Flag::VariableRefresh,
                    _ => return false,
                };
                Msg::Toggle(flag, v.get("address").and_then(Value::as_str).map(str::to_owned))
            }
            "sound" => Msg::ToggleSound,
            "clipboard" => Msg::ToggleClipboard,
            "second" => Msg::ToggleSecond,
            "send-file" => Msg::SendFile,
            "unlock" => {
                field(&mut app, "locked.password", &s("password"));
                app.remember = b("remember");
                Msg::Unlock
            }
            "pick-folder" => Msg::PickFolder,
            "dark" => {
                app.set_dark(b("on"));
                return true;
            }
            // Finding, waking, the microphone, the camera and touch (`nearby.rs`, `media.rs`).
            // `on` is what the switch is to be; without it the switch flips.
            "wake" => {
                app.wake(&s("address"));
                return true;
            }
            "set" => {
                let on = v.get("on").and_then(Value::as_bool);
                return app.set_option(&s("what"), on, v.get("address").and_then(Value::as_str));
            }
            "mic" => {
                let on = v.get("on").and_then(Value::as_bool).unwrap_or_else(|| !app.session.as_ref().is_some_and(|x| x.mic_on));
                app.set_microphone(on);
                return true;
            }
            "camera" => {
                let on = v.get("on").and_then(Value::as_bool).unwrap_or_else(|| !app.session.as_ref().is_some_and(|x| x.camera_on));
                app.set_camera(on);
                return true;
            }
            // For tests (`--ui`, which keeps the list made up and the network out): one of the
            // old program's walk steps (`walk.rs`), the app put into that screen or state.
            "walk" if app.args.ui.is_some() => {
                crate::walk::apply(&mut app, &s("step"));
                say!("walk: {}", s("step"));
                return true;
            }
            // For tests too: a picture of what the draw thread shows now (the picture and, in
            // full screen, the bar over it), saved once it has drawn the window as it now is.
            "shot" if app.args.ui.is_some() => {
                let serial = app.build();
                *app.scene.shot.lock().unwrap() = Some((s("path").into(), serial));
                app.scene.poke();
                return true;
            }
            other => {
                say!("a command nobody knows: {other}");
                return false;
            }
        };
        app.update(msg);
        true
    }

    // The state.

    /// The state without what the app has to do (`None` while the app is busy).
    fn state_value(&self) -> Option<Value> {
        self.app.try_borrow().ok().map(|app| state_of(&app))
    }
}

/// The chords of the Send keys menu, by name (Linux key codes, as the old menu sends them).
fn chord(name: &str) -> Option<&'static [u16]> {
    // The old menu's list (`menus.rs`).
    Some(match name {
        "ctrl-alt-del" => &[29, 56, 111],
        "win" => &[125],
        "win-l" => &[125, 38],
        "alt-tab" => &[56, 15],
        "alt-f4" => &[56, 62],
        "ctrl-shift-esc" => &[29, 42, 1],
        "print" => &[99],
        _ => return None,
    })
}

fn reach_word(r: Option<Reach>) -> &'static str {
    match r {
        Some(Reach::Online) => "online",
        Some(Reach::Asleep) => "asleep",
        Some(Reach::Unreachable) => "unreachable",
        None => "",
    }
}

fn problem(p: &Problem) -> Value {
    let (kind, said) = match p {
        Problem::Unreachable(d) => ("unreachable", d.as_str()),
        Problem::Asleep => ("asleep", ""),
        Problem::NotPaired => ("not-paired", ""),
        Problem::Changed => ("changed", ""),
        Problem::Refused(s) => ("refused", s.as_str()),
        Problem::LoggingIn(s) => ("logging-in", s.as_str()),
        Problem::Locked(s) => ("locked", s.as_str()),
        Problem::Ended(s) => ("ended", s.as_str()),
    };
    json!({"kind": kind, "said": said})
}

fn options(app: &App, choice: &Choice) -> Value {
    let (list, at) = app.options(choice);
    json!({"items": list.iter().map(|(t, on)| json!({"text": t, "on": on})).collect::<Vec<_>>(), "at": at})
}

/// Everything the app shows, as it is now.
fn state_of(app: &App) -> Value {
    let screen = match &app.screen {
        Screen::Computers => json!({"kind": "computers"}),
        Screen::Connecting => json!({"kind": "connecting", "name": app.attempt.as_ref().map(|a| a.target.name.clone()), "address": app.attempt.as_ref().map(|a| a.target.address.clone())}),
        Screen::Problem(t, p) => json!({"kind": "problem", "name": t.name, "address": t.address, "problem": problem(p), "retrying": app.retry_at.is_some()}),
        Screen::Session => json!({"kind": "session"}),
        Screen::Settings => json!({"kind": "settings"}),
        Screen::Computer(a) => json!({"kind": "computer", "address": a}),
    };
    let dialog = app.dialog.as_ref().map(|d| match d {
        // (`address`: what the dialog opens with, as from Nearby's "Pair…".)
        Dialog::Pair { waiting, problem } => json!({"kind": "pair", "waiting": waiting, "problem": problem, "address": app.fields.get(&id("pair.address")).map(|f| f.string()).unwrap_or_default()}),
        Dialog::Code { name, code, until } => json!({"kind": "code", "name": name, "code": code, "seconds": until.saturating_duration_since(Instant::now()).as_secs()}),
        Dialog::Paired { target, key } => json!({"kind": "paired", "name": target.name, "address": target.address, "key": key}),
        Dialog::Forget(a) => json!({"kind": "forget", "address": a, "name": app.name_of(a)}),
        Dialog::Address { address, problem } => json!({"kind": "address", "address": address, "problem": problem}),
        Dialog::About => json!({"kind": "about", "version": env!("CARGO_PKG_VERSION")}),
    });
    let computers: Vec<Value> = app
        .store
        .computers
        .iter()
        .map(|c| {
            let reach = app.reach_of(&c.address);
            json!({
                "address": c.address, "short": crate::store::short(&c.address), "name": c.name, "os": c.os, "os_name": os_name(&c.os),
                "last": if c.used > 0 { when(c.used) } else { String::new() },
                "paired": if c.paired > 0 { when(c.paired) } else { String::new() },
                "reach": reach_word(reach),
                "key": app.store.key(&c.address).map(|k| key_words(&k, 4)).unwrap_or_default(),
                "placement": placement_key(c.placement), "sound": c.sound, "clipboard": c.clipboard,
                "connected": app.session.as_ref().is_some_and(|s| s.target.address == c.address),
                // Seen on this network now; may be sent the waking packet; "Wake it over the
                // network" in its settings; the network cards it is woken through.
                "seen": app.seen(&c.address).is_some(), "wakeable": app.wakeable(&c.address), "wake": c.wake, "cards": c.cards,
            })
        })
        .collect();
    // Computers running remoteman on this network that this PC isn't paired with.
    let nearby: Vec<Value> = app.sightings.unpaired.iter().map(|f| json!({"name": f.name, "address": f.address, "short": crate::store::short(&f.address), "os": f.os, "os_name": os_name(&f.os)})).collect();
    let st = &app.store.settings;
    let settings = json!({
        "match_refresh": st.match_refresh, "ctrl_as_command": st.ctrl_as_command, "shortcuts": st.shortcuts,
        "sound": st.sound, "clipboard": st.clipboard, "figures": st.figures, "fullscreen": st.fullscreen,
        "hover_bar": st.hover_bar, "capture": st.capture, "scroll_reversed": st.scroll_reversed,
        "hdr": st.hdr, "variable_refresh": st.variable_refresh,
        "folder": app.store.receive_folder().display().to_string(),
        "controls": options(app, &Choice::Controls), "default_quality": options(app, &Choice::DefaultQuality),
        "scroll": options(app, &Choice::Scroll), "pointer": options(app, &Choice::Pointer),
        "touch": st.touch, "allow_microphone": st.allow_microphone, "allow_camera": st.allow_camera,
    });
    // The drop-downs of one computer: the one whose page is open, or the session's.
    let address = match &app.screen {
        Screen::Computer(a) => Some(a.clone()),
        _ => app.session.as_ref().map(|s| s.target.address.clone()),
    };
    let computer = address.map(|a| {
        json!({
            "address": a,
            "placement": options(app, &Choice::Placement(a.clone())), "quality": options(app, &Choice::Quality(a.clone())),
            "bitrate": options(app, &Choice::Bitrate(a.clone())), "size": options(app, &Choice::Size(a.clone())),
            "side": options(app, &Choice::Side(a.clone())),
        })
    });
    let session = app.session.as_ref().map(|s| {
        let f = &app.figures;
        json!({
            "address": s.target.address, "name": s.target.name, "os": s.os, "features": s.features,
            "placement": placement_key(s.display.placement), "quality": s.quality.key(), "bitrate": s.bitrate,
            "bitrate_step": crate::app::BITRATES.iter().position(|b| *b == s.bitrate).unwrap_or(crate::app::BITRATES.len() - 1),
            "sound": s.sound_on, "clipboard": s.clipboard_on, "second": s.second_on,
            "format": app.format().map(|(w, h, hz)| json!([w, h, hz])),
            "shown": s.shared.presented_total.load(Ordering::Relaxed) > 0,
            "figures": {"shown": f.shown, "decoded": f.decoded, "decode_ms": f.decode_ms, "age_ms": f.age_ms, "round_trip_ms": f.round_trip_ms, "megabits": f.megabits, "lost": f.lost, "replaced": f.replaced, "decoder": f.decoder},
        })
    });
    // With this PC's microphone and camera as lent to the computer (`media.rs`): `mic`,
    // `mic_can`, `mic_why`, `mic_device`, `camera`, `camera_can`, `camera_why`.
    let session = session.map(|mut v| {
        if let (Some(o), serde_json::Value::Object(m)) = (v.as_object_mut(), crate::media::session_state(app)) {
            o.extend(m);
        }
        v
    });
    let decodes = |word: &str| app.decoders.iter().any(|m| format!("{:?}", m.codec).eq_ignore_ascii_case(word) && m.chroma == remoteman_core::Chroma::C420);
    let picture = picture_state(app);
    json!({
        "picture": picture,
        "screen": screen,
        "dialog": dialog,
        "computers": computers,
        "nearby": nearby,
        "settings": settings,
        "computer": computer,
        "session": session,
        "fullscreen": app.fullscreen,
        "monitor": [app.monitor.0, app.monitor.1, app.monitor.2],
        "bitrates": crate::app::BITRATES,
        "captured": app.captured,
        "toast": app.toast.as_ref().map(|t| t.0.clone()),
        "remember": app.remember,
        "decodes": {"av1": decodes("av1"), "hevc": decodes("hevc"), "hevc444": app.decoders.iter().any(|m| m.chroma == remoteman_core::Chroma::C444)},
        "features": {
            "discovery": FEATURES.discovery, "presence": FEATURES.presence, "wake": FEATURES.wake, "pairing_code": FEATURES.pairing_code,
            "sound": FEATURES.sound, "microphone": FEATURES.microphone, "camera": FEATURES.camera, "clipboard": FEATURES.clipboard,
            "files": FEATURES.files, "folder_choice": FEATURES.folder_choice, "hdr": FEATURES.hdr, "variable_refresh": FEATURES.variable_refresh,
            "refresh_choice": FEATURES.refresh_choice, "follow_window": FEATURES.follow_window, "sharp_text": FEATURES.sharp_text,
            "pointer_capture": FEATURES.pointer_capture, "ctrl_as_command": FEATURES.ctrl_as_command, "scroll_direction": FEATURES.scroll_direction,
            "touch": FEATURES.touch, "system_shortcuts": FEATURES.system_shortcuts, "key_mapping": FEATURES.key_mapping,
            "release_shortcut": FEATURES.release_shortcut, "second_display": FEATURES.second_display, "mirror": FEATURES.mirror, "way_in": FEATURES.way_in,
        },
        "not_yet": crate::features::NOT_YET,
    })
}

/// HDR, variable refresh and 4:4:4 as they stand: the switches, what this monitor and this PC
/// can do, and what the picture is doing now, with why where something is off.
fn picture_state(app: &App) -> Value {
    let (scene, st) = (&app.scene, &app.store.settings);
    let session = app.session.as_ref();
    let offers = |f: &str| session.is_some_and(|s| s.offers(f));
    let shown = match scene.hdr_shown.load(Ordering::Relaxed) {
        1 => "passed-through",
        2 => "tone-mapped",
        _ => "sdr",
    };
    let screen = scene.screen_hdr.load(Ordering::Relaxed);
    let hdr_why = if !st.hdr {
        "Off"
    } else if !screen {
        "This monitor isn't in HDR"
    } else if session.is_some() && !offers("hdr") {
        "This computer doesn't send HDR"
    } else {
        "Used"
    };
    let range = *scene.vrr_range.lock().unwrap();
    let tearing_allowed = scene.tearing_allowed.load(Ordering::Relaxed);
    let (vrr_active, vrr_why) = if FEATURES.variable_refresh { crate::monitor::variable_refresh(st.variable_refresh, app.fullscreen, range, tearing_allowed) } else { (false, crate::features::NOT_YET) };
    // 4:4:4: where it is decoded, how fast the processor is taken to be, and whether it is in
    // what Automatic offers for the display this window would ask for now.
    let full = app.decoders.iter().find(|m| m.chroma == remoteman_core::Chroma::C444);
    let address = session.map(|s| s.target.address.clone());
    let placement = session.map_or(app.store.settings.placement, |s| s.display.placement);
    let display = app.display_for(address.as_deref(), placement);
    let (rate, measured) = app.cpu_444_rate();
    let withheld = app.withholds_444(&display, Quality::Automatic);
    json!({
        "hdr": {
            "on": st.hdr, "screen": screen, "headroom": f32::from_bits(scene.headroom.load(Ordering::Relaxed)),
            "offered": offers("hdr"), "asked": session.is_some_and(|s| s.display.hdr),
            "stream": session.is_some_and(|s| s.shared.hdr.load(Ordering::Relaxed)), "shown": shown, "why": hdr_why,
        },
        "variable_refresh": {
            "on": st.variable_refresh, "range": range.map(|(lo, hi)| json!([lo, hi])), "tearing_allowed": tearing_allowed,
            "active": vrr_active, "why": vrr_why,
        },
        "sharp_text": {
            "decoder": match full { None => "none", Some(m) if crate::decode::mode_in_software(*m) => "processor", Some(_) => "card" },
            "rate": rate, "measured": measured,
            "display": [display.width, display.height, display.refresh],
            "offered": full.is_some() && withheld.is_none(), "why": withheld.unwrap_or_default(),
        },
    })
}

// The engine's window.

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: passing the message on as it came.
    let pass = || unsafe { DefWindowProcW(hwnd, msg, w, l) };
    let Some(e) = engine() else { return pass() };
    match msg {
        WM_TIMER => e.tick(),
        WM_WAKE => {
            if let Ok(mut app) = e.app.try_borrow_mut() {
                app.poll();
            }
            e.cursor_changed();
            e.publish();
        }
        WM_CLIPBOARDUPDATE => {
            if e.app.try_borrow().map(|a| a.session.as_ref().is_some_and(|s| s.clipboard_on)).unwrap_or(false) {
                e.copied();
                e.publish();
            }
        }
        WM_INPUT => {
            use windows::Win32::UI::Input::{GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTHEADER, RID_INPUT, RIM_TYPEMOUSE};
            let mut input = RAWINPUT::default();
            let mut size = size_of::<RAWINPUT>() as u32;
            // SAFETY: the buffer is as large as said.
            let got = unsafe { GetRawInputData(HRAWINPUT(l.0 as *mut _), RID_INPUT, Some(&mut input as *mut _ as *mut _), &mut size, size_of::<RAWINPUTHEADER>() as u32) };
            if e.captured.get() && got != u32::MAX && input.header.dwType == RIM_TYPEMOUSE.0 {
                // SAFETY: a mouse's input, as its header says.
                let mouse = unsafe { input.data.mouse };
                if mouse.usFlags.0 & 1 == 0 && (mouse.lLastX != 0 || mouse.lLastY != 0) {
                    e.point_at(0);
                    e.send(Event::PointerBy { dx: mouse.lLastX as f32, dy: mouse.lLastY as f32 });
                }
            }
            return pass();
        }
        _ => return pass(),
    }
    LRESULT(0)
}

unsafe extern "system" fn keyboard_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for this hook the parameter points at the key's description.
        let key = unsafe { &*(l.0 as *const KBDLLHOOKSTRUCT) };
        let down = matches!(w.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if engine().is_some_and(|e| e.grabs(key, down)) {
            if let Some(e) = engine() {
                e.publish();
            }
            return LRESULT(1);
        }
    }
    // SAFETY: passing the key on as it came.
    unsafe { CallNextHookEx(None, code, w, l) }
}

/// The second display's window: everything done in it is the computer's, on that display.
unsafe extern "system" fn second_procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: passing the message on as it came.
    let pass = || unsafe { DefWindowProcW(hwnd, msg, w, l) };
    let Some(e) = engine() else { return pass() };
    let Some(shared) = e.second.try_borrow().ok().and_then(|s| s.as_ref().filter(|s| s.0 == hwnd.0 as isize).map(|s| s.2.clone())) else { return pass() };
    let point = |l: LPARAM| ((l.0 & 0xFFFF) as i16 as i32, ((l.0 >> 16) & 0xFFFF) as i16 as i32);
    let pointer = |l: LPARAM| {
        let (x, y) = point(l);
        let (vw, vh) = unpack(shared.stream_size.load(Ordering::Relaxed));
        let (cw, ch) = unpack(shared.scene.client_size.load(Ordering::Relaxed));
        if vw == 0 || vh == 0 || cw == 0 || ch == 0 {
            return;
        }
        let (vw, vh, cw, ch) = (vw as f32, vh as f32, cw as f32, ch as f32);
        let fit = (cw / vw).min(ch / vh);
        let (dw, dh) = (vw * fit, vh * fit);
        e.point_at(1);
        e.send(Event::PointerTo { x: ((x as f32 - ((cw - dw) / 2.0).floor()) / dw).clamp(0.0, 1.0), y: ((y as f32 - ((ch - dh) / 2.0).floor()) / dh).clamp(0.0, 1.0) });
    };
    let button = |button: Button, down: bool| {
        pointer(l);
        e.send(Event::Button { button, down });
        // SAFETY: capture for our own window, so the release comes here.
        unsafe {
            if down {
                SetCapture(hwnd);
            } else {
                let _ = ReleaseCapture();
            }
        }
    };
    // SAFETY: Win32 calls on our own window.
    unsafe {
        use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW};
        match msg {
            WM_MOUSEMOVE => pointer(l),
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => button(Button::Left, true),
            WM_LBUTTONUP => button(Button::Left, false),
            WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => button(Button::Right, true),
            WM_RBUTTONUP => button(Button::Right, false),
            WM_MBUTTONDOWN | WM_MBUTTONDBLCLK => button(Button::Middle, true),
            WM_MBUTTONUP => button(Button::Middle, false),
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let notches = ((w.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                let k = if e.app.borrow().store.settings.scroll_reversed { -NOTCH } else { NOTCH };
                e.point_at(1);
                e.send(if msg == WM_MOUSEHWHEEL { Event::Scroll { dx: notches * k, dy: 0.0 } } else { Event::Scroll { dx: 0.0, dy: -notches * k } });
            }
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
                let mut scancode = ((l.0 >> 16) & 0xFF) as u16;
                if scancode == 0 {
                    scancode = MapVirtualKeyW(w.0 as u32, MAPVK_VK_TO_VSC) as u16;
                }
                e.key(scancode, l.0 & (1 << 24) != 0, matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN));
            }
            WM_SYSCHAR => {}
            WM_KILLFOCUS => e.release_keys(),
            WM_SIZE => {
                let (cw, ch) = ((l.0 & 0xFFFF) as u32, ((l.0 >> 16) & 0xFFFF) as u32);
                if cw > 0 && ch > 0 {
                    shared.scene.client_size.store(pack(cw, ch), Ordering::Relaxed);
                    shared.scene.poke();
                }
            }
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                let _ = windows::Win32::Graphics::Gdi::ValidateRect(Some(hwnd), None);
                shared.scene.poke();
            }
            WM_CLOSE => {
                e.app.borrow_mut().set_second(false, true);
                e.publish();
            }
            _ => return pass(),
        }
    }
    LRESULT(0)
}
