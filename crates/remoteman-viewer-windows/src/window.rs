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
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
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

/// An ordinary window that can be sized, minimised and maximised, but without Windows' own
/// system menu: with the frame extended over the whole window for the backdrop, Windows would
/// otherwise draw its own three buttons under ours.
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 & !WS_SYSMENU.0);

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
    /// The second display's window, its draw thread and what they share, while there is one.
    second: RefCell<Option<(isize, std::thread::JoinHandle<()>, std::sync::Arc<crate::stream::Shared>)>>,
    /// Which display of the session the pointer was last on (0: this window's).
    pointer_on: Cell<u8>,
    /// The pointer is held in the window and its raw movement is sent.
    captured: Cell<bool>,
    /// The clipboard's count of changes when this program last put a picture there itself.
    own_copy: Cell<u32>,
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

/// Give the window Windows 11's Mica as its backdrop, in the light or the dark tint. False
/// where Windows has none (before Windows 11 22H2).
fn backdrop(hwnd: HWND, dark: bool) -> bool {
    use windows::Win32::Graphics::Dwm::{DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
    // SAFETY: each attribute is given with its own size.
    unsafe {
        let dark = windows::core::BOOL::from(dark);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        // The backdrop is drawn wherever the frame is, so the frame is made the whole window.
        let all = windows::Win32::UI::Controls::MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &all);
        let kind = DWMSBT_MAINWINDOW;
        DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &kind as *const _ as *const _, 4).is_ok()
    }
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

/// The picture on this PC's clipboard as a PNG, if it holds one: as a program put it there
/// under the name "PNG", or made from the plain bitmap every program gives.
pub fn clipboard_get_picture() -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::BITMAPINFOHEADER;
    const CF_DIB: u32 = 8;
    let hwnd = win().map(|w| w.hwnd.get()).unwrap_or(0);
    // SAFETY: the clipboard is opened and closed here; each block is read within its size.
    unsafe {
        OpenClipboard(Some(HWND(hwnd as *mut _))).ok()?;
        let read = |format: u32| -> Option<Vec<u8>> {
            let global = HGLOBAL(GetClipboardData(format).ok()?.0);
            let at = GlobalLock(global) as *const u8;
            if at.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(at, GlobalSize(global)).to_vec();
            let _ = GlobalUnlock(global);
            Some(bytes)
        };
        let png = read(RegisterClipboardFormatW(w!("PNG"))).filter(|b| b.starts_with(&[0x89, b'P', b'N', b'G']));
        let dib = if png.is_none() { read(CF_DIB) } else { None };
        let _ = CloseClipboard();
        if png.is_some() {
            return png;
        }
        // A plain bitmap: its header, then rows from the bottom up, blue first.
        let dib = dib?;
        if dib.len() < size_of::<BITMAPINFOHEADER>() {
            return None;
        }
        let head = std::ptr::read_unaligned(dib.as_ptr() as *const BITMAPINFOHEADER);
        let (w, h, bits) = (head.biWidth as usize, head.biHeight.unsigned_abs() as usize, head.biBitCount as usize);
        // Plain colours only (or the three masks some programs put after the header).
        if w == 0 || h == 0 || !(bits == 24 || bits == 32) || !(head.biCompression == 0 || head.biCompression == 3) {
            return None;
        }
        let start = head.biSize as usize + if head.biCompression == 3 && head.biSize == 40 { 12 } else { 0 };
        let stride = (w * bits / 8).div_ceil(4) * 4;
        if dib.len() < start + stride * h {
            return None;
        }
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            let row = if head.biHeight > 0 { h - 1 - y } else { y };
            let line = &dib[start + row * stride..];
            for x in 0..w {
                let p = &line[x * bits / 8..];
                rgb.extend_from_slice(&[p[2], p[1], p[0]]);
            }
        }
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, w as u32, h as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().ok()?.write_image_data(&rgb).ok()?;
        Some(png)
    }
}

