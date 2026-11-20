//! The display a client is given.
//!
//! A monitor of its own, where the driver allows a second one ([`Screen::give`]): the driver's
//! monitor 1, made when the first device asks, brought to the device's size and rate, made part
//! of the desktop where the device asked for it (beside the others, as the main display, or as
//! the only one) and taken out of the desktop again when the device leaves. The driver as
//! Guesthouse builds it can't take one monitor away (its DESTROYMONITOR does nothing), so the
//! monitor stays plugged in, as a screen switched off would, and only its place on the desktop
//! comes and goes. The driver allows a second monitor when its settings file says
//! `<count>2</count>` and its device has been started since.
//!
//! Otherwise ([`Screen::fit`]) the driver's one monitor, switched to the client's size and
//! refresh rate where the driver and Windows allow, and put back when the client goes. And
//! for a device that asks to mirror, that monitor as it is.
//!
//! The driver has one list of modes for all its monitors, and adding a mode to it re-creates
//! them all: a device with a size not yet listed makes every driver monitor blink once.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows::Win32::Graphics::Gdi::{
    CDS_FULLSCREEN, CDS_NORESET, CDS_SET_PRIMARY, CDS_TYPE, CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DM_POSITION, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICE_PRIMARY_DEVICE, DISPLAY_DEVICEW, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT,
    DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplayDevicesW, EnumDisplaySettingsW,
};
use windows::Win32::Graphics::Gdi::UpdateWindow;
use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, SW_SHOWNOACTIVATE, ShowWindow, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP};
use windows::core::{HSTRING, PCWSTR, w};

use crate::ccd;
use crate::vdd::{self, Channel, Driver};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
}

/// Where a monitor sits on the Windows desktop, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// How far the host may go to give a client its own size (`--mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Leave the monitor as it is and stream that.
    Keep,
    /// Switch to the client's size if the monitor already offers it.
    Listed,
    /// As `Listed`, and otherwise have the driver add the mode (which re-creates the monitor).
    Any,
}

/// One display adapter's output as Windows lists it.
#[derive(Debug, Clone)]
pub struct Output {
    /// `\\.\DISPLAY1`: what the display calls take.
    pub device: String,
    pub description: String,
    pub id: String,
    pub primary: bool,
}

fn text(wide: &[u16]) -> String {
    String::from_utf16_lossy(&wide[..wide.iter().position(|&c| c == 0).unwrap_or(wide.len())])
}

/// Every display output that has monitors of the driver's, with each such monitor's number
/// with the driver and whether it is the one showing there now. Windows names outputs anew
/// when the driver re-creates its monitors; what stays is a monitor's own number in its device
/// path ("UID256" for the driver's first). An output that is no part of the desktop lists
/// every monitor it could show, so two such outputs list the same ones.
fn driver_outputs() -> Vec<(Output, Vec<(u32, bool)>)> {
    let mut all = Vec::new();
    for i in 0.. {
        let mut d = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        // SAFETY: `d` is sized as the call wants.
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut d, 0) }.as_bool() {
            break;
        }
        let mut monitors = Vec::new();
        for j in 0.. {
            let mut m = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
            // 1: the monitor's device path instead of its hardware id.
            // SAFETY: as above; the name is a NUL-terminated array of the call before.
            if !unsafe { EnumDisplayDevicesW(PCWSTR(d.DeviceName.as_ptr()), j, &mut m, 1) }.as_bool() {
                break;
            }
            let path = text(&m.DeviceID).to_lowercase();
            let number = path.split("uid").nth(1).and_then(|rest| rest.chars().take_while(char::is_ascii_digit).collect::<String>().parse::<u32>().ok());
            if let (Some(number @ 256..=271), true) = (number, vdd::MONITOR_HINTS.iter().any(|h| path.contains(h))) {
                // (`DISPLAY_DEVICE_ACTIVE`.)
                monitors.push((number - 256, m.StateFlags.0 & 1 != 0));
            }
        }
        if !monitors.is_empty() {
            all.push((Output { device: text(&d.DeviceName), description: text(&d.DeviceString), id: text(&d.DeviceID), primary: d.StateFlags.0 & DISPLAY_DEVICE_PRIMARY_DEVICE.0 != 0 }, monitors));
        }
    }
    all
}

/// The output for the driver's monitor number `index`: the one showing it, or, while that
/// monitor is no part of the desktop, an output that could (a different one for each such
/// monitor). Which monitor an output shows once it has joined the desktop is Windows' choice:
/// ask [`driver_index`] afterwards.
pub fn driver_output(index: u32) -> Option<Output> {
    let all = driver_outputs();
    if let Some((o, _)) = all.iter().find(|(_, m)| m.contains(&(index, true))) {
        return Some(o.clone());
    }
    let showing = |i: u32| all.iter().any(|(_, m)| m.contains(&(i, true)));
    // Among the outputs showing nothing: the first for the lowest monitor that isn't shown,
    // the second for the next, and so on.
    let before = (0..index).filter(|i| !showing(*i)).count();
    all.iter().filter(|(_, m)| m.iter().all(|(_, on)| !on) && m.iter().any(|(i, _)| *i == index)).nth(before).map(|(o, _)| o.clone())
}

/// The driver's number for the monitor this output is showing.
pub fn driver_index(device: &str) -> Option<u32> {
    driver_outputs().into_iter().find(|(o, _)| o.device == device).and_then(|(_, m)| m.into_iter().find(|(_, on)| *on).map(|(i, _)| i))
}

/// Is this output part of the desktop?
pub fn attached(device: &str) -> bool {
    outputs().iter().any(|o| o.device == device)
}

/// Write down where an output is to be on the desktop, with what mode, or (`None`) that it is
/// to be no part of it. Nothing changes until [`apply`].
fn stage(device: &str, to: Option<(Mode, i32, i32, bool)>) -> Result<()> {
    // Display changes are taken from the desktop that takes input (see `crate::desk`).
    crate::desk::follow();
    let mut dm = devmode();
    let mut flags = CDS_UPDATEREGISTRY | CDS_NORESET;
    dm.dmFields = DM_POSITION | DM_PELSWIDTH | DM_PELSHEIGHT;
    if let Some((mode, x, y, primary)) = to {
        (dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency) = (mode.width, mode.height, mode.refresh);
        dm.dmFields |= DM_DISPLAYFREQUENCY;
        dm.Anonymous1.Anonymous2.dmPosition.x = x;
        dm.Anonymous1.Anonymous2.dmPosition.y = y;
        if primary {
            flags |= CDS_SET_PRIMARY;
        }
    }
    // SAFETY: a whole DEVMODE; no window, no parameters.
    let result = unsafe { ChangeDisplaySettingsExW(&HSTRING::from(device), Some(&dm), None, flags, None) };
    if result != DISP_CHANGE_SUCCESSFUL {
        bail!("Windows refused to place {device} as {to:?} (ChangeDisplaySettingsEx: {})", result.0);
    }
    Ok(())
}

/// Make the desktop what was staged.
fn apply() -> Result<()> {
    crate::desk::follow();
    // SAFETY: no device and no mode means "apply what the registry holds".
    let result = unsafe { ChangeDisplaySettingsExW(PCWSTR::null(), None, None, CDS_TYPE(0), None) };
    if result != DISP_CHANGE_SUCCESSFUL {
        bail!("Windows wouldn't rearrange the desktop (ChangeDisplaySettingsEx: {})", result.0);
    }
    Ok(())
}

