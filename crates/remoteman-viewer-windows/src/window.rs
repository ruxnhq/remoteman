//! The window: one Win32 window for the whole app, with no frame of Windows' own (the app
//! draws its title bar, and says which parts of it move the window and which is the maximise
//! button). Every event is handed to the [`App`]; what comes back is done here. While a
//! session's picture is all there is under the pointer, the pointer, buttons, wheel and keys go
//! to the other computer instead, and the window wears that computer's cursor.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use anyhow::{Context, Result};
use remoteman_core::{Button, Event};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, GetStockObject, HBRUSH, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow, ScreenToClient};
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyState, MAPVK_VK_TO_VSC, MapVirtualKeyW, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, PWSTR, w};

use crate::app::{App, Effect, Hit, Msg, Pointer};
use crate::stream::pack;

/// Posted by other threads when something of theirs is ready: an answer to an attempt, the
/// session's end, the cursor's shape.
const WM_WAKE: u32 = WM_APP + 1;
/// Posted by the window to itself when its size could not be handed on at once.
const WM_SYNC: u32 = WM_APP + 2;
/// A wheel notch in the protocol's display pixels, as the Linux viewer sends it.
const NOTCH: f32 = 40.0;
/// Text on the clipboard, as UTF-16.
const CF_UNICODETEXT: u32 = 13;

/// Going through the screens for pictures of them (`--ui`).
struct Walk {
    steps: Vec<String>,
    at: usize,
    /// How many pictures had been saved when the one being waited for was asked for.
    waiting: Option<u32>,
    dir: PathBuf,
}

/// What the window procedure works with; it lives as long as the window, on its thread.
struct Win {
    app: RefCell<App>,
    hwnd: Cell<isize>,
    /// Keys the other computer has been told are down.
    held: RefCell<HashSet<u16>>,
    /// Mouse buttons the other computer has been told are down, as bits.
    buttons: Cell<u8>,
    /// The other computer's cursor (`None`: it has none here, so no cursor is shown).
    cursor: Cell<Option<HCURSOR>>,
    got_shape: Cell<bool>,
    arrow: HCURSOR,
    ibeam: HCURSOR,
    /// Windows has been asked to say when the pointer leaves.
    tracking: Cell<bool>,
    /// Where the pointer last was in the client area, in pixels.
    mouse: Cell<(i32, i32)>,
    full: Cell<bool>,
    /// The window's style and place before it went full screen.
    before_full: RefCell<Option<(WINDOW_STYLE, WINDOWPLACEMENT)>>,
    deadline: Cell<Option<Instant>>,
    /// The system's shortcuts are this PC's own for now (Ctrl+Alt+Shift gave them back).
    released: Cell<bool>,
    /// Ctrl, Alt and Shift went down together with nothing else: letting go toggles `released`.
    chord: Cell<bool>,
    hook: Cell<isize>,
    walk: RefCell<Option<Walk>>,
    /// The colours follow Windows' setting (not fixed on the command line).
    follow_theme: bool,
}

thread_local! {
    static WIN: RefCell<Option<Rc<Win>>> = const { RefCell::new(None) };
}

fn win() -> Option<Rc<Win>> {
    WIN.with(|w| w.borrow().clone())
}

/// Whether Windows is set to dark apps.
pub fn system_dark() -> bool {
    let (mut value, mut size) = (1u32, 4u32);
    // SAFETY: the out pointers are valid for the sizes given.
    let read = unsafe { RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), w!("AppsUseLightTheme"), RRF_RT_REG_DWORD, None, Some(&mut value as *mut u32 as *mut _), Some(&mut size)) };
    read.is_ok() && value == 0
}

/// Ask the window (from any thread) to look at what has changed.
pub fn wake(hwnd: isize) {
    if hwnd != 0 {
        // SAFETY: posting to a window that may be gone is harmless.
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_WAKE, WPARAM(0), LPARAM(0));
        }
    }
}

/// The text on this PC's clipboard, if it holds any.
pub fn clipboard_get() -> Option<String> {
    let hwnd = win().map(|w| w.hwnd.get()).unwrap_or(0);
    // SAFETY: the clipboard is opened and closed here; the memory is read within its size.
    unsafe {
        OpenClipboard(Some(HWND(hwnd as *mut _))).ok()?;
        let text = GetClipboardData(CF_UNICODETEXT).ok().and_then(|handle| {
            let global = HGLOBAL(handle.0);
            let at = GlobalLock(global) as *const u16;
            if at.is_null() {
                return None;
            }
            let units = std::slice::from_raw_parts(at, GlobalSize(global) / 2);
            let text = String::from_utf16_lossy(&units[..units.iter().position(|c| *c == 0).unwrap_or(units.len())]);
            let _ = GlobalUnlock(global);
            Some(text)
        });
        let _ = CloseClipboard();
        text
    }
}

