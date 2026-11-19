//! The window: a plain Win32 one with the picture in it. It sends the pointer, buttons, wheel
//! and keys to the other computer, and wears the other computer's cursor.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use anyhow::{Context, Result};
use remoteman_core::{Button, Client, Event};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, GetStockObject, HBRUSH, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use crate::stream::{Shared, pack, unpack};

/// Posted by other threads when the cursor's shape or presence changed.
const WM_CURSOR_CHANGED: u32 = WM_APP + 1;
/// A wheel notch in the protocol's display pixels, as the Linux viewer sends it.
const NOTCH: f32 = 40.0;

/// What the window procedure works with; it lives as long as the window, on its thread.
struct State {
    shared: Arc<Shared>,
    client: Arc<Client>,
    /// Keys the other computer has been told are down.
    held: RefCell<HashSet<u16>>,
    /// The other computer's cursor (`None`: it has none here, so no cursor is shown).
    cursor: Cell<Option<HCURSOR>>,
    /// Until the host has sent a shape, the ordinary arrow.
    arrow: HCURSOR,
    got_shape: Cell<bool>,
    deadline: Cell<Option<Instant>>,
}

thread_local! {
    static STATE: RefCell<Option<Arc<State>>> = const { RefCell::new(None) };
}

/// The primary monitor's refresh rate, for asking the host for the same.
pub fn monitor_refresh() -> u32 {
    let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    // SAFETY: the structure says its own size.
    let ok = unsafe { EnumDisplaySettingsW(None, ENUM_CURRENT_SETTINGS, &mut mode) }.as_bool();
    if ok && mode.dmDisplayFrequency > 1 { mode.dmDisplayFrequency } else { 60 }
}

/// Ask the window (from any thread) to look at the cursor again.
pub fn wake(hwnd: isize) {
    if hwnd != 0 {
        // SAFETY: posting to a window that may be gone is harmless.
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_CURSOR_CHANGED, WPARAM(0), LPARAM(0));
        }
    }
}

/// Make the window, with a client area of the stream's size where the monitor has room for it
/// and the same shape, smaller, where it hasn't. `fullscreen`: no frame, the whole monitor.
pub fn create(shared: &Arc<Shared>, client: &Arc<Client>, size: (u32, u32), fullscreen: bool) -> Result<HWND> {
    // SAFETY: Win32 calls with structures that live through each call; the window procedure's
    // state is set before any message that reads it.
    unsafe {
        // Pixels are pixels: the picture must not be scaled a second time by Windows.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None)?;
        let arrow = LoadCursorW(None, IDC_ARROW)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance.into(),
            lpszClassName: w!("remoteman"),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        anyhow::ensure!(RegisterClassW(&class) != 0, "register the window class");

        let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (style, rect) = if fullscreen {
            (WS_POPUP, info.rcMonitor)
        } else {
            let style = WS_OVERLAPPEDWINDOW;
            let work = info.rcWork;
            let (room_w, room_h) = (((work.right - work.left) as f64 * 0.9).max(320.0), ((work.bottom - work.top) as f64 * 0.9).max(240.0));
            let fit = (room_w / size.0 as f64).min(room_h / size.1 as f64).min(1.0);
            let mut r = RECT { left: 0, top: 0, right: (size.0 as f64 * fit) as i32, bottom: (size.1 as f64 * fit) as i32 };
            let _ = AdjustWindowRectEx(&mut r, style, false, WINDOW_EX_STYLE(0));
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let (x, y) = (work.left + ((work.right - work.left) - w) / 2, work.top + ((work.bottom - work.top) - h) / 2);
            (style, RECT { left: x, top: y, right: x + w, bottom: y + h })
        };

        STATE.with(|s| {
            *s.borrow_mut() = Some(Arc::new(State {
                shared: shared.clone(),
                client: client.clone(),
                held: RefCell::new(HashSet::new()),
                cursor: Cell::new(None),
                arrow,
                got_shape: Cell::new(false),
                deadline: Cell::new(None),
            }))
        });
        let title = windows::core::HSTRING::from(format!("{} - remoteman", client.host_name));
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("remoteman"), &title, style, rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top, None, None, Some(instance.into()), None).context("create the window")?;
        let mut client_rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut client_rect);
        shared.client_size.store(pack(client_rect.right as u32, client_rect.bottom as u32), Ordering::Relaxed);
        shared.hwnd.store(hwnd.0 as isize, Ordering::Relaxed);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        say!("window: client area {}x{}{}", client_rect.right, client_rect.bottom, if fullscreen { ", full screen" } else { "" });
        // A shape may have arrived before there was a window to tell.
        wake(hwnd.0 as isize);
        Ok(hwnd)
    }
}

