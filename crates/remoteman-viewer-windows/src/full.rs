//! Full screen: the session's picture in a top-level window of the engine's own over the whole
//! monitor, with the old program's bar (drawn with Direct2D, `session.rs`) coming out at the top
//! edge, the keyboard and the pointer. The app's window waits hidden behind it.
//!
//! Why not the app's own window in full screen: a picture inside WinUI's `SwapChainPanel` is
//! composed by the desktop and never makes a monitor vary its refresh. What does is an
//! activated top-level window presenting into an HWND flip swap chain with tearing allowed
//! (measured on an RTX 5090 with an ASUS XG32UCWMG at 240 Hz: `probe.rs`, and this window
//! with `--watch-vblank`). The draw thread presents into such a swap chain here
//! (`draw::Surface::Full`), on a Direct3D device that does nothing else (on the engine's,
//! which decodes, the desktop composed it at the monitor's fixed rate); the bar is a
//! DirectComposition layer over it, hung there only while something of the app's is shown.
//!
//! Input here is the old window's: what isn't the bar's or its menus' goes to the computer.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, MAPVK_VK_TO_VSC, MapVirtualKeyW, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use crate::app::{Msg, Pointer};
use crate::engine::{Engine, engine};
use crate::stream::{pack, pack_scale};

pub(crate) struct Full {
    pub(crate) hwnd: isize,
    /// Where the pointer last was, in pixels; whether Windows says when it leaves.
    mouse: Cell<(i32, i32)>,
    tracking: Cell<bool>,
    arrow: HCURSOR,
}

impl Full {
    fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    fn scale(&self) -> f32 {
        // SAFETY: a query on our own window.
        unsafe { GetDpiForWindow(self.hwnd()).max(96) as f32 / 96.0 }
    }
}

/// Go full screen on the monitor the app's window is on.
pub(crate) fn enter(e: &Engine) -> Result<()> {
    if e.full.borrow().is_some() {
        return Ok(());
    }
    let host = HWND(e.host.get() as *mut _);
    let (.., r, _) = crate::window::monitor_of(host);
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let (gpu, scene, shot, title) = {
        let app = e.app.borrow();
        (app.gpu.clone(), app.scene.clone(), app.args.shot.clone(), app.session.as_ref().map(|s| format!("{} - remoteman", s.target.name)).unwrap_or_else(|| "remoteman".into()))
    };
    // Whatever the computer was told is down is let go before the keyboard moves, and the one
    // draw thread that presents the session's pictures moves from the panel to this window.
    e.release_keys();
    e.hold_pointer(false);
    e.stop_drawing(&scene);
    // SAFETY: Win32 calls with structures that live through each call; the window is ours.
    let hwnd = unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(procedure),
            hInstance: instance.into(),
            lpszClassName: w!("remoteman-engine-full"),
            // The exe's own icon (its first), for the taskbar and Alt+Tab.
            hIcon: LoadIconW(Some(instance.into()), windows::core::PCWSTR(1 as _)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP, w!("remoteman-engine-full"), &windows::core::HSTRING::from(title), WS_POPUP, r.left, r.top, w, h, None, None, Some(instance.into()), None).context("the full-screen window")?
    };
    // SAFETY: a plain load.
    let arrow = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
    *e.full.borrow_mut() = Some(Rc::new(Full { hwnd: hwnd.0 as isize, mouse: Cell::new((-1, -1)), tracking: Cell::new(false), arrow }));
    let scale = e.full.borrow().as_ref().map_or(1.0, |f| f.scale());
    {
        let mut app = e.app.borrow_mut();
        app.panel = false;
        app.set_fullscreen(true);
        app.scene.client_size.store(pack(w as u32, h as u32), Ordering::Relaxed);
        app.scene.panel_scale.store(pack_scale(1.0, 1.0), Ordering::Relaxed);
        app.resized(w as f32 / scale, h as f32 / scale, scale, false);
    }
    struct SendHwnd(isize);
    let handle = SendHwnd(hwnd.0 as isize);
    match crate::draw::start_on(gpu, scene.clone(), move || {
        let handle = handle;
        crate::draw::Surface::Full(HWND(handle.0 as *mut _))
    }, shot)
    {
        Ok(drawing) => *e.drawing.borrow_mut() = Some(drawing),
        Err(err) => {
            leave(e, true);
            return Err(err);
        }
    }
    // SAFETY: calls on our windows; this process has the foreground (the person just asked).
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        let _ = ShowWindow(host, SW_HIDE);
    }
    say!("full screen: a window of its own, {w}x{h} at {:.0} % scaling; the app's window waits behind it", scale * 100.0);
    Ok(())
}