/// Put a picture (a PNG) on this PC's clipboard: as it is, under the name programs look for,
/// and as the plain bitmap every program takes.
pub fn clipboard_set_picture(data: &[u8]) -> bool {
    use windows::Win32::Graphics::Gdi::BITMAPINFOHEADER;
    const CF_DIB: u32 = 8;
    let hwnd = win().map(|w| w.hwnd.get()).unwrap_or(0);
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let Ok(mut reader) = decoder.read_info() else { return false };
    let mut pixels = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let Ok(info) = reader.next_frame(&mut pixels) else { return false };
    let (w, h) = (info.width as usize, info.height as usize);
    let step = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => 4,
        (png::ColorType::Rgb, png::BitDepth::Eight) => 3,
        _ => return false,
    };
    let head = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: h as i32, biPlanes: 1, biBitCount: 32, biSizeImage: (w * h * 4) as u32, ..Default::default() };
    let mut dib = vec![0u8; size_of::<BITMAPINFOHEADER>() + w * h * 4];
    // SAFETY: the header is written into room made for it.
    unsafe { std::ptr::write_unaligned(dib.as_mut_ptr() as *mut BITMAPINFOHEADER, head) };
    for y in 0..h {
        let from = &pixels[(h - 1 - y) * info.line_size..];
        let to = &mut dib[size_of::<BITMAPINFOHEADER>() + y * w * 4..];
        for x in 0..w {
            let p = &from[x * step..];
            to[x * 4..x * 4 + 4].copy_from_slice(&[p[2], p[1], p[0], if step == 4 { p[3] } else { 255 }]);
        }
    }
    // SAFETY: each block is allocated for its bytes, filled, and handed to the clipboard.
    unsafe {
        if OpenClipboard(Some(HWND(hwnd as *mut _))).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        for (format, bytes) in [(RegisterClipboardFormatW(w!("PNG")), data), (CF_DIB, &dib[..])] {
            if let Ok(global) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) {
                let at = GlobalLock(global) as *mut u8;
                if !at.is_null() {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len());
                    let _ = GlobalUnlock(global);
                    let _ = SetClipboardData(format, Some(HANDLE(global.0)));
                }
            }
        }
        let _ = CloseClipboard();
    }
    true
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