/// Pump messages until the window is closed, the session ends or `deadline` passes.
pub fn run(shared: &Arc<Shared>, deadline: Option<Instant>) {
    STATE.with(|s| s.borrow().as_ref().unwrap().deadline.set(deadline));
    // SAFETY: the usual message loop; the timer belongs to our window.
    unsafe {
        let hwnd = HWND(shared.hwnd.load(Ordering::Relaxed) as *mut _);
        // Nothing else wakes the loop when the session ends or the time is up.
        SetTimer(Some(hwnd), 1, 100, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    STATE.with(|s| *s.borrow_mut() = None);
}

impl State {
    /// A spot in the client area as fractions of the remote display, allowing for the bars
    /// that keep the picture's shape. `None` before the stream's size is known.
    fn fraction(&self, x: i32, y: i32) -> Option<(f32, f32)> {
        let (vw, vh) = unpack(self.shared.stream_size.load(Ordering::Relaxed));
        let (ww, wh) = unpack(self.shared.client_size.load(Ordering::Relaxed));
        if vw == 0 || vh == 0 || ww == 0 || wh == 0 {
            return None;
        }
        let (vw, vh, ww, wh) = (vw as f64, vh as f64, ww as f64, wh as f64);
        let scale = (ww / vw).min(wh / vh);
        let (dw, dh) = (vw * scale, vh * scale);
        let (ox, oy) = ((ww - dw) / 2.0, (wh - dh) / 2.0);
        Some((((x as f64 - ox) / dw).clamp(0.0, 1.0) as f32, ((y as f64 - oy) / dh).clamp(0.0, 1.0) as f32))
    }

    fn pointer(&self, l: LPARAM) {
        // Signed: while a button is held the pointer may be left of or above the window.
        let (x, y) = ((l.0 & 0xFFFF) as i16 as i32, ((l.0 >> 16) & 0xFFFF) as i16 as i32);
        if let Some((x, y)) = self.fraction(x, y) {
            self.client.send(Event::PointerTo { x, y });
        }
    }

    fn key(&self, w: WPARAM, l: LPARAM, down: bool) {
        let mut scancode = ((l.0 >> 16) & 0xFF) as u16;
        let extended = l.0 & (1 << 24) != 0;
        if scancode == 0 {
            // Keys made up by software may only name the key's meaning.
            // SAFETY: a lookup.
            scancode = unsafe { MapVirtualKeyW(w.0 as u32, MAPVK_VK_TO_VSC) } as u16;
        }
        let Some(usage) = crate::keys::usage(scancode, extended) else { return };
        if down {
            // A key held down repeats here, and the repeats are sent, as the Linux viewer does.
            self.held.borrow_mut().insert(usage);
            self.client.send(Event::Key { usage, down: true });
        } else if self.held.borrow_mut().remove(&usage) {
            // Only what the other computer was told is down.
            self.client.send(Event::Key { usage, down: false });
        }
    }

    /// Let go of everything: the other computer mustn't be left with a key held when this
    /// window stops hearing about the keyboard.
    fn release_keys(&self) {
        for usage in self.held.borrow_mut().drain() {
            self.client.send(Event::Key { usage, down: false });
        }
    }

    /// Take the host's newest shape, if there is one, and wear it.
    fn cursor_changed(&self, hwnd: HWND) {
        if let Some(shape) = self.shared.cursor.lock().unwrap().take() {
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
            self.apply_cursor(hwnd);
            if let Some(old) = old {
                // SAFETY: ours, and no longer the window's cursor.
                unsafe {
                    let _ = DestroyCursor(old);
                }
            }
        } else {
            self.apply_cursor(hwnd);
        }
    }

    fn wanted_cursor(&self) -> Option<HCURSOR> {
        if !self.got_shape.get() {
            Some(self.arrow)
        } else if self.shared.cursor_gone.load(Ordering::Relaxed) {
            // The host says its cursor has left this display.
            None
        } else {
            self.cursor.get()
        }
    }

    /// Set the cursor now if the pointer is over the picture (Windows asks again by itself
    /// whenever the pointer moves).
    fn apply_cursor(&self, hwnd: HWND) {
        // SAFETY: plain queries and a cursor that outlives the call.
        unsafe {
            let (mut at, mut rect) = (POINT::default(), RECT::default());
            if GetCursorPos(&mut at).is_ok() && WindowFromPoint(at) == hwnd && GetClientRect(hwnd, &mut rect).is_ok() {
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut at);
                if at.x >= 0 && at.y >= 0 && at.x < rect.right && at.y < rect.bottom {
                    SetCursor(self.wanted_cursor());
                }
            }
        }
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let Some(state) = STATE.with(|s| s.borrow().clone()) else {
        // SAFETY: passing the message on as it came.
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    };
    let button = |button: Button, down: bool| {
        state.pointer(l);
        state.client.send(Event::Button { button, down });
        // SAFETY: capturing the mouse for our own window. Captured, the release comes here
        // even if the pointer has left the window.
        unsafe {
            if down {
                SetCapture(hwnd);
            } else {
                let _ = ReleaseCapture();
            }
        }
    };
    match msg {
        WM_MOUSEMOVE => state.pointer(l),
        WM_LBUTTONDOWN => button(Button::Left, true),
        WM_LBUTTONUP => button(Button::Left, false),
        WM_RBUTTONDOWN => button(Button::Right, true),
        WM_RBUTTONUP => button(Button::Right, false),
        WM_MBUTTONDOWN => button(Button::Middle, true),
        WM_MBUTTONUP => button(Button::Middle, false),
        // The wheel counts 120 a notch, away from the user positive; the protocol counts
        // towards the end of the document positive.
        WM_MOUSEWHEEL => state.client.send(Event::Scroll { dx: 0.0, dy: -(((w.0 >> 16) & 0xFFFF) as i16 as f32) / 120.0 * NOTCH }),
        WM_MOUSEHWHEEL => state.client.send(Event::Scroll { dx: (((w.0 >> 16) & 0xFFFF) as i16 as f32) / 120.0 * NOTCH, dy: 0.0 }),
        // The SYS ones are keys pressed with Alt held, and F10: the other computer's too. Not
        // handing them on to Windows also keeps Alt from opening the window's menu.
        WM_KEYDOWN | WM_SYSKEYDOWN => state.key(w, l, true),
        WM_KEYUP | WM_SYSKEYUP => state.key(w, l, false),
        WM_SYSCHAR => {}
        WM_KILLFOCUS => state.release_keys(),
        WM_SIZE => {
            let (width, height) = ((l.0 & 0xFFFF) as u32, ((l.0 >> 16) & 0xFFFF) as u32);
            // The draw thread follows; a minimised window (0x0) keeps what it had.
            if width > 0 && height > 0 {
                state.shared.client_size.store(pack(width, height), Ordering::Relaxed);
            }
        }
        WM_SETCURSOR if (l.0 & 0xFFFF) as u32 == HTCLIENT => {
            // SAFETY: a cursor that outlives the call, or none.
            unsafe { SetCursor(state.wanted_cursor()) };
            return LRESULT(1);
        }
        WM_CURSOR_CHANGED => state.cursor_changed(hwnd),
        // The draw thread paints all of it.
        WM_ERASEBKGND => return LRESULT(1),
        WM_TIMER => {
            let over = state.deadline.get().is_some_and(|d| Instant::now() >= d) || state.shared.closed.lock().unwrap().is_some();
            if over {
                // SAFETY: our own window.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
        WM_DESTROY => {
            state.release_keys();
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
        }
        // SAFETY: passing the message on as it came.
        _ => return unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
    LRESULT(0)
}
