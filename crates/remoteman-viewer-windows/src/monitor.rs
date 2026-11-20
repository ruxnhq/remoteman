//! What the monitor under the window can do that the picture cares about beyond its size:
//! whether it varies its refresh to follow the frames (variable refresh rate), read from its
//! EDID, and what that makes of the "Variable refresh rate" switch. (Whether it is in HDR is
//! `gpu::screen_hdr`.)
//!
//! Windows has no call that says a monitor does variable refresh, let alone that it is switched
//! on for it (that is the graphics driver's setting, G-SYNC or FreeSync). The monitor's EDID
//! says whether it *can*: AMD's FreeSync block or the HDMI Forum's VRR range in its CTA-861
//! extension, or, for DisplayPort Adaptive-Sync, a range of refresh rates with continuous
//! frequencies allowed (what Linux's amdgpu goes by). So "can" is all this knows.

// The engine's (the WinUI 3 app's); the old program's window doesn't present that way.
#![allow(dead_code)]

use std::sync::Mutex;

/// The range of refresh rates the monitor with this EDID varies within, if it says it does.
pub fn vrr_range(edid: &[u8]) -> Option<(u32, u32)> {
    if edid.len() < 128 || edid[0..8] != [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0] {
        return None;
    }
    // The CTA-861 extensions' vendor blocks say it outright.
    for block in edid.chunks_exact(128).skip(1) {
        if block[0] != 0x02 {
            continue;
        }
        let end = (block[2] as usize).clamp(4, 127);
        let mut i = 4;
        while i < end {
            let (tag, len) = (block[i] >> 5, (block[i] & 31) as usize);
            let data = &block[i..(i + 1 + len).min(127)];
            if tag == 3 && data.len() >= 4 {
                let oui = (data[3] as u32) << 16 | (data[2] as u32) << 8 | data[1] as u32;
                // AMD's FreeSync block: its range at bytes 6 and 7.
                if oui == 0x00001a && data.len() >= 8 && data[6] > 0 && data[7] > data[6] {
                    return Some((data[6] as u32, data[7] as u32));
                }
                // The HDMI Forum's: VRRmin in six bits, VRRmax in ten (0: none).
                if oui == 0xc45dd8 && data.len() >= 11 {
                    let (min, max) = ((data[9] & 0x3f) as u32, ((data[9] as u32 & 0xc0) << 2) | data[10] as u32);
                    if max > min && min > 0 {
                        return Some((min, max));
                    }
                }
            }
            i += 1 + len;
        }
    }
    // DisplayPort Adaptive-Sync: EDID 1.4, a digital input, continuous frequencies, and a
    // range-limits-only descriptor wider than 10 Hz.
    let (version, revision, digital, continuous) = (edid[18], edid[19], edid[20] & 0x80 != 0, edid[24] & 1 != 0);
    if version != 1 || revision < 4 || !digital || !continuous {
        return None;
    }
    for d in edid[54..126].chunks_exact(18) {
        if d[0] == 0 && d[1] == 0 && d[3] == 0xfd && d[10] == 1 {
            let offsets = d[4] & 3;
            let min = d[5] as u32 + if offsets == 3 { 255 } else { 0 };
            let max = d[6] as u32 + if offsets >= 2 { 255 } else { 0 };
            if max > min + 10 {
                return Some((min, max));
            }
        }
    }
    None
}

/// The EDID of the monitor showing `hwnd`, from where Windows keeps it.
fn edid_of(hwnd: windows::Win32::Foundation::HWND) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::{DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICEW, EnumDisplayDevicesW, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow};
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY, RegGetValueW};
    use windows::Win32::UI::WindowsAndMessaging::EDD_GET_DEVICE_INTERFACE_NAME;
    use windows::core::{HSTRING, PCWSTR, w};
    // SAFETY: structures that say their own size and live through each call; the buffer is as
    // large as said.
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() {
            return None;
        }
        // The monitor on that output: "\\?\DISPLAY#AUSAA06#5&56b7e85&0&UID4353#{…}".
        let mut path = None;
        for n in 0..4 {
            let mut device = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
            if !EnumDisplayDevicesW(PCWSTR(info.szDevice.as_ptr()), n, &mut device, EDD_GET_DEVICE_INTERFACE_NAME).as_bool() {
                break;
            }
            if device.StateFlags.0 & DISPLAY_DEVICE_ACTIVE.0 != 0 {
                let len = device.DeviceID.iter().position(|c| *c == 0).unwrap_or(device.DeviceID.len());
                path = Some(String::from_utf16_lossy(&device.DeviceID[..len]));
                break;
            }
        }
        let path = path?;
        let parts: Vec<&str> = path.trim_start_matches(r"\\?\").split('#').collect();
        if parts.len() < 3 {
            return None;
        }
        let key = HSTRING::from(format!(r"SYSTEM\CurrentControlSet\Enum\{}\{}\{}\Device Parameters", parts[0], parts[1], parts[2]));
        let mut buf = vec![0u8; 1024];
        let mut size = buf.len() as u32;
        RegGetValueW(HKEY_LOCAL_MACHINE, &key, w!("EDID"), RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)).ok().ok()?;
        buf.truncate(size as usize);
        Some(buf)
    }
}