/// For tests and repairs: take an output out of the desktop.
pub fn detach(device: &str) -> Result<()> {
    stage(device, None)?;
    apply()
}

/// For tests and repairs: the driver's first monitor in this mode, as the main display, kept.
pub fn set_first(mode: Mode) -> Result<()> {
    let first = driver_output(0).context("the driver's first monitor isn't there")?;
    stage(&first.device, Some((mode, 0, 0, true)))?;
    apply()
}

/// Where a device's monitor goes among the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// An extra display to the right of the driver's first monitor.
    Beside,
    /// The main display, the first monitor kept on to its right.
    Main,
    /// The first monitor off while the device is here.
    Only,
}

/// The outputs that are part of the desktop.
pub fn outputs() -> Vec<Output> {
    let mut all = Vec::new();
    for i in 0.. {
        let mut d = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        // SAFETY: `d` is sized as the call wants.
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut d, 0) }.as_bool() {
            break;
        }
        if d.StateFlags.0 & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP.0 != 0 {
            all.push(Output { device: text(&d.DeviceName), description: text(&d.DeviceString), id: text(&d.DeviceID), primary: d.StateFlags.0 & DISPLAY_DEVICE_PRIMARY_DEVICE.0 != 0 });
        }
    }
    all
}

/// The driver's monitor among the outputs: the one whose adapter looks like the driver's, else
/// the only display there is (in a guest where the driver's monitor is the only display, the
/// same). Never one of several: on a PC with screens of its own, that would be one of them,
/// switched to a device's size while the picture still came from the driver's monitor.
pub fn find_output() -> Result<(Output, bool)> {
    let all = outputs();
    // The hints in their order, so that this driver's own hardware id wins over a name it shares.
    let by_name = vdd::ADAPTER_HINTS.iter().find_map(|h| all.iter().find(|o| format!("{} {}", o.description, o.id).to_lowercase().contains(h)));
    match by_name.or_else(|| if all.len() == 1 { all.first() } else { None }) {
        Some(o) => Ok((o.clone(), by_name.is_some())),
        None if all.is_empty() => bail!("Windows lists no display that is part of the desktop (is this process in a session with a desktop?)"),
        None => bail!("the display driver's monitor isn't part of the desktop, and this computer's own screens aren't used in its place (turn the driver's display back on in Settings > System > Display)"),
    }
}

fn devmode() -> DEVMODEW {
    DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() }
}

/// The mode an output is in now, and where it sits on the desktop.
pub fn current(device: &str) -> Result<(Mode, Area)> {
    let mut dm = devmode();
    // SAFETY: `dm` is sized as the call wants.
    if !unsafe { EnumDisplaySettingsW(&HSTRING::from(device), ENUM_CURRENT_SETTINGS, &mut dm) }.as_bool() {
        bail!("Windows wouldn't say what mode {device} is in");
    }
    // SAFETY: for a display device this half of the union is the one filled in.
    let at = unsafe { dm.Anonymous1.Anonymous2.dmPosition };
    let mode = Mode { width: dm.dmPelsWidth, height: dm.dmPelsHeight, refresh: dm.dmDisplayFrequency };
    Ok((mode, Area { left: at.x, top: at.y, width: mode.width, height: mode.height }))
}

/// Every mode an output offers, without repeats (Windows lists each once per colour depth).
pub fn modes(device: &str) -> Vec<Mode> {
    let (mut all, device) = (Vec::new(), HSTRING::from(device));
    for i in 0.. {
        let mut dm = devmode();
        // SAFETY: as above.
        if !unsafe { EnumDisplaySettingsW(&device, ENUM_DISPLAY_SETTINGS_MODE(i), &mut dm) }.as_bool() {
            break;
        }
        let mode = Mode { width: dm.dmPelsWidth, height: dm.dmPelsHeight, refresh: dm.dmDisplayFrequency };
        if !all.contains(&mode) {
            all.push(mode);
        }
    }
    all
}

/// Switch an output to a mode for as long as this process wants it: nothing is written to the
/// registry, so [`restore`] brings back what was there, and the change is marked as temporary
/// (`CDS_FULLSCREEN`), which has Windows undo it by itself should this process die first.
fn switch(device: &str, mode: Mode) -> Result<()> {
    crate::desk::follow();
    let mut dm = devmode();
    (dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency) = (mode.width, mode.height, mode.refresh);
    dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    // SAFETY: a whole DEVMODE; no window, no parameters.
    let result = unsafe { ChangeDisplaySettingsExW(&HSTRING::from(device), Some(&dm), None, CDS_FULLSCREEN, None) };
    if result != DISP_CHANGE_SUCCESSFUL {
        bail!("Windows refused {}x{} at {} Hz on {device} (ChangeDisplaySettingsEx: {})", mode.width, mode.height, mode.refresh, result.0);
    }
    Ok(())
}

/// Back to the mode the registry holds for this output.
fn restore(device: &str) -> Result<()> {
    crate::desk::follow();
    // SAFETY: no mode means "the stored one".
    let result = unsafe { ChangeDisplaySettingsExW(&HSTRING::from(device), None, None, CDS_TYPE(0), None) };
    if result != DISP_CHANGE_SUCCESSFUL {
        bail!("Windows wouldn't put {device} back (ChangeDisplaySettingsEx: {})", result.0);
    }
    Ok(())
}

/// Of the refresh rates offered at one size, the one to use for a client that wants `want`:
/// that rate, else the highest below it (the client shows every frame), else the lowest above.
fn nearest_refresh(offered: impl Iterator<Item = u32> + Clone, want: u32) -> Option<u32> {
    offered.clone().filter(|r| *r <= want).max().or_else(|| offered.min())
}

/// Move every window whose middle is in `area` onto the main display, as far in as it was on
/// `area` and kept inside the main display's work area. For a device's display on a PC with
/// screens of its own: Windows puts back the windows it remembers were on a monitor when that
/// monitor comes back, so the owner's windows would turn up on a device's display; and it can
/// leave a monitor's windows off-screen when the monitor goes. So the device's display starts
/// empty, and nothing stays behind where it was. The taskbars and the desktop itself are left
/// alone, as are windows owned by others (they go with their owner). Says how many moved.
fn sweep(area: Area) -> usize {
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetClassNameW, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, SW_MAXIMIZE, SW_RESTORE, SWP_NOACTIVATE, SWP_NOSIZE,
        SWP_NOZORDER, SetWindowPos,
    };
    crate::desk::follow();
    unsafe extern "system" fn each(window: HWND, found: LPARAM) -> windows::core::BOOL {
        // SAFETY: `found` is the Vec handed to EnumWindows below, alive for the call.
        unsafe { (*(found.0 as *mut Vec<HWND>)).push(window) };
        true.into()
    }
    let mut all: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into the Vec, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut all as *mut Vec<HWND> as isize)) };
    // SAFETY: plain calls; the monitor info is sized as asked.
    let work = unsafe {
        let main = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(main, &mut info).as_bool() {
            return 0;
        }
        info.rcWork
    };
    let me = std::process::id();
    let inside = |x: i32, y: i32| x >= area.left && x < area.left + area.width as i32 && y >= area.top && y < area.top + area.height as i32;
    let mut moved = 0;
    for window in all {
        // SAFETY: plain calls on a window handle that may have gone meanwhile (then they fail).
        unsafe {
            if !IsWindowVisible(window).as_bool() || IsIconic(window).as_bool() || GetWindow(window, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
                continue;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, Some(&mut pid));
            if pid == me {
                continue;
            }
            let mut class = [0u16; 64];
            let n = GetClassNameW(window, &mut class) as usize;
            let class = String::from_utf16_lossy(&class[..n]);
            if matches!(class.as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW") {
                continue;
            }
            let mut r = RECT::default();
            if GetWindowRect(window, &mut r).is_err() || !inside((r.left + r.right) / 2, (r.top + r.bottom) / 2) {
                continue;
            }
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let x = (work.left + (r.left - area.left)).min(work.right - w).max(work.left);
            let y = (work.top + (r.top - area.top)).min(work.bottom - h).max(work.top);
            let maximized = IsZoomed(window).as_bool();
            if maximized {
                let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(window, SW_RESTORE);
            }
            if SetWindowPos(window, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE).is_ok() {
                moved += 1;
            }
            if maximized {
                let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(window, SW_MAXIMIZE);
            }
        }
    }
    moved
}

