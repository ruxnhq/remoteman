//! The desktop's arrangement through Windows' display configuration (`SetDisplayConfig`), all
//! of it at once: for a PC with screens of its own.
//!
//! Moving the screens one by one with `ChangeDisplaySettingsEx` went wrong on the owner's PC
//! (2026-11-19): making a device's display the main one moved three screens by three calls,
//! each of which succeeded, and Windows switched the second screen off, whose monitor then
//! slept and dropped off the computer; and while one of the driver's monitors was the main
//! display, Windows refused `ChangeDisplaySettingsEx` for every screen, so the layout couldn't
//! be put back. Here the whole layout goes in one call, which Windows is asked to validate
//! first, and what it did is read back and checked.
//!
//! A screen is an output (`\\.\DISPLAY1`, Windows' "source") showing a monitor (its "target"),
//! told apart by the monitor's device path, which stays the same across restarts
//! (`\\?\DISPLAY#AUSAA06#5&56b7e85&0&UID4353#{…}`; the driver's monitors are `…RMN0001…UID256…`).
//! Windows' rule, found by asking it to validate: a path switched on without a target mode
//! must leave its refresh rate at 0 and its scan-line order unspecified (Windows chooses); a
//! rate is then set on the output with `ChangeDisplaySettingsEx`. A path on the desktop is read
//! with both, so one taken from there at another size (a device's monitor given a new size) was
//! refused with error 87 until its scan-line order was cleared too (2026-11-21).

use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
    DISPLAYCONFIG_MODE_INFO_TYPE_TARGET, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_PIXELFORMAT_32BPP, DISPLAYCONFIG_RATIONAL, DISPLAYCONFIG_ROTATION_IDENTITY, DISPLAYCONFIG_SCALING_PREFERRED, DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_SOURCE_MODE, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS, QUERY_DISPLAY_CONFIG_FLAGS, QueryDisplayConfig,
    SDC_ALLOW_CHANGES, SDC_APPLY, SDC_SAVE_TO_DATABASE, SDC_USE_SUPPLIED_DISPLAY_CONFIG, SDC_VALIDATE, SET_DISPLAY_CONFIG_FLAGS, SetDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID, POINTL};
use windows::Win32::Graphics::Gdi::{CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
use windows::core::HSTRING;

const PATH_ACTIVE: u32 = 1;
const MODE_IDX_INVALID: u32 = 0xffff_ffff;

/// One screen of an arrangement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    /// The output (`\\.\DISPLAY1`).
    pub device: String,
    /// Which monitor it shows: pieces of the monitor's device path, lower case, that must all
    /// be in it in this order, `*` between them (a whole path, or `rmn0001*uid256#`).
    pub monitor: String,
    pub width: u32,
    pub height: u32,
    /// Hz; 0: whatever Windows chooses.
    pub refresh: u32,
    pub left: i32,
    pub top: i32,
}

/// A screen on the desktop now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub device: String,
    /// Its monitor's device path, lower case.
    pub monitor: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// Hz, rounded (0: not known).
    pub refresh: u32,
}

impl Shown {
    /// One of the display driver's monitors (not a screen of the PC's own)?
    pub fn driver(&self) -> bool {
        crate::vdd::MONITOR_HINTS.iter().any(|h| self.monitor.contains(h))
    }
}

/// Does a monitor's device path (lower case) match a [`Want::monitor`] pattern?
pub fn matches(path: &str, pattern: &str) -> bool {
    let mut rest = path;
    for piece in pattern.split('*').filter(|p| !p.is_empty()) {
        match rest.find(piece) {
            Some(at) => rest = &rest[at + piece.len()..],
            None => return false,
        }
    }
    true
}

/// The pattern for the display driver's monitor number `index` (0 for its first).
pub fn driver_monitor(index: u32) -> String {
    format!("{}*uid{}#", crate::vdd::MONITOR_HINTS[0], 256 + index)
}

struct Config {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}