/// Put text on this PC's clipboard.
pub fn clipboard_set(text: &str) {
    let hwnd = win().map(|w| w.hwnd.get()).unwrap_or(0);
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the memory is allocated for the text and its end, filled, and handed to the
    // clipboard, which owns it from then on.
    unsafe {
        if OpenClipboard(Some(HWND(hwnd as *mut _))).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(global) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let at = GlobalLock(global) as *mut u16;
            if !at.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), at, wide.len());
                let _ = GlobalUnlock(global);
                let _ = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(global.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

/// Ask for a file to send. `None`: nothing was chosen.
fn pick_file(hwnd: HWND) -> Option<PathBuf> {
    let mut name = [0u16; 2048];
    let mut ask = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFile: PWSTR(name.as_mut_ptr()),
        nMaxFile: name.len() as u32,
        lpstrTitle: w!("Send a file"),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: the structure and the buffer it points at live through the call.
    let chosen = unsafe { GetOpenFileNameW(&mut ask) }.as_bool();
    chosen.then(|| PathBuf::from(String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(0)])))
}

/// The size in pixels and the refresh rate of the monitor the window is on.
fn monitor_of(hwnd: HWND) -> (u32, u32, u32, RECT, RECT) {
    // SAFETY: structures that say their own size and live through each call.
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info.monitorInfo);
        let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
        let ok = EnumDisplaySettingsW(PCWSTR(info.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode).as_bool();
        let (r, work) = (info.monitorInfo.rcMonitor, info.monitorInfo.rcWork);
        let refresh = if ok && mode.dmDisplayFrequency > 1 { mode.dmDisplayFrequency } else { 60 };
        (((r.right - r.left).max(2)) as u32, ((r.bottom - r.top).max(2)) as u32, refresh, r, work)
    }
}

/// Make the window around `app`, which it then owns. A list's worth of room, or a picture's
/// if a computer was named on the command line.
pub fn create(mut app: App) -> Result<HWND> {
    // SAFETY: Win32 calls with structures that live through each call; the window procedure
    // finds no state until it is set below, and passes everything on meanwhile.
    unsafe {
        // Pixels are pixels: the picture must not be scaled a second time by Windows.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(procedure),
            hInstance: instance.into(),
            lpszClassName: w!("remoteman"),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        anyhow::ensure!(RegisterClassW(&class) != 0, "register the window class");
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("remoteman"), w!("remoteman"), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, 1000, 700, None, None, Some(instance.into()), None).context("create the window")?;

        let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        let (mw, mh, hz, _, work) = monitor_of(hwnd);
        app.scale = scale;
        app.monitor = (mw, mh, hz);
        let want = if app.args.host.is_some() { (1224.0, 744.0) } else { (984.0, 644.0) };
        let (w, h) = (((want.0 * scale) as i32).min(work.right - work.left), ((want.1 * scale) as i32).min(work.bottom - work.top));
        let (x, y) = (work.left + ((work.right - work.left) - w) / 2, work.top + ((work.bottom - work.top) - h) / 2);

        let walk = app.args.ui.clone().map(|steps| Walk { steps: crate::walk::steps(&steps), at: 0, waiting: None, dir: app.args.shots.clone().unwrap_or_default() });
        let follow_theme = app.args.dark.is_none();
        let seconds = app.args.seconds;
        let state = Rc::new(Win {
            app: RefCell::new(app),
            hwnd: Cell::new(hwnd.0 as isize),
            held: RefCell::new(HashSet::new()),
            buttons: Cell::new(0),
            cursor: Cell::new(None),
            got_shape: Cell::new(false),
            arrow: LoadCursorW(None, IDC_ARROW)?,
            ibeam: LoadCursorW(None, IDC_IBEAM)?,
            tracking: Cell::new(false),
            mouse: Cell::new((-1, -1)),
            full: Cell::new(false),
            before_full: RefCell::new(None),
            deadline: Cell::new(seconds.map(|s| Instant::now() + std::time::Duration::from_secs(s))),
            released: Cell::new(false),
            chord: Cell::new(false),
            hook: Cell::new(0),
            walk: RefCell::new(walk),
            follow_theme,
        });
        WIN.with(|w| *w.borrow_mut() = Some(state.clone()));
        state.app.borrow().scene.hwnd.store(hwnd.0 as isize, Ordering::Relaxed);
        // With the state in place the frame is worked out again, this time by us; what is
        // left of it (the borders at the sides and the bottom) comes on top of the size wanted.
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_FRAMECHANGED | SWP_NOZORDER | SWP_NOACTIVATE);
        let (mut outer, mut inner) = (RECT::default(), RECT::default());
        let _ = GetWindowRect(hwnd, &mut outer);
        let _ = GetClientRect(hwnd, &mut inner);
        let (fw, fh) = ((outer.right - outer.left) - inner.right, (outer.bottom - outer.top) - inner.bottom);
        let _ = SetWindowPos(hwnd, None, x - fw / 2, y, w + fw, h + fh, SWP_NOZORDER | SWP_NOACTIVATE);
        state.resync(hwnd);
        let _ = AddClipboardFormatListener(hwnd);
        // The Windows key, Alt+Tab and the like, while a session has the keyboard.
        if let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(instance.into()), 0) {
            state.hook.set(hook.0 as isize);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        say!("window: {}x{} pixels at {:.0} % scaling, on a {mw}x{mh} monitor at {hz} Hz", w, h, scale * 100.0);
        Ok(hwnd)
    }
}