/// [`sweep`] now, and a few times more over the next three seconds, in which Windows puts back
/// the windows it remembers for a monitor that has just joined the desktop.
fn sweep_for_a_while(area: Area) {
    let now = sweep(area);
    let _ = std::thread::Builder::new().name("sweep".into()).spawn(move || {
        // Every tenth of a second: a window Windows puts there is shown to the device until
        // the next sweep (seen: three of the owner's windows, every time the display joined).
        let mut moved = now;
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(100));
            moved += sweep(area);
        }
        if moved > 0 {
            tracing::info!(moved, "windows Windows put on the device's display went back to the main screen");
        }
    });
}

/// One of the PC's own screens as it was on the desktop, to be put back as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placed {
    device: String,
    mode: Mode,
    left: i32,
    top: i32,
    primary: bool,
    /// Its monitor's device path, lower case (empty in a file an older host wrote).
    monitor: String,
}

/// Held while the desktop is being arranged on a PC with screens of its own: by a device's
/// coming and going, and by the watch that keeps the driver's monitors off between devices.
static ARRANGING: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn arranging() -> std::sync::MutexGuard<'static, ()> {
    ARRANGING.lock().unwrap_or_else(|e| e.into_inner())
}

/// The names of the outputs that show the driver's monitors (whether part of the desktop or not).
fn driver_names() -> Vec<String> {
    driver_outputs().into_iter().map(|(o, _)| o.device).collect()
}

/// The PC's own screens on the desktop now (every screen that isn't one of the driver's
/// monitors), with mode, place and monitor.
fn own_screens() -> Vec<Placed> {
    crate::desk::follow();
    let shown = ccd::shown().unwrap_or_else(|e| {
        tracing::warn!("couldn't read the desktop's layout: {e:#}");
        Vec::new()
    });
    shown
        .into_iter()
        .filter(|s| !s.driver())
        .map(|s| {
            // (The rate as Windows' own settings call says it, as the layout file always had it.)
            let refresh = current(&s.device).map(|c| c.0.refresh).unwrap_or(s.refresh);
            Placed { device: s.device, mode: Mode { width: s.width, height: s.height, refresh }, left: s.left, top: s.top, primary: (s.left, s.top) == (0, 0), monitor: s.monitor }
        })
        .collect()
}

/// The layout file: one screen a line, `device width height refresh left top primary monitor`,
/// tab apart (an older host's has no monitor). Written before a device's monitor first joins
/// the desktop, removed once the screens are back as it says; still there at a start means the
/// last host died with a device on, or a screen wasn't there to be put back.
fn save_layout(path: &std::path::Path, screens: &[Placed]) -> Result<()> {
    let text: String = screens.iter().map(|s| format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n", s.device, s.mode.width, s.mode.height, s.mode.refresh, s.left, s.top, s.primary as u8, s.monitor)).collect();
    std::fs::write(path, text).with_context(|| format!("writing down the screens' layout in {}", path.display()))
}

fn load_layout(path: &std::path::Path) -> Result<Vec<Placed>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut all = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split('\t').collect();
        anyhow::ensure!(f.len() == 7 || f.len() == 8, "a line of {} isn't a screen: {line:?}", path.display());
        let n = |i: usize| f[i].parse::<i64>().with_context(|| format!("in {}: {line:?}", path.display()));
        let monitor = f.get(7).map_or(String::new(), |m| m.trim().to_lowercase());
        all.push(Placed { device: f[0].to_string(), mode: Mode { width: n(1)? as u32, height: n(2)? as u32, refresh: n(3)? as u32 }, left: n(4)? as i32, top: n(5)? as i32, primary: n(6)? != 0, monitor });
    }
    anyhow::ensure!(!all.is_empty(), "{} lists no screen", path.display());
    Ok(all)
}

/// The written-down screens as an arrangement: each on its own monitor (as written down, or, in
/// an older file, the one its output shows now if that is one of the PC's), with the screens
/// whose monitors aren't connected now left out.
fn layout_wants(screens: &[Placed]) -> Vec<ccd::Want> {
    let now = ccd::shown().unwrap_or_default();
    screens
        .iter()
        .filter_map(|s| {
            let monitor = if s.monitor.is_empty() { now.iter().find(|n| n.device.eq_ignore_ascii_case(&s.device) && !n.driver())?.monitor.clone() } else { s.monitor.clone() };
            ccd::present(&monitor).then(|| ccd::Want { device: s.device.clone(), monitor, width: s.mode.width, height: s.mode.height, refresh: s.mode.refresh, left: s.left, top: s.top })
        })
        .collect()
}

/// The written-down main screen's monitor, if the file says.
fn layout_main(screens: &[Placed]) -> Option<String> {
    screens.iter().find(|s| s.primary && !s.monitor.is_empty()).map(|s| s.monitor.clone())
}

/// Move an arrangement so that the screen matching `main` (else its first) is at 0,0, everything
/// with it.
fn to_origin(mut wants: Vec<ccd::Want>, main: Option<&str>) -> Vec<ccd::Want> {
    if wants.iter().any(|w| (w.left, w.top) == (0, 0)) {
        return wants;
    }
    let first = main.and_then(|m| wants.iter().find(|w| w.monitor == m)).or(wants.first()).map(|w| (w.left, w.top));
    if let Some((x, y)) = first {
        for w in &mut wants {
            (w.left, w.top) = (w.left - x, w.top - y);
        }
    }
    wants
}

/// The PC's own screens back as the layout file has them and every monitor of the driver's off
/// the desktop, in one arrangement. The file goes only once all its screens are back as it
/// says: while one of them isn't connected (a monitor asleep, as DisplayPort monitors that sleep
/// drop off the computer), those that are go back as it has them, or failing that the driver's
/// monitors are just taken off, and the file stays for the next try (when a device leaves, at
/// the next start, or by the watch between devices once that monitor is back). The caller holds
/// [`ARRANGING`].
fn restore_layout(path: &std::path::Path) -> Result<()> {
    let screens = load_layout(path)?;
    let main = layout_main(&screens);
    let wants = layout_wants(&screens);
    if wants.len() == screens.len() {
        match ccd::arrange(&wants) {
            Ok(()) => {
                let _ = std::fs::remove_file(path);
                tracing::info!(screens = wants.len(), "the PC's own screens are back as they were");
                return Ok(());
            }
            Err(e) => tracing::warn!("couldn't put the PC's screens back as they were: {e:#}"),
        }
    } else {
        tracing::warn!(here = wants.len(), of = screens.len(), "not all of the PC's screens are connected now (asleep, or unplugged): those that are go back, and the rest when they come");
        if !wants.is_empty() {
            match ccd::arrange(&to_origin(wants, main.as_deref())) {
                Ok(()) => return Ok(()),
                Err(e) => tracing::warn!("couldn't put the connected screens back as they were: {e:#}"),
            }
        }
    }
    // At least: none of the driver's monitors on the desktop, and one of the PC's the main display.
    ccd::drop_driver(main.as_deref())?;
    bail!("the PC's screens are as Windows has them, without the driver's monitors; {} stays for the next try", path.display())
}