fn query(flags: QUERY_DISPLAY_CONFIG_FLAGS) -> Result<Config> {
    // SAFETY: the arrays are as long as Windows said, and cut to what it filled in.
    unsafe {
        // (The configuration may change between the two calls.)
        for _ in 0..4 {
            let (mut np, mut nm) = (0u32, 0u32);
            let r = GetDisplayConfigBufferSizes(flags, &mut np, &mut nm);
            if r != ERROR_SUCCESS {
                bail!("GetDisplayConfigBufferSizes: error {}", r.0);
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
            let r = QueryDisplayConfig(flags, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None);
            if r == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            if r != ERROR_SUCCESS {
                bail!("QueryDisplayConfig: error {}", r.0);
            }
            paths.truncate(np as usize);
            modes.truncate(nm as usize);
            return Ok(Config { paths, modes });
        }
        bail!("the display configuration kept changing while it was read")
    }
}

fn header(kind: DISPLAYCONFIG_DEVICE_INFO_TYPE, size: usize, adapter: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id }
}

fn text(wide: &[u16]) -> String {
    String::from_utf16_lossy(&wide[..wide.iter().position(|&c| c == 0).unwrap_or(wide.len())])
}

/// An output's name (`\\.\DISPLAY1`).
fn source_name(adapter: LUID, id: u32) -> Option<String> {
    let mut n = DISPLAYCONFIG_SOURCE_DEVICE_NAME { header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(), adapter, id), ..Default::default() };
    // SAFETY: a request that says its own size.
    (unsafe { DisplayConfigGetDeviceInfo(&mut n.header) } == 0).then(|| text(&n.viewGdiDeviceName))
}

/// A monitor's device path, lower case.
fn monitor_path(adapter: LUID, id: u32) -> Option<String> {
    let mut n = DISPLAYCONFIG_TARGET_DEVICE_NAME { header: header(DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(), adapter, id), ..Default::default() };
    // SAFETY: as above.
    (unsafe { DisplayConfigGetDeviceInfo(&mut n.header) } == 0).then(|| text(&n.monitorDevicePath).to_lowercase())
}

fn same(a: LUID, b: LUID) -> bool {
    a.LowPart == b.LowPart && a.HighPart == b.HighPart
}

fn source_mode(c: &Config, p: &DISPLAYCONFIG_PATH_INFO) -> Option<DISPLAYCONFIG_SOURCE_MODE> {
    // SAFETY: without virtual-mode awareness the index is the whole field; the mode's union is
    // read as what its type says it is.
    unsafe {
        let m = c.modes.get(p.sourceInfo.Anonymous.modeInfoIdx as usize)?;
        (m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE).then_some(m.Anonymous.sourceMode)
    }
}

fn hz(r: DISPLAYCONFIG_RATIONAL) -> u32 {
    if r.Denominator == 0 { 0 } else { (r.Numerator as f64 / r.Denominator as f64).round() as u32 }
}

/// The screens on the desktop now.
pub fn shown() -> Result<Vec<Shown>> {
    let c = query(QDC_ONLY_ACTIVE_PATHS)?;
    Ok(c.paths
        .iter()
        .filter_map(|p| {
            let s = source_mode(&c, p)?;
            Some(Shown {
                device: source_name(p.sourceInfo.adapterId, p.sourceInfo.id)?,
                monitor: monitor_path(p.targetInfo.adapterId, p.targetInfo.id).unwrap_or_default(),
                left: s.position.x,
                top: s.position.y,
                width: s.width,
                height: s.height,
                refresh: hz(p.targetInfo.refreshRate),
            })
        })
        .collect())
}

/// The active sources: output name, the source's adapter and id, and its monitor's device path (lower case).
pub fn sources() -> Result<Vec<(String, LUID, u32, String)>> {
    let c = query(QDC_ONLY_ACTIVE_PATHS)?;
    Ok(c.paths
        .iter()
        .filter_map(|p| {
            let (a, id) = (p.sourceInfo.adapterId, p.sourceInfo.id);
            Some((source_name(a, id)?, a, id, monitor_path(p.targetInfo.adapterId, p.targetInfo.id).unwrap_or_default()))
        })
        .collect())
}

/// Is a monitor matching the pattern connected (whether or not it is on the desktop)?
pub fn present(monitor: &str) -> bool {
    let Ok(c) = query(QDC_ALL_PATHS) else { return false };
    c.paths.iter().any(|p| p.targetInfo.targetAvailable.as_bool() && monitor_path(p.targetInfo.adapterId, p.targetInfo.id).is_some_and(|m| matches(&m, monitor)))
}