/// Pump messages until the window is closed.
pub fn run() {
    let Some(state) = win() else { return };
    // SAFETY: the usual message loop; the timer belongs to our window.
    unsafe {
        let hwnd = state.hwnd();
        // What moves by itself (the ring, the bar, the caret), the figures, and what other
        // threads leave to be noticed.
        SetTimer(Some(hwnd), 1, 16, None);
        state.event(|app| app.start());
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let hook = state.hook.replace(0);
        if hook != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(hook as *mut _));
        }
    }
}

/// When the loop has ended: the app, for what it has to say at the end.
pub fn take() -> Option<App> {
    let state = WIN.with(|w| w.borrow_mut().take())?;
    Rc::try_unwrap(state).ok().map(|w| w.app.into_inner())
}

fn point(l: LPARAM) -> (i32, i32) {
    // Signed: while a button is held the pointer may be left of or above the window.
    ((l.0 & 0xFFFF) as i16 as i32, ((l.0 >> 16) & 0xFFFF) as i16 as i32)
}

impl Win {
    fn hwnd(&self) -> HWND {
        HWND(self.hwnd.get() as *mut _)
    }

    /// Hand something to the app and do what comes back. `None` if the app is busy (an event
    /// that arrived inside another's handling).
    fn event<R>(&self, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        let (result, effects) = {
            let mut app = self.app.try_borrow_mut().ok()?;
            let result = f(&mut app);
            (result, app.finish())
        };
        for effect in effects {
            self.apply(effect);
        }
        Some(result)
    }

    /// Ask the app something.
    fn ask<R>(&self, f: impl FnOnce(&App) -> R) -> Option<R> {
        self.app.try_borrow().ok().map(|app| f(&app))
    }

    fn forwarding(&self) -> bool {
        self.ask(|app| app.forwarding()).unwrap_or(false)
    }

    fn scale(&self) -> f32 {
        // SAFETY: a query on our own window.
        unsafe { GetDpiForWindow(self.hwnd()).max(96) as f32 / 96.0 }
    }