/// On a PC with screens of its own, the arrangements for a device's monitor (the driver's
/// monitor `index`, shown by `device`, in `mode`) among the PC's screens as written down:
/// beside them to the right, level with the main screen's top; as the main display, the PC's
/// screens kept where they are beside it (everything measured from it, so they all move left);
/// or as the only one, the PC's screens off. A screen whose monitor isn't connected is left out.
fn pc_arrangement(screens: &[Placed], device: &str, index: u32, mode: Mode) -> impl Fn(Place) -> Vec<ccd::Want> {
    let mine = to_origin(layout_wants(screens), layout_main(screens).as_deref());
    let right = mine.iter().map(|w| w.left + w.width as i32).max().unwrap_or(0);
    let top = mine.iter().find(|w| (w.left, w.top) == (0, 0)).map_or(0, |w| w.top);
    let device = device.to_string();
    let theirs = move |left: i32, top: i32| ccd::Want { device: device.clone(), monitor: ccd::driver_monitor(index), width: mode.width, height: mode.height, refresh: mode.refresh, left, top };
    move |place| match place {
        Place::Beside => mine.iter().cloned().chain(std::iter::once(theirs(right, top))).collect(),
        Place::Main => std::iter::once(theirs(0, 0)).chain(mine.iter().map(|w| ccd::Want { left: w.left - right, top: w.top - top, ..w.clone() })).collect(),
        // (By design the PC's monitors get no picture: one that sleeps then may drop off the
        // computer and not come back by itself, as DisplayPort monitors do.)
        Place::Only => vec![theirs(0, 0)],
    }
}

/// For tests on a PC with screens of its own, without the driver (which only SYSTEM and
/// administrators may open): what a device's coming and going does to the desktop, with one of
/// the driver's monitors that exists already (`output` showing monitor `index`, in a `mode` its
/// list has). `beside` and `main` write the screens down in `state` first, as for a device;
/// `back` puts them back as [`Screen::release`] does; `drop` takes the driver's monitors off;
/// `probe` asks Windows (testing only) whether it would take each screen's settings again now.
/// Says the desktop afterwards.
pub fn layout_test(state: &std::path::Path, step: &str, output: &str, index: u32, mode: Mode) -> Result<()> {
    let layout = state.join("screens.txt");
    // (`watch INDEX`: the watch between devices for INDEX seconds, which takes the lock itself.)
    let _alone = (step != "watch").then(arranging);
    crate::desk::follow();
    match step {
        "watch" => {
            watch_between_devices(layout.clone(), Arc::new(std::sync::atomic::AtomicBool::new(false)));
            std::thread::sleep(Duration::from_secs(index as u64));
        }
        "beside" | "main" => {
            if !layout.exists() {
                let screens = own_screens();
                anyhow::ensure!(!screens.is_empty(), "this PC's own screens aren't on the desktop");
                save_layout(&layout, &screens)?;
            }
            let screens = load_layout(&layout)?;
            let arrangement = pc_arrangement(&screens, output, index, mode);
            let place = if step == "main" { Place::Main } else { Place::Beside };
            let showing = ccd::shown()?.iter().any(|s| s.device.eq_ignore_ascii_case(output) && (s.width, s.height) == (mode.width, mode.height));
            if place == Place::Main && !showing {
                arrange_settled(&arrangement(Place::Beside))?;
            }
            arrange_settled(&arrangement(place))?;
        }
        "back" => restore_layout(&layout)?,
        "drop" => {
            ccd::drop_driver(None)?;
        }
        "probe" => {
            use windows::Win32::Graphics::Gdi::CDS_TEST;
            for s in ccd::shown()? {
                let mut dm = devmode();
                let name = HSTRING::from(s.device.as_str());
                // SAFETY: `dm` is sized as the calls want; testing changes nothing.
                let said = unsafe {
                    let _ = EnumDisplaySettingsW(&name, ENUM_CURRENT_SETTINGS, &mut dm);
                    dm.dmFields = DM_POSITION | DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
                    ChangeDisplaySettingsExW(&name, Some(&dm), None, CDS_TEST, None)
                };
                println!("{} ({}): ChangeDisplaySettingsEx testing its own settings: {}", s.device, if s.driver() { "the driver's" } else { "the PC's" }, said.0);
            }
        }
        other => bail!("--layout-test: no step {other:?} (beside, main, back, drop, probe, watch)"),
    }
    for s in ccd::shown()? {
        println!("{} {}x{}@{} at {},{}{} {}", s.device, s.width, s.height, s.refresh, s.left, s.top, if (s.left, s.top) == (0, 0) { " main" } else { "" }, s.monitor);
    }
    Ok(())
}

/// [`ccd::arrange`], and again if the driver's monitors that the driver re-created join the
/// desktop late (seen: two of them turned up beside a device's display, re-created for its
/// size, after it had been arranged): watched for two seconds.
fn arrange_settled(wants: &[ccd::Want]) -> Result<()> {
    ccd::arrange(wants)?;
    let since = Instant::now();
    let mut again = 0;
    while since.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(250));
        if let Err(e) = ccd::check(wants) {
            anyhow::ensure!(again < 3, "the desktop won't stay as arranged: {e:#}");
            tracing::info!("the desktop changed after it was arranged ({e:#}): arranging it again");
            ccd::arrange(wants)?;
            again += 1;
        }
    }
    Ok(())
}

/// The driver and its monitor.
pub struct Screen {
    driver: Arc<Driver>,
    /// On a PC with screens of its own (not a guest whose only display is the driver's): the
    /// driver's monitors are on the desktop only while a device is, and the PC's own screens
    /// are put back as they were when it goes.
    pc: bool,
    /// Where the PC's own screens are written down while a device's monitor is on the desktop.
    layout: std::path::PathBuf,
    /// The monitor's output; found again after the driver has re-created the monitor.
    device: String,
    /// The mode was switched for a client and is to be put back.
    switched: bool,
    /// The device has a monitor of its own (the driver's monitor 1): its output now.
    own: Option<String>,
    /// What the first monitor was before a device's monitor took its place as the main
    /// display or the only one: put back when the device leaves.
    first_was: Option<Mode>,
    /// The device's second display (the driver's monitor 2), while it has one: its output.
    second: Option<String>,
    /// A device has a monitor on the desktop (on a PC with screens of its own): the watch
    /// between devices leaves the desktop alone meanwhile.
    busy: Arc<std::sync::atomic::AtomicBool>,
    /// The device is shown one of the PC's own screens as it is ([`Screen::mirror_output`]):
    /// nothing was arranged for it, so nothing is put back.
    mirrored: bool,
    /// Only mirrors: the driver isn't used at all, and nothing here touches the desktop (a test
    /// host that isn't elevated).
    mirror_only: bool,
}