/// Ask for a folder, with Windows' own dialog. `None`: none was chosen.
fn pick_folder(hwnd: HWND) -> Option<PathBuf> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
    use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    // SAFETY: COM calls on this thread, which says so first; the name Windows hands back is
    // read to its end and freed.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let _ = dialog.SetTitle(w!("Save received files to"));
        dialog.SetOptions(dialog.GetOptions().ok()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        dialog.Show(Some(hwnd)).ok()?;
        let name = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as *const _));
        path
    }
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
        // No surface of Windows' own: what is on the window is the draw thread's layers.
        let hwnd = CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP, w!("remoteman"), w!("remoteman"), STYLE, CW_USEDEFAULT, CW_USEDEFAULT, 1000, 700, None, None, Some(instance.into()), None).context("create the window")?;

        app.mica = backdrop(hwnd, app.dark);
        say!("window: {}", if app.mica { "Mica behind it, kept when another window has the keyboard" } else { "no Mica on this Windows: a plain background" });
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
            second: RefCell::new(None),
            pointer_on: Cell::new(0),
            captured: Cell::new(false),
            own_copy: Cell::new(0),
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
                Effect::SetClipboardPicture(png) => {
                    if clipboard_set_picture(&png) {
                        // What this put there is not this PC's own copy: not to be sent back.
                        self.own_copy.set(GetClipboardSequenceNumber());
                    } else {
                        say!("clipboard: the picture from the computer is of a kind that isn't put on this clipboard");
                    }
                }
                Effect::PickFolder => {
                    if let Some(folder) = pick_folder(hwnd) {
                        self.event(|app| app.set_folder(&folder));
                    }
                }
                Effect::OpenSecond(title) => {
                    self.close_second();
                    let shared = self.ask(|app| app.session.as_ref().and_then(|s| s.shared.second.clone())).flatten();
                    match shared.context("no session").and_then(|shared| open_second(self, shared, &title)) {
                        Ok(()) => say!("second display: its window is open"),
                        Err(e) => {
                            say!("second display: no window: {e:#}");
                            self.event(|app| app.set_second(false, true));
                        }
                    }
                }
                Effect::CloseSecond => self.close_second(),
                Effect::ReadClipboard => self.copied(),
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

    /// This PC's clipboard changed (or sharing has just begun): text if it holds any, else a
    /// picture, goes to the computer.
    fn copied(&self) {
        // SAFETY: a plain query.
        if unsafe { GetClipboardSequenceNumber() } == self.own_copy.get() {
            return;
        }
        match clipboard_get().filter(|t| !t.is_empty()) {
            Some(text) => {
                self.event(|app| app.copied_here(text));
            }
            None => {
                if let Some(png) = clipboard_get_picture() {
                    self.event(|app| app.copied_picture(png));
                }
            }
        }
    }

    /// Take the second display's window away, and its draw thread with it.
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

    /// Hold the pointer in the picture and have Windows report its raw movement, or let it go.
    fn hold_pointer(&self, hold: bool) {
        use windows::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_REMOVE, RAWINPUTDEVICE_FLAGS, RegisterRawInputDevices};
        if self.captured.replace(hold) == hold {
            return;
        }
        let hwnd = self.hwnd();
        // SAFETY: structures that live through each call; the rectangle is our own client area.
        unsafe {
            // The mouse, as a generic desktop device.
            let device = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: if hold { RAWINPUTDEVICE_FLAGS(0) } else { RIDEV_REMOVE }, hwndTarget: if hold { hwnd } else { HWND::default() } };
            let _ = RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32);
            if hold {
                let area = self.ask(|app| app.video_pixels()).flatten().unwrap_or((0, 0, 1, 1));
                let mut at = POINT { x: area.0, y: area.1 };
                let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut at);
                let _ = ClipCursor(Some(&RECT { left: at.x, top: at.y, right: at.x + area.2, bottom: at.y + area.3 }));
                SetCursor(None);
            } else {
                let _ = ClipCursor(None);
            }
        }
        say!("pointer: {}", if hold { "captured: its raw movement goes to the computer" } else { "let go" });
        self.event(|app| {
            app.captured = hold;
            app.dirty = true;
        });
    }

    /// The pointer is on this display of the session from here on (0: the first window's).
    fn point_at(&self, index: u8) {
        if self.pointer_on.replace(index) != index {
            self.send(Event::Display(index));
        }
    }

    // To the other computer.

    fn send(&self, event: Event) {
        if let Some(Some(client)) = self.ask(|app| app.client().cloned()) {
            client.send(event);
        }
    }

    /// The pointer at a place in the client area, as the other computer is told.
    fn pointer(&self, x: i32, y: i32) {
        self.point_at(0);
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
        // The Windows keys are extended keys, whatever a program that makes them up says.
        let extended = key.flags.0 & LLKHF_EXTENDED.0 != 0 || matches!(key.vkCode, LWIN | RWIN);
        let Some(usage) = crate::keys::usage(key.scanCode as u16, extended) else { return false };
        if !down {
            // What was sent down is sent up, wherever the keyboard is by then.
            let usage = self.ask(|app| app.key_for(usage)).unwrap_or(usage);
            let ours = self.held.borrow().contains(&usage) && matches!(key.vkCode, TAB | ESCAPE | LWIN | RWIN);
            if ours {
                self.key(key.scanCode as u16, extended, false);
                say!("shortcut: key {usage:#x} let go, sent to the computer");
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
            let image = (shape.width > 0).then(|| std::sync::Arc::new(shape));
            self.event(|app| {
                app.remote_cursor = image;
                app.dirty |= app.captured;
            });
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
        if self.captured.get() {
            // The computer's own is drawn over the picture instead.
            return None;
        }
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
        if self.captured.get() {
            // Its movement comes as raw input, and the interface is out of its reach.
            return;
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
            if self.captured.get() {
                self.send(Event::Button { button, down });
                self.buttons.set(if down { self.buttons.get() | bit } else { self.buttons.get() & !bit });
                return;
            }
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
        // A captured pointer: while the picture has this PC's attention and nothing of the
        // app's own is open over it, and Ctrl+Alt+Shift hasn't given it back.
        // SAFETY: a plain query.
        let front = unsafe { GetForegroundWindow() } == self.hwnd();
        let want = front && !self.released.get() && self.ask(|app| app.forwarding() && app.store.settings.capture).unwrap_or(false);
        self.hold_pointer(want);
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

/// Open a plain window for the session's second display and start drawing its stream there.
fn open_second(win: &Win, shared: std::sync::Arc<crate::stream::Shared>, title: &str) -> Result<()> {
    let gpu = win.ask(|app| app.gpu.clone()).context("the app is busy")?;
    // SAFETY: Win32 calls with structures that live through each call.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW { lpfnWndProc: Some(second_procedure), hInstance: instance.into(), lpszClassName: w!("remoteman-second"), hCursor: win.arrow, ..Default::default() };
        // (Registered already, the second time: that is fine.)
        RegisterClassW(&class);
        let mut r = RECT { left: 0, top: 0, right: 1280, bottom: 720 };
        let _ = AdjustWindowRectEx(&mut r, WS_OVERLAPPEDWINDOW, false, WS_EX_NOREDIRECTIONBITMAP);
        let hwnd = CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP, w!("remoteman-second"), &windows::core::HSTRING::from(title), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, r.right - r.left, r.bottom - r.top, None, None, Some(instance.into()), None).context("create the second window")?;
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let scene = shared.scene.clone();
        scene.quit.store(false, Ordering::Relaxed);
        scene.client_size.store(pack(client.right.max(1) as u32, client.bottom.max(1) as u32), Ordering::Relaxed);
        *scene.session.lock().unwrap() = Some(shared.clone());
        let drawing = crate::draw::start(gpu, scene, hwnd, None)?;
        *win.second.borrow_mut() = Some((hwnd.0 as isize, drawing, shared));
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    Ok(())
}

/// The second display's window: its picture is all there is in it, and everything done in it
/// is the other computer's, on that display.
unsafe extern "system" fn second_procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: passing the message on as it came.
    let pass = || unsafe { DefWindowProcW(hwnd, msg, w, l) };
    let Some(win) = win() else { return pass() };
    let Some(shared) = win.second.try_borrow().ok().and_then(|s| s.as_ref().filter(|s| s.0 == hwnd.0 as isize).map(|s| s.2.clone())) else { return pass() };
    // A place in this window as fractions of the second display, allowing for the bands
    // that keep the picture's shape.
    let pointer = |l: LPARAM| {
        let (x, y) = point(l);
        let (vw, vh) = crate::stream::unpack(shared.stream_size.load(Ordering::Relaxed));
        let (cw, ch) = crate::stream::unpack(shared.scene.client_size.load(Ordering::Relaxed));
        if vw == 0 || vh == 0 || cw == 0 || ch == 0 {
            return;
        }
        let (vw, vh, cw, ch) = (vw as f32, vh as f32, cw as f32, ch as f32);
        let fit = (cw / vw).min(ch / vh);
        let (dw, dh) = (vw * fit, vh * fit);
        win.point_at(1);
        win.send(Event::PointerTo { x: ((x as f32 - ((cw - dw) / 2.0).floor()) / dw).clamp(0.0, 1.0), y: ((y as f32 - ((ch - dh) / 2.0).floor()) / dh).clamp(0.0, 1.0) });
    };
    let button = |button: Button, down: bool| {
        pointer(l);
        win.send(Event::Button { button, down });
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
                let k = if win.ask(|app| app.store.settings.scroll_reversed).unwrap_or(false) { -NOTCH } else { NOTCH };
                win.point_at(1);
                win.send(if msg == WM_MOUSEHWHEEL { Event::Scroll { dx: notches * k, dy: 0.0 } } else { Event::Scroll { dx: 0.0, dy: -notches * k } });
            }
            // Keys typed in this window go to the computer as the first window's do.
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
                let mut scancode = ((l.0 >> 16) & 0xFF) as u16;
                if scancode == 0 {
                    scancode = MapVirtualKeyW(w.0 as u32, MAPVK_VK_TO_VSC) as u16;
                }
                win.key(scancode, l.0 & (1 << 24) != 0, matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN));
            }
            WM_SYSCHAR => {}
            WM_KILLFOCUS => win.release_keys(),
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
            // Closing the window takes the display away on the computer too.
            WM_CLOSE => {
                win.event(|app| app.set_second(false, true));
            }
            _ => return pass(),
        }
    }
    LRESULT(0)
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
            // Windows takes Mica away from a window that hasn't the keyboard, leaving a flat
            // colour. It goes by whether the frame is drawn as active: so it always is.
            WM_NCACTIVATE => return DefWindowProcW(hwnd, msg, WPARAM(1), l),
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
                // A session's bar needs more room than the other screens do.
                let session = win.ask(|app| app.session.is_some() && app.screen == crate::app::Screen::Session).unwrap_or(false);
                info.ptMinTrackSize = POINT { x: ((if session { 760.0 } else { 560.0 }) * scale) as i32, y: (400.0 * scale) as i32 };
            }
            WM_MOUSEMOVE => {
                let (x, y) = point(l);
                win.moved(x, y, true);
            }
            // A captured pointer's raw movement.
            WM_INPUT => {
                use windows::Win32::UI::Input::{GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTHEADER, RID_INPUT, RIM_TYPEMOUSE};
                let mut input = RAWINPUT::default();
                let mut size = size_of::<RAWINPUT>() as u32;
                let got = GetRawInputData(HRAWINPUT(l.0 as *mut _), RID_INPUT, Some(&mut input as *mut _ as *mut _), &mut size, size_of::<RAWINPUTHEADER>() as u32);
                if win.captured.get() && got != u32::MAX && input.header.dwType == RIM_TYPEMOUSE.0 {
                    let mouse = input.data.mouse;
                    // (Bit 0 set: a device that says where it is, not how far it moved.)
                    if mouse.usFlags.0 & 1 == 0 && (mouse.lLastX != 0 || mouse.lLastY != 0) {
                        win.point_at(0);
                        win.send(Event::PointerBy { dx: mouse.lLastX as f32, dy: mouse.lLastY as f32 });
                    }
                }
                return pass();
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
                    // (Not the three keys alone: this is no giving back of the keyboard.)
                    win.chord.set(false);
                    if l.0 & (1 << 30) == 0 {
                        win.event(|app| app.update(Msg::ToggleFigures));
                    }
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
                win.hold_pointer(false);
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
                    if win.ask(|app| app.mica).unwrap_or(false) {
                        backdrop(hwnd, dark);
                    }
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
                    win.copied();
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
                win.close_second();
                win.event(|app| {
                    app.closing();
                    app.scene.quit.store(true, Ordering::Relaxed);
                });
                let _ = DestroyWindow(hwnd);
            }
            WM_DESTROY => PostQuitMessage(0),
            _ => return pass(),
        }
    }
    LRESULT(0)
}