/// Back from full screen: the picture goes back to the app's panel (`back`), whose window comes
/// out again. Without `back` (the app is closing) only the window goes.
pub(crate) fn leave(e: &Engine, back: bool) {
    let Some(full) = e.full.borrow_mut().take() else { return };
    e.release_keys();
    e.hold_pointer(false);
    let scene = e.app.borrow().scene.clone();
    e.stop_drawing(&scene);
    // SAFETY: our own window.
    unsafe {
        let _ = DestroyWindow(full.hwnd());
    }
    {
        let mut app = e.app.borrow_mut();
        app.panel = true;
        app.set_fullscreen(false);
        // Nothing of the kit's is drawn in the panel.
        *app.scene.frame.lock().unwrap() = None;
    }
    if back {
        if let Err(err) = e.draw_panel() {
            say!("full screen: the panel can't show pictures again: {err:#}");
        }
        e.repanel();
        let host = HWND(e.host.get() as *mut _);
        // SAFETY: the app's window, which this process may bring to the front.
        unsafe {
            let _ = ShowWindow(host, SW_SHOW);
            let _ = SetForegroundWindow(host);
        }
    }
    say!("full screen: left; the picture is in the app's window again");
}

/// The window's title (the session's), for the taskbar and Alt+Tab.
pub(crate) fn title(e: &Engine, title: &str) {
    if let Some(full) = e.full.borrow().as_ref() {
        // SAFETY: our own window.
        unsafe {
            let _ = SetWindowTextW(full.hwnd(), &windows::core::HSTRING::from(title));
        }
    }
}

/// The cursor as it should be now, if the pointer is over the window.
pub(crate) fn set_cursor(e: &Engine) {
    let Some(full) = e.full.borrow().as_ref().map(|f| (f.hwnd(), f.mouse.get())) else { return };
    let (hwnd, (x, _)) = full;
    // SAFETY: plain queries and a cursor that outlives the call.
    unsafe {
        let mut at = POINT::default();
        if GetCursorPos(&mut at).is_ok() && WindowFromPoint(at) == hwnd && x >= 0 {
            SetCursor(wanted_cursor(e));
        }
    }
}

fn wanted_cursor(e: &Engine) -> Option<HCURSOR> {
    let full = e.full.borrow();
    let full = full.as_ref()?;
    let (x, y) = full.mouse.get();
    let scale = full.scale();
    let app = e.app.try_borrow().ok()?;
    if app.captured {
        // The computer's own is drawn over the picture instead.
        return None;
    }
    let gone = app.session.as_ref().is_some_and(|s| s.shared.cursor_gone.load(Ordering::Relaxed));
    match app.pointer(x as f32 / scale, y as f32 / scale) {
        // Until the host has sent a shape, the ordinary arrow.
        Pointer::Theirs if !e.got_shape.get() => Some(full.arrow),
        Pointer::Theirs if gone => None,
        Pointer::Theirs => e.cursor.get(),
        _ => Some(full.arrow),
    }
}

fn point(l: LPARAM) -> (i32, i32) {
    ((l.0 & 0xFFFF) as i16 as i32, ((l.0 >> 16) & 0xFFFF) as i16 as i32)
}

/// Hand something to the app (the kit's bar and menus), then do what comes back.
fn event<R>(e: &Engine, f: impl FnOnce(&mut crate::app::App) -> R) -> Option<R> {
    let result = {
        let mut app = e.app.try_borrow_mut().ok()?;
        f(&mut app)
    };
    e.publish();
    Some(result)
}

fn moved(e: &Engine, full: &Full, x: i32, y: i32) {
    full.mouse.set((x, y));
    if !full.tracking.replace(true) {
        let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: full.hwnd(), dwHoverTime: 0 };
        // SAFETY: the structure lives through the call.
        unsafe {
            let _ = TrackMouseEvent(&mut track);
        }
    }
    if e.app.borrow().captured {
        // Its movement comes as raw input (the engine's own window hears it).
        return;
    }
    let scale = full.scale();
    let taken = event(e, |app| app.mouse_move(x as f32 / scale, y as f32 / scale)).unwrap_or(true);
    // A button held on the picture keeps the pointer the computer's, wherever it goes.
    if e.buttons.get() != 0 || (!taken && e.forwarding()) {
        e.pointer(x, y);
    }
}

fn button(e: &Engine, full: &Full, which: i32, down: bool, l: LPARAM) {
    let (x, y) = point(l);
    let scale = full.scale();
    let (dx, dy) = (x as f32 / scale, y as f32 / scale);
    let bit = 1u8 << which;
    // SAFETY: focus and capture for our own window.
    unsafe {
        if e.app.borrow().captured {
            e.button(which, down, None);
            return;
        }
        if down {
            let _ = SetFocus(Some(full.hwnd()));
            let taken = if e.buttons.get() != 0 {
                false
            } else if which == 0 {
                event(e, |app| app.mouse_down(dx, dy)).unwrap_or(true)
            } else {
                e.app.try_borrow().map(|app| app.over_interface(dx, dy)).unwrap_or(true)
            };
            if !taken && e.forwarding() {
                e.button(which, true, Some((x, y)));
            }
            SetCapture(full.hwnd());
        } else {
            if e.buttons.get() & bit != 0 {
                e.button(which, false, Some((x, y)));
            } else if which == 0 {
                event(e, |app| app.mouse_up(dx, dy));
            }
            if e.buttons.get() == 0 {
                let _ = ReleaseCapture();
            }
        }
    }
}

