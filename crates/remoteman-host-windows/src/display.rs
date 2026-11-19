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

/// The driver's output whose monitor is the driver's number `index`, whether or not it is
/// part of the desktop. Windows names outputs anew when the driver re-creates its monitors;
/// what stays is the monitor's own number in its device path ("UID256" for the first).
pub fn driver_output(index: u32) -> Option<Output> {
    let mark = format!("uid{}", 256 + index);
    for i in 0.. {
        let mut d = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        // SAFETY: `d` is sized as the call wants.
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut d, 0) }.as_bool() {
            break;
        }
        let mut m = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        // 1: the monitor's device path instead of its hardware id.
        // SAFETY: as above; the name is a NUL-terminated array of the call before.
        if unsafe { EnumDisplayDevicesW(PCWSTR(d.DeviceName.as_ptr()), 0, &mut m, 1) }.as_bool() {
            let path = text(&m.DeviceID).to_lowercase();
            if path.contains(&mark) && vdd::MONITOR_HINTS.iter().any(|h| path.contains(h)) {
                return Some(Output { device: text(&d.DeviceName), description: text(&d.DeviceString), id: text(&d.DeviceID), primary: d.StateFlags.0 & DISPLAY_DEVICE_PRIMARY_DEVICE.0 != 0 });
            }
        }
    }
    None
}

/// Is this output part of the desktop?
pub fn attached(device: &str) -> bool {
    outputs().iter().any(|o| o.device == device)
}

/// Write down where an output is to be on the desktop, with what mode, or (`None`) that it is
/// to be no part of it. Nothing changes until [`apply`].
fn stage(device: &str, to: Option<(Mode, i32, i32, bool)>) -> Result<()> {
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
/// the primary display (in a guest where the driver's monitor is the only display, the same).
pub fn find_output() -> Result<(Output, bool)> {
    let all = outputs();
    let by_name = all.iter().find(|o| {
        let what = format!("{} {}", o.description, o.id).to_lowercase();
        vdd::ADAPTER_HINTS.iter().any(|h| what.contains(h))
    });
    match by_name.or_else(|| all.iter().find(|o| o.primary)).or(all.first()) {
        Some(o) => Ok((o.clone(), by_name.is_some())),
        None => bail!("Windows lists no display that is part of the desktop (is this process in a session with a desktop?)"),
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

/// The driver and its monitor.
pub struct Screen {
    driver: Arc<Driver>,
    /// The monitor's output; found again after the driver has re-created the monitor.
    device: String,
    /// The mode was switched for a client and is to be put back.
    switched: bool,
    /// The device has a monitor of its own (the driver's monitor 1): its output now.
    own: Option<String>,
    /// What the first monitor was before a device's monitor took its place as the main
    /// display or the only one: put back when the device leaves.
    first_was: Option<Mode>,
}

impl Screen {
    pub fn open() -> Result<Self> {
        let driver = Driver::open()?;
        driver.ping()?;
        let (output, sure) = find_output()?;
        tracing::info!(driver = %driver.path, output = %output.device, adapter = %output.description, recognised = sure, "display driver");
        Ok(Self { driver: Arc::new(driver), device: output.device, switched: false, own: None, first_was: None })
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
        self.own.is_some() as u32
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
        if attached(&own.device) {
            if let Err(e) = detach(&own.device) {
                tracing::warn!("the driver's second monitor is part of the desktop and couldn't be taken out: {e:#}");
            }
        }
        if let Some(first) = driver_output(0) {
            self.device = first.device;
        }
        true
    }

    /// Give the device a monitor of its own at its size and rate, placed as it asked. `None`:
    /// this driver has no second monitor to give (see the top of this file), and nothing
    /// was changed. The mode comes back as it turned out.
    pub fn give(&mut self, want: Mode, place: Place) -> Result<Option<Mode>> {
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

    /// The device's own monitor leaves the desktop, and the first monitor is as it was.
    fn take_back(&mut self) {
        let Some(own) = self.own.take() else { return };
        // (Found again by its number: the name may have changed under it.)
        let own = driver_output(1).map_or(own, |o| o.device);
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
        let (now, _) = current(&self.device)?;
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
            // The monitor comes back as a new output, perhaps under another name.
            let Ok((output, _)) = find_output() else { continue };
            if modes(&output.device).contains(&want) {
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
