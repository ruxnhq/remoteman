//! The engine's C interface, for the WinUI 3 app (`windows/viewer/Engine.cs` is its other
//! side). Every call comes on the app's thread, the one that owns its window.
//!
//! The engine drives the same [`App`] the old program does: what is on screen, what each
//! press does and the sessions are its, unchanged; the app shows its state and hands it what
//! people do. Between them:
//! - `remoteman_state` gives the whole state as one JSON document (UTF-16). When it has
//!   changed, the engine calls the app's `notify` (on the app's thread, from inside one of
//!   these calls or from the engine's own window's messages);
//! - `remoteman_command` takes what people did that isn't the picture's, as JSON (`command`);
//! - the picture's input comes through typed calls (`remoteman_pointer` and the rest), in the
//!   panel's pixels.
//!
//! The engine has a window of its own that nobody sees (a message-only window on the app's
//! thread): other threads wake it, it ticks sixty times a second, it hears the clipboard
//! change and, while the pointer is captured, the mouse's raw movement. The keyboard hook that
//! takes the system's shortcuts during a session is the old program's, on the same thread.

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
use windows::core::{GUID, HRESULT, IUnknown, Interface, w};

use crate::app::{App, Choice, Dialog, Effect, Flag, Msg, Screen};
use crate::features::FEATURES;
use crate::status::{Problem, Reach};
use crate::store::{PLACEMENT_NAMES, Quality, key_words, os_name, placement_key, when};
use crate::stream::{Scene, Shared, pack, pack_scale, unpack};
use crate::ui::{Field, id};

const WM_WAKE: u32 = WM_APP + 1;
/// The wheel's notch in the protocol's pixels, as the old window has it.
const NOTCH: f32 = 40.0;

/// WinUI 3's `ISwapChainPanelNative` (microsoft.ui.xaml.media.dxinterop.h): IUnknown's three
/// and then `SetSwapChain(IDXGISwapChain*)`.
const SWAP_CHAIN_PANEL_NATIVE: GUID = GUID::from_u128(0x63aad0b8_7c24_40ff_85a8_640d944cc325);

type Notify = extern "system" fn();

struct Engine {
    app: RefCell<App>,
    hwnd: isize,
    /// The app's window, and the panel's place in it: x, y, width, height in pixels.
    host: Cell<isize>,
    panel: Cell<(i32, i32, i32, i32)>,
    notify: Notify,
    /// The state as last handed out, and as last told about.
    text: RefCell<Vec<u16>>,
    told: RefCell<String>,
    /// What only the app can do, until it has read the state.
    effects: RefCell<Vec<Value>>,
    drawing: RefCell<Option<JoinHandle<()>>>,
    deadline: Cell<Option<Instant>>,
    // Input, as the old window keeps it.
    held: RefCell<HashSet<u16>>,
    buttons: Cell<u8>,
    released: Cell<bool>,
    chord: Cell<bool>,
    captured: Cell<bool>,
    pointer_on: Cell<u8>,
    own_copy: Cell<u32>,
    hook: Cell<isize>,
    second: RefCell<Option<(isize, JoinHandle<()>, Arc<Shared>)>>,
    /// The host's cursor, made into a Windows one, for the app to wear over the picture.
    cursor: Cell<Option<HCURSOR>>,
    cursor_serial: Cell<u32>,
}

thread_local! {
    static ENGINE: RefCell<Option<Rc<Engine>>> = const { RefCell::new(None) };
    static ERROR: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
}