/// Make the desktop exactly `wants`: these outputs showing these monitors at these sizes and
/// places, everything else off; validated first, applied and kept (Windows' own record of the
/// layout for these monitors), then read back. Refresh rates Windows didn't choose as wanted
/// are set afterwards, as far as Windows lets them be (a warning if not).
pub fn arrange(wants: &[Want]) -> Result<()> {
    anyhow::ensure!(!wants.is_empty(), "an arrangement with no screen");
    anyhow::ensure!(wants.iter().any(|w| (w.left, w.top) == (0, 0)), "an arrangement with no screen at 0,0 (Windows' main display is there)");
    crate::desk::follow();
    let (paths, modes) = supplied(wants)?;
    let valid = validate(&paths, &modes);
    if valid != 0 {
        bail!("Windows won't take this arrangement (SetDisplayConfig validating: error {valid}): {wants:?}");
    }
    // SAFETY: arrays of whole structures, read during the call.
    let done = unsafe { SetDisplayConfig(Some(&paths), Some(&modes), SUPPLIED | SDC_APPLY | SDC_SAVE_TO_DATABASE) };
    if done != 0 {
        bail!("Windows refused this arrangement (SetDisplayConfig: error {done}): {wants:?}");
    }
    for w in wants.iter().filter(|w| w.refresh != 0) {
        if let Err(e) = set_refresh(&w.device, w.refresh) {
            tracing::warn!("{e:#}");
        }
    }
    check(wants)
}

/// How an arrangement is given to Windows: all of it, and Windows may choose what it isn't told.
const SUPPLIED: SET_DISPLAY_CONFIG_FLAGS = SET_DISPLAY_CONFIG_FLAGS(SDC_USE_SUPPLIED_DISPLAY_CONFIG.0 | SDC_ALLOW_CHANGES.0);

/// Would Windows take these paths and modes? 0, or its error. Nothing changes.
fn validate(paths: &[DISPLAYCONFIG_PATH_INFO], modes: &[DISPLAYCONFIG_MODE_INFO]) -> i32 {
    // SAFETY: arrays of whole structures, read during the call.
    unsafe { SetDisplayConfig(Some(paths), Some(modes), SUPPLIED | SDC_VALIDATE) }
}