/// The variable refresh range of the monitor showing `hwnd` (`None`: it doesn't say it has
/// one), looked up again only when the window is on another monitor.
pub fn vrr_of(hwnd: windows::Win32::Foundation::HWND) -> Option<(u32, u32)> {
    static SEEN: Mutex<Option<(isize, Option<(u32, u32)>)>> = Mutex::new(None);
    // SAFETY: a plain query.
    let monitor = unsafe { windows::Win32::Graphics::Gdi::MonitorFromWindow(hwnd, windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST) }.0 as isize;
    let mut seen = SEEN.lock().unwrap();
    if let Some((m, range)) = *seen {
        if m == monitor {
            return range;
        }
    }
    let range = edid_of(hwnd).and_then(|e| vrr_range(&e));
    say!("monitor: {}", range.map_or("its EDID says nothing of variable refresh".to_owned(), |(lo, hi)| format!("variable refresh from {lo} to {hi} Hz, as its EDID says")));
    *seen = Some((monitor, range));
    range
}

/// For tests (`--watch-vblank`): the monitor's own refreshes, as DXGI's vertical blank on the
/// output under `hwnd`, every five seconds in the log. A monitor at a fixed rate blanks once a
/// refresh whatever is presented; one varying its refresh for pictures presented at 100 a
/// second blanks about every 10 ms. (The swap chain's own figures can't tell: for a picture
/// drawn by the compositor they give the time of presenting, not of showing.)
pub fn watch_vblanks(hwnd: isize, scene: std::sync::Arc<crate::stream::Scene>) {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput};
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
    let output = move || -> Option<IDXGIOutput> {
        // SAFETY: DXGI queries with structures that live through each call.
        unsafe {
            let monitor = MonitorFromWindow(windows::Win32::Foundation::HWND(hwnd as *mut _), MONITOR_DEFAULTTONEAREST);
            let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
            let mut a = 0;
            while let Ok(adapter) = factory.EnumAdapters1(a) {
                let mut o = 0;
                while let Ok(output) = adapter.EnumOutputs(o) {
                    if output.GetDesc().is_ok_and(|d| d.Monitor == monitor) {
                        return Some(output);
                    }
                    o += 1;
                }
                a += 1;
            }
            None
        }
    };
    let _ = std::thread::Builder::new().name("vblank".into()).spawn(move || {
        while !scene.quit.load(Ordering::Relaxed) {
            let Some(out) = output() else {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            };
            let (start, mut last, mut gaps) = (Instant::now(), None::<Instant>, Vec::new());
            while start.elapsed() < Duration::from_secs(5) {
                // SAFETY: a call on the output.
                if unsafe { out.WaitForVBlank() }.is_err() {
                    std::thread::sleep(Duration::from_millis(100));
                    break;
                }
                let now = Instant::now();
                if let Some(l) = last {
                    gaps.push(now.duration_since(l).as_secs_f64() * 1000.0);
                }
                last = Some(now);
            }
            if gaps.is_empty() {
                continue;
            }
            let hz = scene.refresh_hz.load(Ordering::Relaxed).max(1) as f64;
            let period = 1000.0 / hz;
            let fixed = gaps.iter().filter(|g| (**g - period).abs() < 0.5).count();
            let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
            let longest = gaps.iter().cloned().fold(0.0, f64::max);
            say!(
                "vblank: {} in 5 s, {mean:.2} ms apart on average, {longest:.2} at most; {fixed} one fixed refresh ({period:.2} ms) apart; {}",
                gaps.len() + 1,
                if scene.tearing.load(Ordering::Relaxed) { "presenting with tearing allowed" } else { "presenting at the refresh" }
            );
        }
    });
}

unsafe extern "system" fn flip_procedure(hwnd: windows::Win32::Foundation::HWND, msg: u32, w: windows::Win32::Foundation::WPARAM, l: windows::Win32::Foundation::LPARAM) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::*;
    match msg {
        // The pointer is the app's window's, under this one on the same thread: it goes there.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            // SAFETY: our own window.
            let _ = unsafe { windows::Win32::Graphics::Gdi::ValidateRect(Some(hwnd), None) };
            LRESULT(0)
        }
        // SAFETY: passing the message on as it came.
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

/// The window the picture goes to while variable refresh is on: borderless, over the whole of
/// the monitor `owner` is on, owned by it and so above it. It takes neither the keyboard nor
/// the pointer (both stay with `owner`, under it on the same thread), so the app works as it
/// did; the app's own things over the picture (its bar, menus) are under it, so it is hidden
/// while there are any. Made hidden; `show_flip_window` puts it up.
pub fn flip_window(owner: windows::Win32::Foundation::HWND) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;
    // SAFETY: Win32 calls with structures that live through each call.
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = WNDCLASSW { lpfnWndProc: Some(flip_procedure), hInstance: instance.into(), lpszClassName: w!("remoteman-engine-vrr"), ..Default::default() };
        RegisterClassW(&class);
        CreateWindowExW(WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP, w!("remoteman-engine-vrr"), w!("remoteman picture"), WS_POPUP, 0, 0, 8, 8, Some(owner), None, Some(instance.into()), None).ok()
    }
}