fn engine() -> Option<Rc<Engine>> {
    ENGINE.with(|e| e.borrow().clone())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A UTF-16 string the app handed over.
///
/// # Safety
/// `p` is null or points at a string that ends with a 0.
unsafe fn read(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: the caller's word that the string ends.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

// The C interface.

/// Start the engine: `args` are the command line's words, one a line (as the old program
/// takes them); `notify` is called on this thread when the state has changed. 0 when it has
/// started; otherwise `remoteman_error` says why.
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_start(args: *const u16, notify: Notify) -> i32 {
    // SAFETY: the app's word.
    let words: Vec<String> = unsafe { read(args) }.lines().map(str::to_owned).filter(|w| !w.is_empty()).collect();
    match start(words, notify) {
        Ok(()) => 0,
        Err(e) => {
            say!("failed: {e:#}");
            ERROR.with(|t| *t.borrow_mut() = wide(&format!("{e:#}")));
            1
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn remoteman_error() -> *const u16 {
    ERROR.with(|t| t.borrow().as_ptr())
}

/// The state as JSON, valid until the next call into the engine.
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_state() -> *const u16 {
    let Some(e) = engine() else { return std::ptr::null() };
    let Some(mut v) = e.state_value() else { return e.text.borrow().as_ptr() };
    *e.told.borrow_mut() = v.to_string();
    v["effects"] = Value::Array(e.effects.borrow_mut().drain(..).collect());
    *e.text.borrow_mut() = wide(&v.to_string());
    e.text.borrow().as_ptr()
}

/// Something people did, as JSON (see `Engine::command`). 0 when it was understood.
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_command(json: *const u16) -> i32 {
    let Some(e) = engine() else { return 1 };
    // SAFETY: the app's word.
    let text = unsafe { read(json) };
    let ok = match serde_json::from_str::<Value>(&text) {
        Ok(v) => e.command(&v),
        Err(err) => {
            say!("a command that isn't JSON: {err}");
            false
        }
    };
    e.publish();
    if ok { 0 } else { 1 }
}

/// Give the engine the app's `SwapChainPanel` (any interface of it) and the window it is in.
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_attach(panel: *mut c_void, window: isize) -> i32 {
    let Some(e) = engine() else { return 1 };
    match e.attach(panel, window) {
        Ok(()) => 0,
        Err(err) => {
            say!("the panel can't show pictures: {err:#}");
            ERROR.with(|t| *t.borrow_mut() = wide(&format!("{err:#}")));
            1
        }
    }
}

/// Where the panel is in the app's window and how large, in pixels, and its scale.
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_panel(x: i32, y: i32, width: i32, height: i32, scale_x: f32, scale_y: f32) {
    let Some(e) = engine() else { return };
    e.panel.set((x, y, width, height));
    e.resync(scale_x, scale_y);
    e.publish();
}

/// The pointer is at this place in the panel, in pixels.
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_pointer(x: i32, y: i32) {
    let Some(e) = engine() else { return };
    if !e.captured.get() && e.forwarding() {
        e.pointer(x, y);
    }
}

/// 0 left, 1 right, 2 middle.
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_button(button: i32, down: bool, x: i32, y: i32) {
    let Some(e) = engine() else { return };
    let (button, bit) = match button {
        1 => (Button::Right, 2),
        2 => (Button::Middle, 4),
        _ => (Button::Left, 1),
    };
    if !e.forwarding() {
        return;
    }
    if !e.captured.get() {
        e.pointer(x, y);
    }
    if down {
        e.send(Event::Button { button, down: true });
        e.buttons.set(e.buttons.get() | bit);
    } else if e.buttons.get() & bit != 0 {
        e.send(Event::Button { button, down: false });
        e.buttons.set(e.buttons.get() & !bit);
    }
}

/// The wheel turned by this many notches (positive away from the user, or to the right).
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_wheel(notches: f32, horizontal: bool) {
    let Some(e) = engine() else { return };
    if !e.forwarding() {
        return;
    }
    let k = if e.app.borrow().store.settings.scroll_reversed { -NOTCH } else { NOTCH };
    // The wheel counts away from the user positive; the protocol counts towards the end of
    // the document positive.
    e.send(if horizontal { Event::Scroll { dx: notches * k, dy: 0.0 } } else { Event::Scroll { dx: 0.0, dy: -notches * k } });
}

/// A key on the picture, by where it is on the keyboard (set 1 scancode and the E0 bit).
/// Returns whether it went to the computer (the app then does nothing else with it).
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_key(scancode: u32, extended: bool, down: bool, repeat: bool) -> bool {
    let Some(e) = engine() else { return false };
    const F11: u16 = 0x57;
    const I: u16 = 0x17;
    let session = e.forwarding();
    if !session {
        return false;
    }
    let held = |vk: i32| unsafe { GetKeyState(vk) } < 0;
    let scancode = scancode as u16;
    if down && scancode == F11 && !extended {
        // The window's own, as in the Linux viewer.
        if !repeat {
            e.app.borrow_mut().update(Msg::ToggleFullscreen);
            e.publish();
        }
        return true;
    }
    if down && scancode == I && held(0x10) && held(0x11) && held(0x12) {
        e.chord.set(false);
        if !repeat {
            e.app.borrow_mut().update(Msg::ToggleFigures);
            e.publish();
        }
        return true;
    }
    e.key(scancode, extended, down);
    e.publish();
    true
}

/// The panel got the keyboard, or lost it.
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_focus(focused: bool) {
    let Some(e) = engine() else { return };
    if !focused {
        e.hold_pointer(false);
        e.release_keys();
    }
}

/// The host's cursor as a Windows cursor (0: none to show; the app shows the arrow until the
/// host has said), and a number that changes when it does.
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_cursor(serial: *mut u32) -> isize {
    let Some(e) = engine() else { return 0 };
    if !serial.is_null() {
        // SAFETY: the app's pointer to its number.
        unsafe { *serial = e.cursor_serial.get() };
    }
    let gone = e.app.borrow().session.as_ref().is_some_and(|s| s.shared.cursor_gone.load(Ordering::Relaxed));
    if gone || e.captured.get() {
        return -1;
    }
    e.cursor.get().map_or(0, |c| c.0 as isize)
}

/// Leaving: nothing may be left connected. Returns what the process should end with.
#[unsafe(no_mangle)]
pub extern "system" fn remoteman_stop() -> i32 {
    let Some(e) = ENGINE.with(|x| x.borrow_mut().take()) else { return 2 };
    e.release_keys();
    e.close_second();
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
    scene.poke();
    if let Some(drawing) = e.drawing.borrow_mut().take() {
        let _ = drawing.join();
    }
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

// Starting.

fn start(words: Vec<String>, notify: Notify) -> Result<()> {
    let args = crate::args::parse_from(words)?;
    if let Some(path) = &args.log {
        crate::log::to_file(path)?;
    }
    let gpu = crate::gpu::Gpu::open()?;
    say!("engine: gpu {}", gpu.name);
    let mut decoders: Vec<Mode> = Vec::new();
    let forced = std::env::var("REMOTEMAN_SOFTWARE").as_deref() == Ok("1");
    for mode in &args.decoders {
        let on_card = if forced { Err(anyhow::anyhow!("REMOTEMAN_SOFTWARE=1")) } else { gpu.decodes(*mode) };
        match on_card {
            Ok(()) => decoders.push(*mode),
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
    say!("decoders offered: {}", decoders.iter().map(|m| format!("{:?}", m.codec).to_lowercase()).collect::<Vec<_>>().join(", "));
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
        notify,
        text: RefCell::new(Vec::new()),
        told: RefCell::new(String::new()),
        effects: RefCell::new(Vec::new()),
        drawing: RefCell::new(None),
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
        cursor_serial: Cell::new(0),
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
    fn attach(&self, panel: *mut c_void, window: isize) -> Result<()> {
        anyhow::ensure!(!panel.is_null(), "no panel");
        self.host.set(window);
        let (gpu, scene) = {
            let app = self.app.borrow();
            (app.gpu.clone(), app.scene.clone())
        };
        let (_, _, w, h) = self.panel.get();
        let swap = crate::draw::panel_chain(&gpu, w.max(8) as u32, h.max(8) as u32)?;
        // SAFETY: the app hands over a live interface of the panel, which is borrowed here
        // and not released; the swap chain is ours and the panel keeps its own reference.
        unsafe {
            let unknown = IUnknown::from_raw_borrowed(&panel).context("no panel")?;
            let mut native: *mut c_void = std::ptr::null_mut();
            unknown.query(&SWAP_CHAIN_PANEL_NATIVE, &mut native).ok().context("the panel has no ISwapChainPanelNative")?;
            type SetSwapChain = unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT;
            let vtable = *(native as *const *const usize);
            let set: SetSwapChain = std::mem::transmute(*vtable.add(3));
            let done = set(native, swap.as_raw());
            // (Release, IUnknown's third.)
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vtable.add(2));
            release(native);
            done.ok().context("give the panel its swap chain")?;
        }
        if self.drawing.borrow().is_none() {
            struct Send1(windows::Win32::Graphics::Dxgi::IDXGISwapChain1);
            // SAFETY: the swap chain is free-threaded; it is only used by the draw thread from
            // here on.
            unsafe impl Send for Send1 {}
            let swap = Send1(swap);
            let shot = self.app.borrow().args.shot.clone();
            let drawing = crate::draw::start_on(gpu, scene, move || {
                let swap = swap;
                crate::draw::Surface::Panel(swap.0)
            }, shot)?;
            *self.drawing.borrow_mut() = Some(drawing);
        }
        say!("engine: the panel shows the picture");
        // Now there is a window: the computer named on the command line, if one was.
        self.app.borrow_mut().start();
        self.publish();
        Ok(())
    }

    /// The panel's size or scale changed, or the monitor under it.
    fn resync(&self, sx: f32, sy: f32) {
        let (_, _, w, h) = self.panel.get();
        if w <= 0 || h <= 0 {
            return;
        }
        let host = HWND(self.host.get() as *mut _);
        let (mw, mh, hz, ..) = crate::window::monitor_of(host);
        let mut app = self.app.borrow_mut();
        app.scene.client_size.store(pack(w as u32, h as u32), Ordering::Relaxed);
        app.scene.panel_scale.store(pack_scale(sx, sy), Ordering::Relaxed);
        app.scene.poke();
        app.monitor = (mw, mh, hz);
        let maximised = unsafe { IsZoomed(host).as_bool() };
        app.resized(w as f32 / sx, h as f32 / sy, sx, maximised);
    }

    fn forwarding(&self) -> bool {
        self.app.try_borrow().map(|a| a.forwarding()).unwrap_or(false)
    }

    fn send(&self, event: Event) {
        if let Some(client) = self.app.try_borrow().ok().and_then(|a| a.client().cloned()) {
            client.send(event);
        }
    }

    fn point_at(&self, index: u8) {
        if self.pointer_on.replace(index) != index {
            self.send(Event::Display(index));
        }
    }

    fn pointer(&self, x: i32, y: i32) {
        self.point_at(0);
        let at = self.app.borrow().fraction(x as f32, y as f32);
        if let Some((x, y)) = at {
            self.send(Event::PointerTo { x, y });
        }
    }

    /// A key by where it is on the keyboard (as the old window's).
    fn key(&self, scancode: u16, extended: bool, down: bool) {
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

    fn release_keys(&self) {
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
        self.chord.set(false);
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
        let (front, ctrl) = unsafe { (GetForegroundWindow().0 as isize == self.host.get(), GetAsyncKeyState(0x11) < 0) };
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
    fn hold_pointer(&self, hold: bool) {
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
                let (x, y, w, h) = self.panel.get();
                let mut at = POINT { x, y };
                let _ = windows::Win32::Graphics::Gdi::ClientToScreen(HWND(self.host.get() as *mut _), &mut at);
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
    fn copied(&self) {
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
    fn cursor_changed(&self) {
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
        self.cursor_serial.set(self.cursor_serial.get().wrapping_add(1));
        if let Some(old) = old {
            // The app may still be wearing it for a moment: it is let go a little later.
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
        // A captured pointer: while the app's window has the keyboard and the picture is the
        // computer's, and Ctrl+Alt+Shift hasn't given it back.
        // SAFETY: a plain query.
        let front = unsafe { GetForegroundWindow() }.0 as isize == self.host.get();
        let want = front && !self.released.get() && self.app.borrow().forwarding() && self.app.borrow().store.settings.capture && self.panel.get().2 > 0;
        self.hold_pointer(want);
        if self.deadline.get().is_some_and(|d| Instant::now() >= d) {
            self.deadline.set(None);
            self.app.borrow_mut().time_up();
        }
        self.publish();
    }

    /// Do what the app's state asks of the engine itself, keep what is the app's, and tell the
    /// app if what it shows has changed.
    fn publish(&self) {
        let effects = {
            let Ok(mut app) = self.app.try_borrow_mut() else { return };
            // Nothing is drawn by the kit here: what it would draw is the app's to show.
            app.dirty = false;
            std::mem::take(&mut app.effects)
        };
        for effect in effects {
            self.apply(effect);
        }
        let changed = self.state_value().is_some_and(|v| *self.told.borrow() != v.to_string());
        if changed || !self.effects.borrow().is_empty() {
            (self.notify)();
        }
    }

    fn apply(&self, effect: Effect) {
        let host = HWND(self.host.get() as *mut _);
        let out = |v: Value| self.effects.borrow_mut().push(v);
        match effect {
            Effect::Minimise => out(json!({"do": "minimise"})),
            Effect::Maximise => out(json!({"do": "maximise"})),
            Effect::Close => out(json!({"do": "close"})),
            Effect::Quit => out(json!({"do": "quit", "code": self.app.borrow().code.unwrap_or(0)})),
            Effect::Fullscreen(on) => out(json!({"do": "fullscreen", "on": on})),
            Effect::Title(title) => out(json!({"do": "title", "text": title})),
            Effect::Size(w, h) => out(json!({"do": "size", "width": w, "height": h})),
            Effect::ReleaseKeys => self.release_keys(),
            Effect::ForgetCursor => {
                if let Some(old) = self.cursor.take() {
                    let _ = old;
                }
                self.cursor_serial.set(self.cursor_serial.get().wrapping_add(1));
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
        self.app.try_borrow().ok().map(|app| state(&app))
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
fn state(app: &App) -> Value {
    let screen = match &app.screen {
        Screen::Computers => json!({"kind": "computers"}),
        Screen::Connecting => json!({"kind": "connecting", "name": app.attempt.as_ref().map(|a| a.target.name.clone()), "address": app.attempt.as_ref().map(|a| a.target.address.clone())}),
        Screen::Problem(t, p) => json!({"kind": "problem", "name": t.name, "address": t.address, "problem": problem(p), "retrying": app.retry_at.is_some()}),
        Screen::Session => json!({"kind": "session"}),
        Screen::Settings => json!({"kind": "settings"}),
        Screen::Computer(a) => json!({"kind": "computer", "address": a}),
    };
    let dialog = app.dialog.as_ref().map(|d| match d {
        Dialog::Pair { waiting, problem } => json!({"kind": "pair", "waiting": waiting, "problem": problem}),
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
                "reach": reach_word(reach),
                "key": app.store.key(&c.address).map(|k| key_words(&k, 4)).unwrap_or_default(),
                "placement": placement_key(c.placement), "sound": c.sound, "clipboard": c.clipboard,
                "connected": app.session.as_ref().is_some_and(|s| s.target.address == c.address),
            })
        })
        .collect();
    let st = &app.store.settings;
    let settings = json!({
        "match_refresh": st.match_refresh, "ctrl_as_command": st.ctrl_as_command, "shortcuts": st.shortcuts,
        "sound": st.sound, "clipboard": st.clipboard, "figures": st.figures, "fullscreen": st.fullscreen,
        "hover_bar": st.hover_bar, "capture": st.capture, "scroll_reversed": st.scroll_reversed,
        "folder": app.store.receive_folder().display().to_string(),
        "controls": options(app, &Choice::Controls), "default_quality": options(app, &Choice::DefaultQuality),
        "scroll": options(app, &Choice::Scroll), "pointer": options(app, &Choice::Pointer),
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
    let decodes = |word: &str| app.decoders.iter().any(|m| format!("{:?}", m.codec).eq_ignore_ascii_case(word));
    json!({
        "screen": screen,
        "dialog": dialog,
        "computers": computers,
        "settings": settings,
        "computer": computer,
        "session": session,
        "fullscreen": app.fullscreen,
        "captured": app.captured,
        "toast": app.toast.as_ref().map(|t| t.0.clone()),
        "remember": app.remember,
        "decodes": {"av1": decodes("av1"), "hevc": decodes("hevc")},
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