    /// The window's size has changed, or what it is on: tell the draw thread and the app.
    fn resync(&self, hwnd: HWND) {
        let mut r = RECT::default();
        // SAFETY: queries on our own window.
        let (zoomed, scale) = unsafe {
            let _ = GetClientRect(hwnd, &mut r);
            (IsZoomed(hwnd).as_bool(), self.scale())
        };
        if r.right <= 0 || r.bottom <= 0 {
            // Minimised: everything keeps what it had.
            return;
        }
        let (mw, mh, hz, ..) = monitor_of(hwnd);
        let told = self.event(|app| {
            app.scene.client_size.store(pack(r.right as u32, r.bottom as u32), Ordering::Relaxed);
            app.monitor = (mw, mh, hz);
            app.resized(r.right as f32 / scale, r.bottom as f32 / scale, scale, zoomed);
        });
        if told.is_none() {
            // SAFETY: posting to our own window.
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_SYNC, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn apply(&self, effect: Effect) {
        let hwnd = self.hwnd();
        // SAFETY: calls on our own window, with structures that live through each call. None
        // of this runs while the app is borrowed, so messages these calls send find it free.
        unsafe {
            match effect {
                Effect::Minimise => {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                }
                Effect::Maximise => {
                    let _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
                }
                Effect::Close | Effect::Quit => {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                Effect::Fullscreen(on) => self.fullscreen(on),
                Effect::Title(title) => {
                    let _ = SetWindowTextW(hwnd, &windows::core::HSTRING::from(title));
                }
                Effect::ReleaseKeys => self.release_keys(),
                Effect::ForgetCursor => {
                    self.got_shape.set(false);
                    if let Some(old) = self.cursor.take() {
                        let _ = DestroyCursor(old);
                    }
                }
                Effect::Size(w, h) => {
                    if !self.full.get() && !IsZoomed(hwnd).as_bool() {
                        let scale = self.scale();
                        let (.., work) = monitor_of(hwnd);
                        let mut now = RECT::default();
                        let _ = GetWindowRect(hwnd, &mut now);
                        let mut client = RECT::default();
                        let _ = GetClientRect(hwnd, &mut client);
                        // The frame's share of the window stays as it is.
                        let (fw, fh) = ((now.right - now.left) - client.right, (now.bottom - now.top) - client.bottom);
                        let (w, h) = (((w * scale).round() as i32 + fw).min(work.right - work.left), ((h * scale).round() as i32 + fh).min(work.bottom - work.top));
                        // About where it was, but all of it on the screen.
                        let cx = (now.left + now.right) / 2;
                        let x = (cx - w / 2).clamp(work.left, (work.right - w).max(work.left));
                        let y = now.top.clamp(work.top, (work.bottom - h).max(work.top));
                        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
                    }
                }
                Effect::PickFile => {
                    self.release_keys();
                    if let Some(path) = pick_file(hwnd) {
                        self.event(|app| app.send_file(&path));
                    }
                }
                Effect::SetClipboard(text) => clipboard_set(&text),
                Effect::ReadClipboard => {
                    if let Some(text) = clipboard_get() {
                        self.event(|app| app.copied_here(text));
                    }
                }
            }
        }
    }

    /// The whole monitor with no frame, or back to how the window was.
    fn fullscreen(&self, on: bool) {
        if on == self.full.get() {
            return;
        }
        let hwnd = self.hwnd();
        // SAFETY: calls on our own window, with structures that live through each call.
        unsafe {
            if on {
                let mut place = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                let _ = GetWindowPlacement(hwnd, &mut place);
                *self.before_full.borrow_mut() = Some((WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32), place));
                self.full.set(true);
                let (.., r, _) = monitor_of(hwnd);
                SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as isize);
                let _ = SetWindowPos(hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_FRAMECHANGED | SWP_NOOWNERZORDER);
            } else {
                self.full.set(false);
                if let Some((style, place)) = self.before_full.borrow_mut().take() {
                    SetWindowLongPtrW(hwnd, GWL_STYLE, (style | WS_VISIBLE).0 as isize);
                    let _ = SetWindowPlacement(hwnd, &place);
                }
                let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER);
            }
        }
        say!("window: {}", if on { "full screen" } else { "out of full screen" });
        self.event(|app| app.set_fullscreen(on));
        self.resync(hwnd);
    }

    // To the other computer.

    fn send(&self, event: Event) {
        if let Some(Some(client)) = self.ask(|app| app.client().cloned()) {
            client.send(event);
        }
    }

    /// The pointer at a place in the client area, as the other computer is told.
    fn pointer(&self, x: i32, y: i32) {
        if let Some(Some((x, y))) = self.ask(|app| app.fraction(x as f32, y as f32)) {
            self.send(Event::PointerTo { x, y });
        }
    }

    /// A key by where it is on the keyboard.
    fn key(&self, scancode: u16, extended: bool, down: bool) {
        let Some(own) = crate::keys::usage(scancode, extended) else { return };
        let modifier = (0xE0..=0xE7).contains(&own);
        if down {
            let usage = self.ask(|app| app.key_for(own)).unwrap_or(own);
            // A key held down repeats here, and the repeats are sent, as the Linux viewer does.
            self.held.borrow_mut().insert(usage);
            self.send(Event::Key { usage, down: true });
            // Ctrl, Alt and Shift together and nothing else: the shortcut that gives the
            // system's shortcuts back to this PC, when they are let go.
            let down = |vk: i32| unsafe { GetKeyState(vk) } < 0;
            self.chord.set(modifier && down(0x11) && down(0x12) && down(0x10) && self.held.borrow().iter().all(|u| (0xE0..=0xE7).contains(u)));
        } else {
            // Only what the other computer was told is down.
            let usage = self.ask(|app| app.key_for(own)).unwrap_or(own);
            if self.held.borrow_mut().remove(&usage) {
                self.send(Event::Key { usage, down: false });
            }
            if modifier && self.chord.replace(false) {
                let released = !self.released.get();
                self.released.set(released);
                self.event(|app| app.say(if released { "System shortcuts are this PC's again. Ctrl+Alt+Shift sends them to the computer" } else { "System shortcuts go to the computer. Ctrl+Alt+Shift gives them back to this PC" }));
            }
        }
    }