/// The paths and modes for [`arrange`]: each wanted output on its monitor, with a source mode
/// of the wanted size and place, and the target mode it has now where that is still right.
fn supplied(wants: &[Want]) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
    let all = query(QDC_ALL_PATHS)?;
    let active = query(QDC_ONLY_ACTIVE_PATHS)?;
    let (mut paths, mut modes) = (Vec::new(), Vec::new());
    let (mut sources, mut targets) = (HashSet::new(), HashSet::new());
    for w in wants {
        let found = all
            .paths
            .iter()
            .find(|p| {
                let (s, t) = ((p.sourceInfo.adapterId.LowPart, p.sourceInfo.adapterId.HighPart, p.sourceInfo.id), (p.targetInfo.adapterId.LowPart, p.targetInfo.adapterId.HighPart, p.targetInfo.id));
                p.targetInfo.targetAvailable.as_bool()
                    && !sources.contains(&s)
                    && !targets.contains(&t)
                    && source_name(p.sourceInfo.adapterId, p.sourceInfo.id).is_some_and(|n| n.eq_ignore_ascii_case(&w.device))
                    && monitor_path(p.targetInfo.adapterId, p.targetInfo.id).is_some_and(|m| matches(&m, &w.monitor))
            })
            .with_context(|| format!("{} can't show the monitor {:?}: it isn't connected, or another output has it", w.device, w.monitor))?;
        let mut path = *found;
        sources.insert((path.sourceInfo.adapterId.LowPart, path.sourceInfo.adapterId.HighPart, path.sourceInfo.id));
        targets.insert((path.targetInfo.adapterId.LowPart, path.targetInfo.adapterId.HighPart, path.targetInfo.id));
        path.flags = PATH_ACTIVE;
        let mut sm = DISPLAYCONFIG_MODE_INFO { infoType: DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, id: path.sourceInfo.id, adapterId: path.sourceInfo.adapterId, ..Default::default() };
        sm.Anonymous.sourceMode = DISPLAYCONFIG_SOURCE_MODE { width: w.width, height: w.height, pixelFormat: DISPLAYCONFIG_PIXELFORMAT_32BPP, position: POINTL { x: w.left, y: w.top } };
        modes.push(sm);
        path.sourceInfo.Anonymous.modeInfoIdx = (modes.len() - 1) as u32;
        // The target mode it has now, if this output shows this monitor already at this size
        // and rate; otherwise Windows chooses one (and the rate, see the top).
        let kept = active.paths.iter().find(|a| same(a.sourceInfo.adapterId, path.sourceInfo.adapterId) && a.sourceInfo.id == path.sourceInfo.id && same(a.targetInfo.adapterId, path.targetInfo.adapterId) && a.targetInfo.id == path.targetInfo.id).and_then(|a| {
            let s = source_mode(&active, a)?;
            let rate = hz(a.targetInfo.refreshRate);
            // SAFETY: as in `source_mode`.
            let t = unsafe { active.modes.get(a.targetInfo.Anonymous.modeInfoIdx as usize) }?;
            ((s.width, s.height) == (w.width, w.height) && (w.refresh == 0 || rate == w.refresh) && t.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_TARGET).then(|| (*t, a.targetInfo.refreshRate))
        });
        match kept {
            Some((t, rate)) => {
                modes.push(t);
                path.targetInfo.Anonymous.modeInfoIdx = (modes.len() - 1) as u32;
                path.targetInfo.refreshRate = rate;
            }
            None => no_target_mode(&mut path),
        }
        if path.targetInfo.rotation.0 == 0 {
            path.targetInfo.rotation = DISPLAYCONFIG_ROTATION_IDENTITY;
        }
        if path.targetInfo.scaling.0 == 0 {
            path.targetInfo.scaling = DISPLAYCONFIG_SCALING_PREFERRED;
        }
        paths.push(path);
    }
    Ok((paths, modes))
}

/// A path that leaves its target mode to Windows, and with it the rate and the scan-line order
/// (see the top). A path on the desktop is read with both, which Windows then refuses.
fn no_target_mode(path: &mut DISPLAYCONFIG_PATH_INFO) {
    path.targetInfo.Anonymous.modeInfoIdx = MODE_IDX_INVALID;
    path.targetInfo.refreshRate = DISPLAYCONFIG_RATIONAL { Numerator: 0, Denominator: 0 };
    path.targetInfo.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED;
}

/// Is the desktop exactly `wants` (every refresh aside)? An error says how it isn't.
pub fn check(wants: &[Want]) -> Result<()> {
    let now = shown()?;
    for w in wants {
        let Some(s) = now.iter().find(|s| s.device.eq_ignore_ascii_case(&w.device)) else { bail!("after arranging, {} isn't on the desktop: {now:?}", w.device) };
        if !matches(&s.monitor, &w.monitor) || (s.left, s.top, s.width, s.height) != (w.left, w.top, w.width, w.height) {
            bail!("after arranging, {} is {s:?}, not as wanted: {w:?}", w.device);
        }
    }
    if now.len() != wants.len() {
        bail!("after arranging, the desktop has {} screens, not {}: {now:?}", now.len(), wants.len());
    }
    Ok(())
}

/// An output's refresh rate, kept, through `ChangeDisplaySettingsEx` (see the top).
pub fn set_refresh(device: &str, refresh: u32) -> Result<()> {
    crate::desk::follow();
    let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    let name = HSTRING::from(device);
    // SAFETY: `dm` is sized as the call wants.
    if !unsafe { EnumDisplaySettingsW(&name, ENUM_CURRENT_SETTINGS, &mut dm) }.as_bool() {
        bail!("Windows wouldn't say what mode {device} is in");
    }
    if dm.dmDisplayFrequency == refresh {
        return Ok(());
    }
    dm.dmDisplayFrequency = refresh;
    dm.dmFields = DM_DISPLAYFREQUENCY;
    // SAFETY: a whole DEVMODE; no window, no parameters.
    let r = unsafe { ChangeDisplaySettingsExW(&name, Some(&dm), None, CDS_UPDATEREGISTRY, None) };
    if r != DISP_CHANGE_SUCCESSFUL {
        bail!("Windows refused {refresh} Hz on {device} (ChangeDisplaySettingsEx: {})", r.0);
    }
    Ok(())
}