/// On a PC with screens of its own, between devices: every few seconds, take the driver's
/// monitors off the desktop if Windows has put them there (it rebuilds the layout from every
/// monitor connected, the driver's included, whenever one of the PC's monitors sleeps or wakes,
/// and may make one of the driver's the main display), and put the PC's screens back as the
/// layout file has them once all of them are connected again (a monitor that was asleep when
/// the last device left). The main screen is the one last seen there, which is kept in mind.
fn watch_between_devices(layout: std::path::PathBuf, busy: Arc<std::sync::atomic::AtomicBool>) {
    let _ = std::thread::Builder::new().name("screens".into()).spawn(move || {
        let mut main: Option<String> = None;
        // After a try that failed, a minute's rest (not a warning every three seconds).
        let mut rest_until: Option<Instant> = None;
        loop {
            std::thread::sleep(Duration::from_secs(3));
            if busy.load(std::sync::atomic::Ordering::Relaxed) || rest_until.is_some_and(|t| Instant::now() < t) {
                continue;
            }
            let Ok(_alone) = ARRANGING.try_lock() else { continue };
            crate::desk::follow();
            let Ok(shown) = ccd::shown() else { continue };
            if !shown.iter().any(ccd::Shown::driver) {
                if let Some(first) = shown.iter().find(|s| (s.left, s.top) == (0, 0)) {
                    main = Some(first.monitor.clone());
                }
            }
            if layout.exists() {
                let Ok(screens) = load_layout(&layout) else { continue };
                if layout_wants(&screens).len() == screens.len() {
                    tracing::info!("all of the PC's screens are connected again: putting them back as they were");
                    if let Err(e) = restore_layout(&layout) {
                        tracing::warn!("{e:#}");
                        rest_until = Some(Instant::now() + Duration::from_secs(60));
                    }
                    continue;
                }
            }
            if shown.iter().any(ccd::Shown::driver) {
                tracing::info!(?shown, "Windows put the driver's monitors on the desktop with no device here: taking them off");
                if let Err(e) = ccd::drop_driver(main.as_deref()) {
                    tracing::warn!("couldn't take the driver's monitors off the desktop: {e:#}");
                    rest_until = Some(Instant::now() + Duration::from_secs(60));
                }
            }
        }
    });
}

impl Screen {
    /// Open the driver. `state` is the host's state folder, where the PC's screens are written
    /// down while a device's monitor is on the desktop.
    pub fn open(state: &std::path::Path) -> Result<Self> {
        let driver = Driver::open()?;
        driver.ping()?;
        let layout = state.join("screens.txt");
        let driver_list = driver_names();
        // A PC with screens of its own; or one whose screens a dead host left off (its layout
        // file says so).
        let pc = outputs().iter().any(|o| !driver_list.contains(&o.device)) || layout.exists();
        let busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if pc {
            {
                let _alone = arranging();
                if layout.exists() {
                    tracing::warn!("the last host left a device's monitor on the desktop, or a screen wasn't there to be put back: putting the PC's screens back");
                    // (On failure the file stays, to be tried again.)
                    if let Err(e) = restore_layout(&layout) {
                        tracing::warn!("couldn't put the PC's screens back yet: {e:#}");
                    }
                }
                // Nobody is here yet: none of the driver's monitors is part of the desktop.
                crate::desk::follow();
                if let Err(e) = ccd::drop_driver(None) {
                    tracing::warn!("couldn't take the driver's monitors off the desktop: {e:#}");
                }
            }
            watch_between_devices(layout.clone(), busy.clone());
            let first = driver_output(0).context("the display driver lists no monitor")?;
            tracing::info!(driver = %driver.path, first = %first.device, "display driver; this PC has screens of its own, so the driver's monitors join the desktop only while a device is here");
            return Ok(Self { driver: Arc::new(driver), pc, layout, device: first.device, switched: false, own: None, first_was: None, second: None, busy, mirrored: false, mirror_only: false });
        }
        let (output, sure) = find_output()?;
        tracing::info!(driver = %driver.path, output = %output.device, adapter = %output.description, recognised = sure, "display driver");
        Ok(Self { driver: Arc::new(driver), pc, layout, device: output.device, switched: false, own: None, first_was: None, second: None, busy, mirrored: false, mirror_only: false })
    }

    /// Has this PC screens of its own, so that the driver's monitors are there only for devices?
    pub fn pc(&self) -> bool {
        self.pc
    }

    /// A host that does nothing but mirror the PC's own screens, with no driver and no change to
    /// the desktop (for testing it as a user who may not open the driver).
    pub fn mirror_only() -> Self {
        Self {
            driver: Arc::new(Driver::absent()),
            pc: true,
            layout: std::path::PathBuf::new(),
            device: String::new(),
            switched: false,
            own: None,
            first_was: None,
            second: None,
            busy: Arc::default(),
            mirrored: false,
            mirror_only: true,
        }
    }

    pub fn only_mirrors(&self) -> bool {
        self.mirror_only
    }

    /// The PC's screen a device that asks to mirror is shown: the main display, or failing that
    /// the first of the others that is part of the desktop; never one of the driver's monitors.
    /// Its Windows name, mode and place on the desktop.
    pub fn mirror_output(&self) -> Result<(String, Mode, Area)> {
        let driver = driver_names();
        let all: Vec<Output> = outputs().into_iter().filter(|o| !driver.contains(&o.device)).collect();
        let Some(output) = all.iter().find(|o| o.primary).or(all.first()) else { bail!("the PC has no screen of its own on the desktop to show") };
        let (mode, area) = current(&output.device)?;
        Ok((output.device.clone(), mode, area))
    }

    /// Say whether the device is shown one of the PC's screens as it is, which nothing is
    /// arranged for and so nothing is put back.
    pub fn set_mirrored(&mut self, on: bool) {
        self.mirrored = on;
    }

    pub fn mirrored(&self) -> bool {
        self.mirrored
    }

    /// Give back what an earlier picture was given (a monitor of its own, a changed mode), if
    /// there is anything: for a picture that is to be a mirror of the PC's screen instead.
    pub fn release_given(&mut self) {
        if !self.mirror_only && (self.own.is_some() || self.second.is_some() || self.switched) {
            self.release();
        }
    }


    /// The driver, for the picture's thread to ask for the frame channel with.
    pub fn driver(&self) -> Arc<Driver> {
        self.driver.clone()
    }

    /// The output the client is shown: its own monitor's if it has one.
    pub fn device(&self) -> &str {
        self.own.as_deref().unwrap_or(&self.device)
    }

    /// The driver's number for the monitor the client is shown.
    pub fn monitor(&self) -> u32 {
        self.own.as_deref().and_then(driver_index).unwrap_or(self.own.is_some() as u32)
    }

    /// The mode and place on the desktop, now, of the monitor the client is shown.
    pub fn now(&self) -> Result<(Mode, Area)> {
        current(self.device())
    }