    /// Let go of everything: the other computer mustn't be left with a key or a button held
    /// when this window stops hearing about them.
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

    /// A key seen before Windows acts on it: says whether it is the other computer's.
    fn grabs(&self, key: &KBDLLHOOKSTRUCT, down: bool) -> bool {
        const TAB: u32 = 0x09;
        const ESCAPE: u32 = 0x1B;
        const LWIN: u32 = 0x5B;
        const RWIN: u32 = 0x5C;
        let extended = key.flags.0 & LLKHF_EXTENDED.0 != 0;
        let Some(usage) = crate::keys::usage(key.scanCode as u16, extended) else { return false };
        if !down {
            // What was sent down is sent up, wherever the keyboard is by then.
            let usage = self.ask(|app| app.key_for(usage)).unwrap_or(usage);
            let ours = self.held.borrow().contains(&usage) && matches!(key.vkCode, TAB | ESCAPE | LWIN | RWIN);
            if ours {
                self.key(key.scanCode as u16, extended, false);
            }
            return ours;
        }
        // SAFETY: plain queries.
        let (front, ctrl) = unsafe { (GetForegroundWindow() == self.hwnd(), GetAsyncKeyState(0x11) < 0) };
        let alt = key.flags.0 & LLKHF_ALTDOWN.0 != 0;
        let system = match key.vkCode {
            LWIN | RWIN => true,
            TAB => alt,
            ESCAPE => alt || ctrl,
            _ => false,
        };
        if !system || !front || self.released.get() || !self.ask(|app| app.forwarding() && app.store.settings.shortcuts).unwrap_or(false) {
            return false;
        }
        self.key(key.scanCode as u16, extended, true);
        true
    }

    // The cursor.

    /// Take the host's newest shape, if there is one, and wear it.
    fn cursor_changed(&self) {
        let shape = self.ask(|app| app.session.as_ref().and_then(|s| s.shared.cursor.lock().unwrap().take())).flatten();
        if let Some(shape) = shape {
            let made = crate::cursor::make(&shape);
            // Shapes change a few times a minute at most, so each is worth a line.
            if shape.width == 0 {
                say!("cursor: the host has none now");
            } else if made.is_none() {
                say!("cursor: Windows wouldn't make a {}x{} cursor; none is shown", shape.width, shape.height);
            } else {
                say!("cursor: wearing the host's ({}x{}, hot spot {},{})", shape.width, shape.height, shape.hot_x, shape.hot_y);
            }
            let old = self.cursor.replace(made);
            self.got_shape.set(true);
            self.set_cursor();
            if let Some(old) = old {
                // SAFETY: ours, and no longer the window's cursor.
                unsafe {
                    let _ = DestroyCursor(old);
                }
            }
        } else {
            self.set_cursor();
        }
    }

    fn wanted_cursor(&self) -> Option<HCURSOR> {
        let (x, y) = self.mouse.get();
        let scale = self.scale();
        let pointer = self.ask(|app| (app.pointer(x as f32 / scale, y as f32 / scale), app.session.as_ref().is_some_and(|s| s.shared.cursor_gone.load(Ordering::Relaxed))));
        match pointer {
            Some((Pointer::Text, _)) => Some(self.ibeam),
            // Until the host has sent a shape, the ordinary arrow.
            Some((Pointer::Theirs, _)) if !self.got_shape.get() => Some(self.arrow),
            // The host says its cursor has left this display.
            Some((Pointer::Theirs, true)) => None,
            Some((Pointer::Theirs, false)) => self.cursor.get(),
            _ => Some(self.arrow),
        }
    }