/// Put the window up over the whole of `owner`'s monitor, or take it down.
pub fn show_flip_window(hwnd: windows::Win32::Foundation::HWND, owner: windows::Win32::Foundation::HWND, show: bool) {
    use windows::Win32::UI::WindowsAndMessaging::*;
    // SAFETY: calls on our own window.
    unsafe {
        if show {
            let (.., r, _) = crate::window::monitor_of(owner);
            let _ = SetWindowPos(hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        } else {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// What the "Variable refresh rate" switch comes to: whether each frame is presented as it
/// arrives (with tearing allowed, so a monitor that varies its refresh shows it at once), and
/// if not, why not, in the words the app may show.
pub fn variable_refresh(on: bool, fullscreen: bool, range: Option<(u32, u32)>, tearing: bool) -> (bool, &'static str) {
    if !on {
        (false, "Off")
    } else if range.is_none() {
        (false, "This monitor doesn't vary its refresh rate")
    } else if !tearing {
        (false, "Windows doesn't allow it here")
    } else if !fullscreen {
        (false, "In full screen only")
    } else {
        (true, "Each frame is shown when it arrives")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A base block with a range-limits descriptor (and the checksum left wrong, which isn't
    /// looked at).
    fn base(version: (u8, u8), input: u8, features: u8, range: Option<(u8, u8, u8)>) -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[0..8].copy_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]);
        (e[18], e[19], e[20], e[24]) = (version.0, version.1, input, features);
        if let Some((min, max, flags)) = range {
            let d = 54 + 18;
            e[d + 3] = 0xfd;
            (e[d + 5], e[d + 6], e[d + 7], e[d + 8], e[d + 10]) = (min, max, 30, 160, flags);
        }
        e
    }

    fn with_cta(mut e: Vec<u8>, blocks: &[&[u8]]) -> Vec<u8> {
        e[126] = 1;
        let mut x = vec![0u8; 128];
        x[0] = 0x02;
        x[1] = 3;
        let mut at = 4;
        for b in blocks {
            x[at..at + b.len()].copy_from_slice(b);
            at += b.len();
        }
        x[2] = at as u8;
        e.extend(x);
        e
    }

    #[test]
    fn freesync_block_gives_its_range() {
        // As this PC's ASUS XG32UCWMG has it: 48 to 240 Hz.
        let amd = [0x68, 0x1a, 0x00, 0x00, 0x01, 0x01, 48, 240, 0x00];
        assert_eq!(vrr_range(&with_cta(base((1, 4), 0xb5, 0x3b, Some((48, 240, 1))), &[&amd])), Some((48, 240)));
    }

    #[test]
    fn hdmi_forum_block_gives_its_range() {
        // VRRmin 48, VRRmax 240 (no high bits).
        let hf = [0x6a, 0xd8, 0x5d, 0xc4, 0x01, 0x78, 0x80, 0x00, 0x00, 48, 240];
        assert_eq!(vrr_range(&with_cta(base((1, 3), 0x80, 0x0a, None), &[&hf])), Some((48, 240)));
        // A short block (no VRR bytes), as this PC's ROG GJALLAR has: nothing.
        let short = [0x67, 0xd8, 0x5d, 0xc4, 0x01, 0x78, 0x80, 0x00];
        assert_eq!(vrr_range(&with_cta(base((1, 3), 0x80, 0xfa, Some((24, 144, 1))), &[&short])), None);
    }

    #[test]
    fn displayport_range_counts_only_with_continuous_frequencies() {
        assert_eq!(vrr_range(&base((1, 4), 0xa5, 0x01, Some((40, 144, 1)))), Some((40, 144)));
        // Not continuous, or EDID 1.3, or a GTF range: a fixed-rate monitor's limits.
        assert_eq!(vrr_range(&base((1, 4), 0xa5, 0x00, Some((40, 144, 1)))), None);
        assert_eq!(vrr_range(&base((1, 3), 0xa5, 0x01, Some((40, 144, 1)))), None);
        assert_eq!(vrr_range(&base((1, 4), 0xa5, 0x01, Some((40, 144, 0)))), None);
        assert_eq!(vrr_range(&base((1, 4), 0xa5, 0x01, Some((56, 61, 1)))), None);
        assert_eq!(vrr_range(&[0u8; 64]), None);
    }

    #[test]
    fn the_switch_needs_full_screen_a_capable_monitor_and_tearing() {
        let vrr = Some((48, 240));
        assert_eq!(variable_refresh(true, true, vrr, true).0, true);
        assert_eq!(variable_refresh(false, true, vrr, true), (false, "Off"));
        assert_eq!(variable_refresh(true, false, vrr, true), (false, "In full screen only"));
        assert!(!variable_refresh(true, true, None, true).0);
        assert!(!variable_refresh(true, true, vrr, false).0);
    }
}
