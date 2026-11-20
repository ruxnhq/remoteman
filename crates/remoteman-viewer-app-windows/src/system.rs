//! What the view asks of Windows itself: its window's buttons and size (Reactor has none of
//! those for a window in a session, whose title bar is the pill), whether it runs from a package,
//! and a note when it can't start.

use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::WindowsAndMessaging::*;

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut _)
}

pub fn minimise(h: isize) {
    // SAFETY: calls on our own window.
    unsafe {
        let _ = ShowWindow(hwnd(h), SW_MINIMIZE);
    }
}

/// Maximise, or back to its size if it is maximised.
pub fn maximise(h: isize) {
    // SAFETY: calls on our own window.
    unsafe {
        let _ = ShowWindow(hwnd(h), if IsZoomed(hwnd(h)).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
    }
}

/// Windows' own title bar on the window, or none (a session in a window: the pill is its title
/// bar, with the window's buttons). The frame for resizing stays.
pub fn caption(h: isize, show: bool) {
    // SAFETY: calls on our own window.
    unsafe {
        let style = GetWindowLongPtrW(hwnd(h), GWL_STYLE);
        let caption = WS_CAPTION.0 as isize;
        let new = if show { style | caption } else { style & !caption };
        if new != style {
            SetWindowLongPtrW(hwnd(h), GWL_STYLE, new);
            let _ = SetWindowPos(hwnd(h), None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

pub fn maximised(h: isize) -> bool {
    // SAFETY: a query on our own window.
    unsafe { IsZoomed(hwnd(h)).as_bool() }
}

/// Start moving the window with the mouse, from a press on the pill's words (its title bar
/// during a session).
pub fn drag(h: isize) {
    // SAFETY: the usual way to hand a press to the window's frame.
    unsafe {
        let _ = ReleaseCapture();
        let _ = SendMessageW(hwnd(h), WM_NCLBUTTONDOWN, Some(WPARAM(HTCAPTION as usize)), Some(LPARAM(0)));
    }
}

/// The window's scale (pixels to the app's units).
pub fn scale(h: isize) -> f64 {
    // SAFETY: a query on our own window.
    unsafe { GetDpiForWindow(hwnd(h)).max(96) as f64 / 96.0 }
}

/// Make the window's client area this large (in the app's units), about where it is and all of
/// it on its monitor; not while it is maximised.
pub fn resize_client(h: isize, w: f64, hh: f64) {
    if h == 0 || maximised(h) {
        return;
    }
    let k = scale(h);
    // SAFETY: calls on our own window with structures that live through each call.
    unsafe {
        let (mut now, mut client) = (RECT::default(), RECT::default());
        let _ = GetWindowRect(hwnd(h), &mut now);
        let _ = GetClientRect(hwnd(h), &mut client);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromWindow(hwnd(h), MONITOR_DEFAULTTONEAREST), &mut info);
        let work = info.rcWork;
        let (fw, fh) = ((now.right - now.left) - client.right, (now.bottom - now.top) - client.bottom);
        let (w, hh) = (((w * k).round() as i32 + fw).min(work.right - work.left), ((hh * k).round() as i32 + fh).min(work.bottom - work.top));
        let cx = (now.left + now.right) / 2;
        let x = (cx - w / 2).clamp(work.left, (work.right - w).max(work.left));
        let y = now.top.clamp(work.top, (work.bottom - hh).max(work.top));
        let _ = SetWindowPos(hwnd(h), None, x, y, w, hh, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// The package's own data folder (`LocalState`) when this runs from an MSIX package, as
/// `ApplicationData.Current.LocalFolder` has it.
pub fn package_folder() -> Option<PathBuf> {
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFamilyName;
    let mut length = 0u32;
    // SAFETY: the first call asks for the length; the buffer is as long as said.
    unsafe {
        // 15700, APPMODEL_ERROR_NO_PACKAGE: not packaged.
        if GetCurrentPackageFamilyName(&mut length, None).0 == 15700 || length == 0 {
            return None;
        }
        let mut name = vec![0u16; length as usize];
        if GetCurrentPackageFamilyName(&mut length, Some(windows::core::PWSTR(name.as_mut_ptr()))).is_err() {
            return None;
        }
        let family = String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())]);
        let local = std::env::var_os("LOCALAPPDATA")?;
        Some(PathBuf::from(local).join("Packages").join(family).join("LocalState"))
    }
}

/// A line in `remoteman-crash.log` (in the package's folder, or the temporary folder).
pub fn crash_note(line: &str, packaged: bool) {
    let dir = if packaged { package_folder() } else { None }.unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("remoteman-crash.log")).and_then(|mut f| {
        use std::io::Write;
        writeln!(f, "{line}")
    });
}

/// The scancode of a key Windows didn't give one for (made up by software).
pub fn scancode_of(vk: u32) -> u32 {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW};
    // SAFETY: a plain conversion.
    unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) }
}