    /// Set the cursor now if the pointer is in the window (Windows asks again by itself
    /// whenever the pointer moves).
    fn set_cursor(&self) {
        let hwnd = self.hwnd();
        // SAFETY: plain queries and a cursor that outlives the call.
        unsafe {
            let (mut at, mut rect) = (POINT::default(), RECT::default());
            if GetCursorPos(&mut at).is_ok() && WindowFromPoint(at) == hwnd && GetClientRect(hwnd, &mut rect).is_ok() {
                let _ = ScreenToClient(hwnd, &mut at);
                if at.x >= 0 && at.y >= 0 && at.x < rect.right && at.y < rect.bottom {
                    SetCursor(self.wanted_cursor());
                }
            }
        }
    }

    // The pointer.

    fn moved(&self, x: i32, y: i32, client: bool) {
        self.mouse.set((x, y));
        if !self.tracking.replace(true) {
            let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: if client { TME_LEAVE } else { TME_LEAVE | TME_NONCLIENT }, hwndTrack: self.hwnd(), dwHoverTime: 0 };
            // SAFETY: the structure lives through the call.
            unsafe {
                let _ = TrackMouseEvent(&mut track);
            }
        }
        let scale = self.scale();
        let taken = self.event(|app| app.mouse_move(x as f32 / scale, y as f32 / scale)).unwrap_or(true);
        // A button held on the picture keeps the pointer the other computer's, wherever it goes.
        if self.buttons.get() != 0 || (!taken && self.forwarding()) {
            self.pointer(x, y);
        }
    }

    fn button(&self, button: Button, bit: u8, down: bool, l: LPARAM) {
        let (x, y) = point(l);
        let scale = self.scale();
        let (dx, dy) = (x as f32 / scale, y as f32 / scale);
        let hwnd = self.hwnd();
        // SAFETY: focus and capture for our own window.
        unsafe {
            if down {
                let _ = SetFocus(Some(hwnd));
                let taken = if self.buttons.get() != 0 {
                    false
                } else if button == Button::Left {
                    self.event(|app| app.mouse_down(dx, dy)).unwrap_or(true)
                } else {
                    self.ask(|app| app.over_interface(dx, dy)).unwrap_or(true)
                };
                if !taken && self.forwarding() {
                    self.pointer(x, y);
                    self.send(Event::Button { button, down: true });
                    self.buttons.set(self.buttons.get() | bit);
                }
                // Captured, the release comes here even if the pointer has left the window.
                SetCapture(hwnd);
            } else {
                if self.buttons.get() & bit != 0 {
                    self.pointer(x, y);
                    self.send(Event::Button { button, down: false });
                    self.buttons.set(self.buttons.get() & !bit);
                } else if button == Button::Left {
                    self.event(|app| app.mouse_up(dx, dy));
                }
                if self.buttons.get() == 0 {
                    let _ = ReleaseCapture();
                }
            }
        }
    }

    fn wheel(&self, notches: f32, horizontal: bool) {
        let (x, y) = self.mouse.get();
        let scale = self.scale();
        let theirs = self.ask(|app| app.forwarding() && !app.over_interface(x as f32 / scale, y as f32 / scale)).unwrap_or(false);
        if theirs {
            let k = if self.ask(|app| app.store.settings.scroll_reversed).unwrap_or(false) { -NOTCH } else { NOTCH };
            // The wheel counts away from the user positive; the protocol counts towards the
            // end of the document positive.
            self.send(if horizontal { Event::Scroll { dx: notches * k, dy: 0.0 } } else { Event::Scroll { dx: 0.0, dy: -notches * k } });
        } else if !horizontal {
            self.event(|app| app.wheel(notches));
        }
    }

    /// Sixty times a second: what moves by itself, what other threads have left, and the
    /// pictures of the window if those are being taken.
    fn tick(&self) {
        self.event(|app| {
            app.poll();
            app.tick();
        });
        if self.deadline.get().is_some_and(|d| Instant::now() >= d) {
            self.deadline.set(None);
            self.event(|app| app.time_up());
        }
        self.walk();
    }

    fn walk(&self) {
        let step = {
            let mut walk = self.walk.borrow_mut();
            let Some(walk) = walk.as_mut() else { return };
            let saved = self.ask(|app| app.scene.shots.load(Ordering::Relaxed)).unwrap_or(0);
            if let Some(before) = walk.waiting {
                if saved == before {
                    return;
                }
                walk.waiting = None;
                walk.at += 1;
            }
            match walk.steps.get(walk.at) {
                Some(step) => (step.clone(), walk.dir.join(format!("{:02}-{step}.png", walk.at + 1)), saved),
                None => {
                    say!("walk: {} pictures saved", walk.at);
                    drop(self.walk.take());
                    self.event(|app| app.quit(0));
                    return;
                }
            }
        };
        let (name, path, saved) = step;
        // The step itself, and whatever it does to the window; then, with the window as it
        // now is, the description the picture is to show.
        self.event(|app| crate::walk::apply(app, &name));
        let serial = self.event(|app| app.build());
        if let (Some(serial), Some(walk)) = (serial, self.walk.borrow_mut().as_mut()) {
            self.ask(|app| {
                *app.scene.shot.lock().unwrap() = Some((path, serial));
                app.scene.poke();
            });
            walk.waiting = Some(saved);
        }
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for this hook the parameter points at the key's description.
        let key = unsafe { &*(l.0 as *const KBDLLHOOKSTRUCT) };
        let down = matches!(w.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if win().is_some_and(|win| win.grabs(key, down)) {
            return LRESULT(1);
        }
    }
    // SAFETY: passing the key on as it came.
    unsafe { CallNextHookEx(None, code, w, l) }
}

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: passing the message on as it came.
    let pass = || unsafe { DefWindowProcW(hwnd, msg, w, l) };
    let Some(win) = win() else { return pass() };
    // SAFETY: Win32 calls on our own window with structures that live through each call, and
    // pointers Windows hands over with the message they belong to.
    unsafe {
        match msg {
            // The frame: none of Windows' own at the top, so the client area starts where the
            // window does; its borders at the sides and the bottom stay, for resizing.
            WM_NCCALCSIZE if w.0 != 0 => {
                if win.full.get() {
                    return LRESULT(0);
                }
                let params = &mut *(l.0 as *mut NCCALCSIZE_PARAMS);
                let top = params.rgrc[0].top;
                let result = pass();
                let dpi = GetDpiForWindow(hwnd);
                // Maximised, Windows hangs the frame over the monitor's edge on every side.
                params.rgrc[0].top = if IsZoomed(hwnd).as_bool() { top + GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) } else { top };
                return result;
            }
            WM_NCHITTEST => {
                let hit = pass();
                if hit.0 as u32 != HTCLIENT || win.full.get() {
                    return hit;
                }
                let mut at = POINT { x: (l.0 & 0xFFFF) as i16 as i32, y: ((l.0 >> 16) & 0xFFFF) as i16 as i32 };
                let _ = ScreenToClient(hwnd, &mut at);
                let scale = win.scale();
                // The top edge resizes too, as the frame that isn't there would.
                if !IsZoomed(hwnd).as_bool() && at.y < (5.0 * scale) as i32 {
                    return LRESULT(HTTOP as isize);
                }
                return LRESULT(match win.ask(|app| app.hit(at.x as f32 / scale, at.y as f32 / scale)) {
                    Some(Hit::Caption) => HTCAPTION,
                    Some(Hit::Maximise) => HTMAXBUTTON,
                    _ => HTCLIENT,
                } as isize);
            }
            // The maximise button is told to Windows as such (for its snap layouts), so its
            // presses come as presses on the frame.
            WM_NCMOUSEMOVE => {
                let mut at = POINT { x: (l.0 & 0xFFFF) as i16 as i32, y: ((l.0 >> 16) & 0xFFFF) as i16 as i32 };
                let _ = ScreenToClient(hwnd, &mut at);
                win.moved(at.x, at.y, false);
                return pass();
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONUP if w.0 as u32 == HTMAXBUTTON => {
                let mut at = POINT { x: (l.0 & 0xFFFF) as i16 as i32, y: ((l.0 >> 16) & 0xFFFF) as i16 as i32 };
                let _ = ScreenToClient(hwnd, &mut at);
                let scale = win.scale();
                let (x, y) = (at.x as f32 / scale, at.y as f32 / scale);
                if msg == WM_NCLBUTTONDOWN {
                    win.event(|app| app.mouse_down(x, y));
                } else {
                    win.event(|app| app.mouse_up(x, y));
                }
            }
            WM_NCMOUSELEAVE | WM_MOUSELEAVE => {
                win.tracking.set(false);
                // (From the frame to the client area or back is not leaving.)
                let mut at = POINT::default();
                let _ = GetCursorPos(&mut at);
                if WindowFromPoint(at) != hwnd {
                    win.mouse.set((-1, -1));
                    win.event(|app| app.mouse_leave());
                }
            }
            WM_GETMINMAXINFO => {
                let info = &mut *(l.0 as *mut MINMAXINFO);
                let scale = win.scale();
                info.ptMinTrackSize = POINT { x: (560.0 * scale) as i32, y: (400.0 * scale) as i32 };
            }
            WM_MOUSEMOVE => {
                let (x, y) = point(l);
                win.moved(x, y, true);
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => win.button(Button::Left, 1, true, l),
            WM_LBUTTONUP => win.button(Button::Left, 1, false, l),
            WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => win.button(Button::Right, 2, true, l),
            WM_RBUTTONUP => win.button(Button::Right, 2, false, l),
            WM_MBUTTONDOWN | WM_MBUTTONDBLCLK => win.button(Button::Middle, 4, true, l),
            WM_MBUTTONUP => win.button(Button::Middle, 4, false, l),
            WM_MOUSEWHEEL => win.wheel(((w.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0, false),
            WM_MOUSEHWHEEL => win.wheel(((w.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0, true),
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                const F11: u32 = 0x7A;
                let vk = w.0 as u32;
                let held = |vk: i32| GetKeyState(vk) < 0;
                let (shift, ctrl, alt) = (held(0x10), held(0x11), held(0x12));
                let session = win.ask(|app| app.session.is_some() && app.screen == crate::app::Screen::Session).unwrap_or(false);
                if session && vk == F11 {
                    // This window's own, as in the Linux viewer.
                    if l.0 & (1 << 30) == 0 {
                        win.event(|app| app.update(Msg::ToggleFullscreen));
                    }
                } else if session && ctrl && alt && shift && vk == 0x49 {
                    win.event(|app| app.update(Msg::ToggleFigures));
                } else if win.forwarding() {
                    let mut scancode = ((l.0 >> 16) & 0xFF) as u16;
                    if scancode == 0 {
                        // Keys made up by software may only name the key's meaning.
                        scancode = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) as u16;
                    }
                    win.key(scancode, l.0 & (1 << 24) != 0, true);
                } else {
                    let used = win.event(|app| app.key_down(vk, shift, ctrl)).unwrap_or(false);
                    // Alt+F4 and the like stay Windows' own on the app's own screens.
                    if msg == WM_SYSKEYDOWN && !used {
                        return pass();
                    }
                }
            }
            WM_KEYUP | WM_SYSKEYUP => {
                let mut scancode = ((l.0 >> 16) & 0xFF) as u16;
                if scancode == 0 {
                    scancode = MapVirtualKeyW(w.0 as u32, MAPVK_VK_TO_VSC) as u16;
                }
                win.key(scancode, l.0 & (1 << 24) != 0, false);
                if msg == WM_SYSKEYUP && !win.forwarding() {
                    return pass();
                }
            }
            WM_CHAR => {
                if !win.forwarding() {
                    win.event(|app| app.char(w.0 as u16));
                }
            }
            // Not handing these on keeps Alt from opening the window's menu during a session.
            WM_SYSCHAR => {
                if !win.forwarding() {
                    return pass();
                }
            }
            WM_KILLFOCUS => {
                win.release_keys();
                win.event(|app| app.blur());
            }
            WM_SIZE | WM_SYNC => win.resync(hwnd),
            WM_EXITSIZEMOVE | WM_DISPLAYCHANGE => win.resync(hwnd),
            WM_DPICHANGED => {
                // Windows says where the window should be at the new scaling.
                let r = &*(l.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                win.resync(hwnd);
            }
            WM_SETTINGCHANGE => {
                if win.follow_theme {
                    let dark = system_dark();
                    win.event(|app| app.set_dark(dark));
                }
                return pass();
            }
            WM_SETCURSOR if (l.0 & 0xFFFF) as u32 == HTCLIENT => {
                SetCursor(win.wanted_cursor());
                return LRESULT(1);
            }
            WM_WAKE => {
                win.event(|app| app.poll());
                win.cursor_changed();
            }
            WM_CLIPBOARDUPDATE => {
                // Only looked at while a session shares it.
                if win.ask(|app| app.session.as_ref().is_some_and(|s| s.clipboard_on)).unwrap_or(false) {
                    if let Some(text) = clipboard_get() {
                        win.event(|app| app.copied_here(text));
                    }
                }
            }
            // The draw thread paints all of it.
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                let _ = windows::Win32::Graphics::Gdi::ValidateRect(Some(hwnd), None);
                win.ask(|app| app.scene.poke());
            }
            WM_TIMER => win.tick(),
            WM_CLOSE => {
                win.release_keys();
                win.event(|app| app.closing());
                let _ = DestroyWindow(hwnd);
            }
            WM_DESTROY => PostQuitMessage(0),
            _ => return pass(),
        }
    }
    LRESULT(0)
}