fn wheel(e: &Engine, full: &Full, notches: f32, horizontal: bool) {
    let (x, y) = full.mouse.get();
    let scale = full.scale();
    let theirs = e.app.try_borrow().map(|app| app.forwarding() && !app.over_interface(x as f32 / scale, y as f32 / scale)).unwrap_or(false);
    if theirs {
        e.wheel(notches, horizontal);
    } else if !horizontal {
        event(e, |app| app.wheel(notches));
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: passing the message on as it came.
    let pass = || unsafe { DefWindowProcW(hwnd, msg, w, l) };
    let Some(e) = engine() else { return pass() };
    // (Held apart from the engine's cell, so that nothing stays borrowed while the app works.)
    let Some(full) = e.full.try_borrow().ok().and_then(|f| f.clone()).filter(|f| f.hwnd == hwnd.0 as isize) else { return pass() };
    // SAFETY: Win32 calls on our own window, with what Windows hands over with the message.
    unsafe {
        match msg {
            WM_MOUSEMOVE => {
                let (x, y) = point(l);
                moved(&e, &full, x, y);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                full.tracking.set(false);
                full.mouse.set((-1, -1));
                event(&e, |app| app.mouse_leave());
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                button(&e, &full, 0, true, l);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                button(&e, &full, 0, false, l);
                LRESULT(0)
            }
            WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => {
                button(&e, &full, 1, true, l);
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                button(&e, &full, 1, false, l);
                LRESULT(0)
            }
            WM_MBUTTONDOWN | WM_MBUTTONDBLCLK => {
                button(&e, &full, 2, true, l);
                LRESULT(0)
            }
            WM_MBUTTONUP => {
                button(&e, &full, 2, false, l);
                LRESULT(0)
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                wheel(&e, &full, ((w.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0, msg == WM_MOUSEHWHEEL);
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
                let down = matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN);
                let vk = w.0 as u32;
                let mut scancode = ((l.0 >> 16) & 0xFF) as u16;
                if scancode == 0 {
                    // Keys made up by software may only name the key's meaning.
                    scancode = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) as u16;
                }
                let extended = l.0 & (1 << 24) != 0;
                let repeat = down && l.0 & (1 << 30) != 0;
                // F11, Ctrl+Alt+Shift+I and the computer's keys, as on the panel.
                if e.picture_key(scancode, extended, down, repeat) {
                    LRESULT(0)
                } else if down {
                    let held = |vk: i32| GetKeyState(vk) < 0;
                    // The kit's menus and the bar take keys while one is open.
                    let used = event(&e, |app| app.key_down(vk, held(0x10), held(0x11))).unwrap_or(false);
                    if msg == WM_SYSKEYDOWN && !used { pass() } else { LRESULT(0) }
                } else if msg == WM_SYSKEYUP {
                    pass()
                } else {
                    LRESULT(0)
                }
            }
            WM_CHAR => {
                if !e.forwarding() {
                    event(&e, |app| app.char(w.0 as u16));
                }
                LRESULT(0)
            }
            // Not handing these on keeps Alt from opening the window's menu during a session.
            WM_SYSCHAR => {
                if e.forwarding() { LRESULT(0) } else { pass() }
            }
            WM_KILLFOCUS => {
                e.hold_pointer(false);
                e.release_keys();
                event(&e, |app| app.blur());
                LRESULT(0)
            }
            WM_SIZE => {
                let (cw, ch) = ((l.0 & 0xFFFF) as u32, ((l.0 >> 16) & 0xFFFF) as u32);
                if cw > 0 && ch > 0 {
                    let scale = full.scale();
                    event(&e, |app| {
                        app.scene.client_size.store(pack(cw, ch), Ordering::Relaxed);
                        app.resized(cw as f32 / scale, ch as f32 / scale, scale, false);
                        app.scene.poke();
                    });
                }
                LRESULT(0)
            }
            WM_SETCURSOR if (l.0 & 0xFFFF) as u32 == HTCLIENT => {
                SetCursor(wanted_cursor(&e));
                LRESULT(1)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let _ = windows::Win32::Graphics::Gdi::ValidateRect(Some(hwnd), None);
                e.app.borrow().scene.poke();
                LRESULT(0)
            }
            // Alt+F4 while a menu of the bar is open (otherwise it goes to the computer): out of
            // full screen, not out of the app.
            WM_CLOSE => {
                event(&e, |app| app.update(Msg::ToggleFullscreen));
                LRESULT(0)
            }
            _ => pass(),
        }
    }
}