/// The desktop without the display driver's monitors: the PC's own screens kept as they are,
/// moved together so that one of them is at 0,0 (`main`, a monitor pattern, if it is among
/// them; else the one at 0,0 already; else the first). `false`: no driver monitor was on it.
pub fn drop_driver(main: Option<&str>) -> Result<bool> {
    let now = shown()?;
    if !now.iter().any(Shown::driver) {
        return Ok(false);
    }
    let own: Vec<&Shown> = now.iter().filter(|s| !s.driver()).collect();
    anyhow::ensure!(!own.is_empty(), "only the driver's monitors are on the desktop: left as they are");
    let first = main.and_then(|m| own.iter().find(|s| matches(&s.monitor, m))).or_else(|| own.iter().find(|s| (s.left, s.top) == (0, 0))).unwrap_or(&own[0]);
    let (dx, dy) = (-first.left, -first.top);
    let wants: Vec<Want> = own.iter().map(|s| Want { device: s.device.clone(), monitor: s.monitor.clone(), width: s.width, height: s.height, refresh: 0, left: s.left + dx, top: s.top + dy }).collect();
    arrange(&wants)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Devices::Display::DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;

    #[test]
    fn patterns_match_in_order() {
        let drv = r"\\?\display#rmn0001#1&28a6823a&0&uid256#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        assert!(matches(drv, &driver_monitor(0)));
        assert!(!matches(drv, &driver_monitor(1)));
        assert!(matches(drv, drv));
        assert!(!matches(r"\\?\display#ausaa06#5&56b7e85&0&uid4353#{x}", &driver_monitor(0)));
        // In order: the product first, then the number.
        assert!(!matches(r"\\?\display#uid256#rmn0001", &driver_monitor(0)));
    }

    #[test]
    fn a_path_without_its_target_mode_says_nothing_of_one() {
        // As a path on the desktop is read: a target mode, its rate and its scan-line order.
        let mut path = DISPLAYCONFIG_PATH_INFO { flags: PATH_ACTIVE, ..Default::default() };
        path.targetInfo.Anonymous.modeInfoIdx = 3;
        path.targetInfo.refreshRate = DISPLAYCONFIG_RATIONAL { Numerator: 240016, Denominator: 1000 };
        path.targetInfo.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
        no_target_mode(&mut path);
        // SAFETY: as in `source_mode`.
        assert_eq!(unsafe { path.targetInfo.Anonymous.modeInfoIdx }, MODE_IDX_INVALID);
        assert_eq!((path.targetInfo.refreshRate.Numerator, path.targetInfo.refreshRate.Denominator), (0, 0));
        assert_eq!(path.targetInfo.scanLineOrdering, DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED);
    }

    /// The screens on the desktop now, each wanted at a rate it isn't at, so that none keeps its
    /// target mode (as a device's monitor on the desktop that is given another size): Windows,
    /// only asked whether it would take that, says yes. Nothing on the desktop changes. Passes
    /// by itself where there is no desktop to ask.
    #[test]
    fn screens_on_the_desktop_without_their_target_modes_validate() {
        let Ok(now) = shown() else { return };
        if now.is_empty() || now.iter().enumerate().any(|(i, s)| now[..i].iter().any(|o| o.device == s.device)) {
            return;
        }
        let wants: Vec<Want> = now.iter().map(|s| Want { device: s.device.clone(), monitor: s.monitor.clone(), width: s.width, height: s.height, refresh: s.refresh + 1, left: s.left, top: s.top }).collect();
        let (paths, modes) = supplied(&wants).unwrap();
        // SAFETY: as in `source_mode`.
        assert!(paths.iter().all(|p| unsafe { p.targetInfo.Anonymous.modeInfoIdx } == MODE_IDX_INVALID));
        assert_eq!(validate(&paths, &modes), 0, "Windows won't take {wants:?}");
    }
}