    /// Has this driver a second monitor to give a device? Asked once, when the host starts: the
    /// driver is told to make it, and it is taken out of the desktop (Windows adds a monitor
    /// that appears) until a device comes.
    pub fn has_own(&mut self) -> bool {
        if driver_output(1).is_none() {
            let _ = self.driver.command("CREATEMONITOR");
            let asked = Instant::now();
            while driver_output(1).is_none() && asked.elapsed() < Duration::from_secs(4) {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        let Some(own) = driver_output(1) else { return false };
        // Windows takes a moment to make a new monitor part of the desktop.
        let seen = Instant::now();
        while !attached(&own.device) && seen.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if self.pc {
            // The PC's screens as they are, the driver's monitors off, in one arrangement (a
            // third monitor of the driver's may join a moment later: the watch between devices
            // takes it off then).
            if let Some(first) = driver_output(0) {
                self.device = first.device;
            }
            let _alone = arranging();
            if let Err(e) = ccd::drop_driver(None) {
                tracing::warn!("couldn't take the driver's monitors off the desktop: {e:#}");
            }
            return true;
        }
        if attached(&own.device) {
            if let Err(e) = detach(&own.device) {
                tracing::warn!("the driver's second monitor is part of the desktop and couldn't be taken out: {e:#}");
            }
        }
        if let Some(first) = driver_output(0) {
            self.device = first.device;
        }
        // The driver made every monitor its settings allow: a third, which comes a moment
        // after the second if it comes, is kept out of the desktop as well.
        let seen = Instant::now();
        while seen.elapsed() < Duration::from_millis(2500) {
            match driver_output(2) {
                Some(third) if attached(&third.device) => {
                    let _ = detach(&third.device);
                    break;
                }
                // There and already out (left so by an earlier run).
                Some(_) if seen.elapsed() > Duration::from_millis(1500) => break,
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        true
    }

    /// Has the driver a third monitor, for a device's second display?
    pub fn has_second(&self) -> bool {
        driver_outputs().iter().any(|(_, m)| m.iter().any(|(i, _)| *i == 2))
    }

    /// Give the device a second display of this size on that side of everything else on the
    /// desktop, level with its own monitor. Says the mode it got and where it is.
    pub fn give_second(&mut self, want: Mode, side: remoteman_core::Side) -> Result<(Mode, Area, u32)> {
        use remoteman_core::Side;
        anyhow::ensure!(self.own.is_some(), "a second display goes beside a monitor of the device's own, which it hasn't got");
        // Of the driver's monitors 1 and 2, the one the device's own monitor isn't.
        let own_index = self.monitor();
        let other = if own_index == 2 { 1 } else { 2 };
        let mut second = driver_output(other).context("the display driver has no monitor left for a second display")?;
        if !modes(&second.device).contains(&want) {
            let offered = modes(self.own.as_deref().unwrap_or(&self.device));
            if let Err(e) = self.add_mode(want, &offered) {
                tracing::warn!("couldn't add {}x{} at {} Hz to the driver's modes: {e:#}", want.width, want.height, want.refresh);
            }
            // Every monitor of the driver's came back under a new name.
            let asked = Instant::now();
            while (driver_output(2).is_none() || driver_output(1).is_none()) && asked.elapsed() < Duration::from_secs(6) {
                std::thread::sleep(Duration::from_millis(200));
            }
            second = driver_output(other).context("the second display's monitor didn't come back after its mode was added")?;
            if let Some(first) = driver_output(0) {
                self.device = first.device;
            }
            if let Some(own) = driver_output(own_index) {
                self.own = Some(own.device);
            }
        }
        let offered = modes(&second.device);
        let rates = offered.iter().filter(|m| (m.width, m.height) == (want.width, want.height)).map(|m| m.refresh);
        let mode = match nearest_refresh(rates, want.refresh) {
            Some(refresh) => Mode { refresh, ..want },
            None => *offered.first().context("the second display's monitor offers no mode")?,
        };
        if self.pc {
            return self.give_second_pc(&second.device, other, mode, side);
        }
        // Everything on the desktop now, and the device's own monitor among it.
        let (_, own) = current(self.own.as_deref().unwrap_or(&self.device))?;
        let all: Vec<Area> = outputs().iter().filter(|o| o.device != second.device).filter_map(|o| current(&o.device).ok().map(|c| c.1)).collect();
        let edge = |pick: fn(&Area) -> i32, most: bool| all.iter().map(pick).reduce(|a, b| if most { a.max(b) } else { a.min(b) }).unwrap_or(0);
        let (x, y) = match side {
            Side::Right => (edge(|a| a.left + a.width as i32, true), own.top),
            Side::Left => (edge(|a| a.left, false) - mode.width as i32, own.top),
            Side::Above => (own.left, edge(|a| a.top, false) - mode.height as i32),
            Side::Below => (own.left, edge(|a| a.top + a.height as i32, true)),
        };
        stage(&second.device, Some((mode, x, y, false)))?;
        apply()?;
        let (now, area) = current(&second.device)?;
        tracing::info!(?want, ?now, ?side, output = %second.device, left = area.left, top = area.top, "the device has a second display");
        if self.pc {
            sweep_for_a_while(area);
        }
        let index = driver_index(&second.device).unwrap_or(2);
        self.second = Some(second.device);
        Ok((now, area, index))
    }

    /// [`Screen::give_second`] on a PC with screens of its own: one arrangement of what is on the
    /// desktop now (the PC's screens and the device's own monitor; any other of the driver's
    /// monitors off) and the second display on that side of it, level with the device's own.
    fn give_second_pc(&mut self, second: &str, index: u32, mode: Mode, side: remoteman_core::Side) -> Result<(Mode, Area, u32)> {
        use remoteman_core::Side;
        let _alone = arranging();
        crate::desk::follow();
        let own_device = self.own.clone().context("a second display goes beside a monitor of the device's own, which it hasn't got")?;
        let shown = ccd::shown()?;
        let mut wants: Vec<ccd::Want> = shown
            .iter()
            .filter(|s| !s.driver() || s.device.eq_ignore_ascii_case(&own_device))
            .map(|s| ccd::Want { device: s.device.clone(), monitor: s.monitor.clone(), width: s.width, height: s.height, refresh: 0, left: s.left, top: s.top })
            .collect();
        let own = wants.iter().find(|w| w.device.eq_ignore_ascii_case(&own_device)).cloned().context("the device's own monitor isn't on the desktop")?;
        let edge = |pick: fn(&ccd::Want) -> i32, most: bool| wants.iter().map(pick).reduce(|a, b| if most { a.max(b) } else { a.min(b) }).unwrap_or(0);
        let (x, y) = match side {
            Side::Right => (edge(|w| w.left + w.width as i32, true), own.top),
            Side::Left => (edge(|w| w.left, false) - mode.width as i32, own.top),
            Side::Above => (own.left, edge(|w| w.top, false) - mode.height as i32),
            Side::Below => (own.left, edge(|w| w.top + w.height as i32, true)),
        };
        wants.push(ccd::Want { device: second.to_string(), monitor: ccd::driver_monitor(index), width: mode.width, height: mode.height, refresh: mode.refresh, left: x, top: y });
        ccd::arrange(&wants)?;
        let (now, area) = current(second)?;
        tracing::info!(?now, ?side, output = %second, left = area.left, top = area.top, "the device has a second display");
        sweep_for_a_while(area);
        self.second = Some(second.to_string());
        Ok((now, area, index))
    }

    /// The device's second display leaves the desktop.
    pub fn take_second(&mut self) {
        if self.pc {
            let Some(second) = self.second.take() else { return };
            let _alone = arranging();
            crate::desk::follow();
            let kept = ccd::shown().map(|shown| {
                shown.into_iter().filter(|s| !s.device.eq_ignore_ascii_case(&second)).map(|s| ccd::Want { device: s.device, monitor: s.monitor, width: s.width, height: s.height, refresh: 0, left: s.left, top: s.top }).collect::<Vec<_>>()
            });
            match kept.and_then(|wants| ccd::arrange(&wants)) {
                Ok(()) => tracing::info!("the device's second display is off the desktop again"),
                Err(e) => tracing::warn!("couldn't take the device's second display off the desktop: {e:#}"),
            }
            return;
        }
        let Some(second) = self.second.take() else { return };
        match detach(&second) {
            Ok(()) => tracing::info!("the device's second display is off the desktop again"),
            Err(e) => tracing::warn!("couldn't take the device's second display off the desktop: {e:#}"),
        }
    }

    /// Give the device a monitor of its own at its size and rate, placed as it asked. `None`:
    /// this driver has no second monitor to give (see the top of this file), and nothing
    /// was changed. The mode comes back as it turned out.
    pub fn give(&mut self, want: Mode, place: Place) -> Result<Option<Mode>> {
        if self.pc {
            return self.give_pc(want, place).map(Some);
        }
        // The driver makes every monitor its settings allow that isn't there yet.
        if driver_output(1).is_none() {
            self.driver.command("CREATEMONITOR")?;
            let asked = Instant::now();
            while driver_output(1).is_none() && asked.elapsed() < Duration::from_secs(4) {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        let Some(mut own) = driver_output(1) else { return Ok(None) };
        if let Some(first) = driver_output(0) {
            self.device = first.device;
        }
        if !modes(&own.device).contains(&want) {
            // The first monitor is off for this device (it has the only display and now wants
            // another size): the list of modes is read from the first monitor and re-creates
            // it, so it comes back on for the moment that takes.
            if !attached(&self.device) {
                self.take_back();
            }
            // One list for all the driver's monitors: the first one's, with this mode added.
            let offered = modes(&self.device);
            let added = self.add_mode(want, &offered);
            if let Err(e) = added {
                tracing::warn!("couldn't add {}x{} at {} Hz to the driver's modes: {e:#}", want.width, want.height, want.refresh);
            }
            // Every monitor came back under a new name.
            let asked = Instant::now();
            while (driver_output(1).is_none() || driver_output(0).is_none()) && asked.elapsed() < Duration::from_secs(6) {
                std::thread::sleep(Duration::from_millis(200));
            }
            own = driver_output(1).context("the device's monitor didn't come back after its mode was added")?;
            if let Some(first) = driver_output(0) {
                self.device = first.device;
            }
        }
        let offered = modes(&own.device);
        let rates = offered.iter().filter(|m| (m.width, m.height) == (want.width, want.height)).map(|m| m.refresh);
        let mode = match nearest_refresh(rates, want.refresh) {
            Some(refresh) => Mode { refresh, ..want },
            // Not the device's size at all: the first mode the monitor has.
            None => *offered.first().context("the device's monitor offers no mode")?,
        };
        let first_on = attached(&self.device);
        let first = if first_on { Some(current(&self.device)?.0) } else { self.first_was };
        match (place, first) {
            (Place::Beside, Some(first)) if first_on => stage(&own.device, Some((mode, first.width as i32, 0, false)))?,
            // (No first monitor on the desktop to be beside: the device's is the only one.)
            (Place::Beside, _) => stage(&own.device, Some((mode, 0, 0, true)))?,
            (Place::Main, Some(first)) => {
                stage(&own.device, Some((mode, 0, 0, true)))?;
                stage(&self.device, Some((first, mode.width as i32, 0, false)))?;
                self.first_was = self.first_was.or(Some(first));
            }
            (Place::Only, Some(first)) => {
                stage(&own.device, Some((mode, 0, 0, true)))?;
                stage(&self.device, None)?;
                self.first_was = self.first_was.or(Some(first));
            }
            (_, None) => stage(&own.device, Some((mode, 0, 0, true)))?,
        }
        apply()?;
        self.own = Some(own.device.clone());
        let (now, area) = current(&own.device)?;
        tracing::info!(?want, ?now, ?place, output = %own.device, left = area.left, top = area.top, "the device has a monitor of its own");
        Ok(Some(now))
    }

    /// [`Screen::give`] on a PC with screens of its own: the device's monitor (the driver's
    /// monitor 1, or its only one) joins the desktop placed against the PC's own screens, as
    /// they were before any device came (written down first, so that they are put back
    /// exactly, even by the next host if this one dies): beside them to the right, level with
    /// the main screen's top; or as the main display, the PC's screens kept where they are
    /// beside it; or as the only one, the PC's screens off while the device is here.
    fn give_pc(&mut self, want: Mode, place: Place) -> Result<Mode> {
        self.busy.store(true, std::sync::atomic::Ordering::Relaxed);
        let _alone = arranging();
        crate::desk::follow();
        let given = self.give_pc_arranging(want, place);
        if let Err(e) = &given {
            // Nothing half done is left on the desktop: the PC's screens back as written down
            // (or at least the driver's monitors off), and the watch between devices on again.
            tracing::warn!("couldn't give the device a monitor of its own: {e:#}; putting the PC's screens back");
            let back = if self.layout.exists() { restore_layout(&self.layout) } else { ccd::drop_driver(None).map(|_| ()) };
            if let Err(e) = back {
                tracing::warn!("{e:#}");
            }
            self.own = None;
            self.busy.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        given
    }

    /// [`Screen::give_pc`]'s work, with [`ARRANGING`] held.
    fn give_pc_arranging(&mut self, want: Mode, place: Place) -> Result<Mode> {
        if !self.layout.exists() {
            let screens = own_screens();
            anyhow::ensure!(!screens.is_empty(), "this PC's own screens aren't on the desktop");
            save_layout(&self.layout, &screens)?;
        }
        let screens = load_layout(&self.layout)?;
        if driver_output(1).is_none() {
            let _ = self.driver.command("CREATEMONITOR");
            let asked = Instant::now();
            while driver_output(1).is_none() && asked.elapsed() < Duration::from_secs(4) {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        let index = if driver_output(1).is_some() { 1 } else { 0 };
        let mut own = driver_output(index).context("the display driver lists no monitor")?;
        if !modes(&own.device).contains(&want) {
            let offered = modes(&own.device);
            if let Err(e) = self.add_mode(want, &offered) {
                tracing::warn!("couldn't add {}x{} at {} Hz to the driver's modes: {e:#}", want.width, want.height, want.refresh);
            }
            // Every monitor came back under a new name.
            let asked = Instant::now();
            while driver_output(index).is_none() && asked.elapsed() < Duration::from_secs(6) {
                std::thread::sleep(Duration::from_millis(200));
            }
            own = driver_output(index).context("the device's monitor didn't come back after its mode was added")?;
            if let Some(first) = driver_output(0) {
                self.device = first.device;
            }
        }
        let offered = modes(&own.device);
        let rates = offered.iter().filter(|m| (m.width, m.height) == (want.width, want.height)).map(|m| m.refresh);
        let mode = match nearest_refresh(rates, want.refresh) {
            Some(refresh) => Mode { refresh, ..want },
            None => *offered.first().context("the device's monitor offers no mode")?,
        };
        let arrangement = pc_arrangement(&screens, &own.device, index, mode);
        // As the main display, it joins as an extra one first: its rate is set while one of the
        // PC's screens is the main display (seen: with one of the driver's monitors the main
        // display, Windows refused to set anything with ChangeDisplaySettingsEx).
        let showing = ccd::shown().unwrap_or_default().iter().any(|s| s.device.eq_ignore_ascii_case(&own.device) && (s.width, s.height, s.refresh) == (mode.width, mode.height, mode.refresh));
        if place == Place::Main && !showing {
            arrange_settled(&arrangement(Place::Beside))?;
        }
        arrange_settled(&arrangement(place))?;
        self.own = Some(own.device.clone());
        let (now, area) = current(&own.device)?;
        tracing::info!(?want, ?now, ?place, output = %own.device, left = area.left, top = area.top, "the device has a monitor of its own");
        // As an extra display it starts empty: none of the owner's windows on it. (As the main or
        // only display it is where the desktop is, windows and all, as the device asked.)
        if place == Place::Beside {
            sweep_for_a_while(area);
        }
        Ok(now)
    }

    /// The device's own monitor leaves the desktop, and the first monitor is as it was.
    fn take_back(&mut self) {
        if self.pc {
            // Whatever is on the device's displays goes to the main screen before they leave
            // (Windows may leave it off-screen otherwise).
            for device in [self.second.take(), self.own.take()].into_iter().flatten() {
                if let Ok((_, area)) = current(&device) {
                    let moved = sweep(area);
                    if moved > 0 {
                        tracing::info!(moved, "windows on the device's display went to the main screen before it left");
                    }
                }
            }
            {
                let _alone = arranging();
                crate::desk::follow();
                let done = if self.layout.exists() { restore_layout(&self.layout) } else { ccd::drop_driver(None).map(|_| ()) };
                if let Err(e) = done {
                    tracing::warn!("couldn't put the PC's screens back as they were: {e:#}");
                }
            }
            self.busy.store(false, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        self.take_second();
        let Some(own) = self.own.take() else { return };
        let first = driver_output(0).map_or(self.device.clone(), |o| o.device);
        self.device = first.clone();
        let staged = match self.first_was.take() {
            Some(was) => stage(&first, Some((was, 0, 0, true))),
            None => Ok(()),
        }
        .and_then(|()| stage(&own, None))
        .and_then(|()| apply());
        match staged {
            Ok(()) => tracing::info!("the device's monitor is off the desktop again"),
            Err(e) => tracing::warn!("couldn't take the device's monitor off the desktop: {e:#}"),
        }
    }

    /// Bring the monitor as near to what the client wants as `fit` allows, and say what mode it
    /// is in afterwards. Not getting the client's mode is not a failure: the picture is then
    /// whatever the monitor shows, and the client is told its real size.
    pub fn fit(&mut self, want: Mode, fit: Fit) -> Result<Mode> {
        // On a PC with screens of its own the driver's monitors show nothing until they join the
        // desktop: a device always gets one there.
        if self.pc {
            return self.give_pc(want, Place::Beside);
        }
        self.take_back();
        let (was, _) = self.now()?;
        if fit == Fit::Keep || was == want {
            return Ok(was);
        }
        let mut offered = modes(&self.device);
        let listed = |offered: &[Mode]| offered.contains(&want);
        if !listed(&offered) && fit == Fit::Any {
            match self.add_mode(want, &offered) {
                Ok(()) => offered = modes(&self.device),
                Err(e) => tracing::warn!("couldn't add {}x{} at {} Hz to the monitor: {e:#}", want.width, want.height, want.refresh),
            }
        }
        // The client's size at the nearest rate, if its own rate isn't to be had.
        let rates = offered.iter().filter(|m| (m.width, m.height) == (want.width, want.height)).map(|m| m.refresh);
        let Some(refresh) = nearest_refresh(rates, want.refresh) else {
            tracing::info!(?want, ?was, "the monitor doesn't offer the client's size; streaming it as it is");
            return Ok(self.now()?.0);
        };
        let target = Mode { refresh, ..want };
        if target != self.now()?.0 {
            match switch(&self.device, target) {
                Ok(()) => self.switched = true,
                Err(e) => tracing::warn!("{e:#}; streaming the monitor as it is"),
            }
        }
        let (now, _) = self.now()?;
        tracing::info!(?want, ?was, ?now, "monitor mode");
        Ok(now)
    }

    /// Have the driver offer one more mode. It takes a whole new list, so it is given what the
    /// monitor offers now with the new mode at the end, and re-creates the monitor to tell
    /// Windows; wait until Windows lists the mode. The list's first mode is the monitor's
    /// preferred one, which Windows may fall back to when the monitor comes back: that is made
    /// the mode it is in now, so that nothing changes by itself. The driver keeps the list in
    /// memory only; restarting the driver brings back the one from its settings file.
    fn add_mode(&mut self, want: Mode, offered: &[Mode]) -> Result<()> {
        // (The first monitor's mode, or the device's own monitor's while the first is off; with
        // neither on the desktop, the one the list starts with now.)
        let now = current(&self.device).or_else(|e| self.own.as_deref().map_or(Err(e), current)).map(|c| c.0).ok().or_else(|| offered.first().copied()).unwrap_or(want);
        let mut list: Vec<(u32, u32, u32)> = std::iter::once(&now).chain(offered.iter().filter(|m| **m != now)).map(|m| (m.width, m.height, m.refresh)).collect();
        // The driver keeps the first and the newest 95; drop from the middle ourselves so that
        // what is sent is what it keeps.
        if list.len() > 95 {
            list.drain(1..list.len() - 94);
        }
        list.push((want.width, want.height, want.refresh));
        tracing::info!(modes = list.len(), "asking the driver to add {}x{} at {} Hz (it re-creates the monitor)", want.width, want.height, want.refresh);
        self.driver.set_modes(&list)?;
        let asked = Instant::now();
        while asked.elapsed() < Duration::from_secs(8) {
            std::thread::sleep(Duration::from_millis(200));
            // The monitors come back as new outputs, perhaps under other names. With several,
            // the first is found by its number; any of them listing the mode means all do.
            let listed = (0..3).filter_map(driver_output).any(|o| modes(&o.device).contains(&want));
            let Some(output) = driver_output(0).or_else(|| find_output().ok().map(|f| f.0)) else { continue };
            if listed || modes(&output.device).contains(&want) {
                tracing::info!(after_ms = asked.elapsed().as_millis() as u64, output = %output.device, "the monitor offers the mode");
                self.device = output.device;
                return Ok(());
            }
        }
        if let Ok((output, _)) = find_output() {
            self.device = output.device;
        }
        bail!("the driver was asked, but 8 s later Windows doesn't list the mode");
    }

    /// The client is gone: put the monitor back as it was.
    pub fn release(&mut self) {
        // A mirrored screen was left as it was; and a host that only mirrors has no driver.
        if std::mem::take(&mut self.mirrored) || self.mirror_only {
            return;
        }
        self.take_back();
        if std::mem::take(&mut self.switched) {
            match restore(&self.device) {
                Ok(()) => tracing::info!(now = ?self.now().ok().map(|n| n.0), "the monitor is back in its own mode"),
                Err(e) => tracing::warn!("{e:#}"),
            }
        }
    }

}

/// Make Windows show a new frame on the monitor at this spot of the desktop: a one-pixel window
/// appears there for a moment. (Moving the pointer wouldn't do: the driver takes the cursor
/// apart from the picture, so it makes no frame.) Only works from the desktop's own session.
pub fn nudge(at: (i32, i32)) {
    // On the desktop that is showing (its window is gone again before the thread may move).
    crate::desk::follow();
    // SAFETY: a window of a system class, made, shown and destroyed on this thread.
    unsafe {
        let style = WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST;
        match CreateWindowExW(style, w!("STATIC"), w!(""), WS_POPUP, at.0, at.1, 1, 1, None, None, None, None) {
            Ok(window) => {
                let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
                let _ = UpdateWindow(window);
                std::thread::sleep(Duration::from_millis(30));
                let _ = DestroyWindow(window);
            }
            Err(e) => tracing::debug!("couldn't make a window to wake the desktop with: {e}"),
        }
    }
}

/// The frame channel of the driver's monitor, if it is there yet (see [`Driver::open_channel`]).
pub fn channel(driver: &Driver, monitor: u32) -> Result<Option<Channel>> {
    // Asking about the channel is what makes the driver fill it.
    driver.caps().context("frame channel")?;
    driver.open_channel(monitor)
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.release();
    }
}
