//! The Windows scale (Settings > Display > Scale) of the device's monitor.
//!
//! The client says what scale it wants the desktop drawn at (2.0 on an iPad); the monitor the
//! driver gives it is left by Windows at whatever scale Windows picks for it, so text would be
//! tiny or huge on the device. After the monitor is on the desktop at its mode, its scale is set
//! to the nearest step Windows has, through `DisplayConfigSetDeviceInfo` (type -4, undocumented
//! but what the Settings app itself uses) and read through type -3. Only the driver's monitors
//! are ever touched, never a screen of the PC's own.

use windows::Win32::Devices::Display::{DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_TYPE, DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo};
use windows::Win32::Foundation::LUID;

const GET_DPI_SCALE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(-3);
const SET_DPI_SCALE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(-4);

/// Windows' scale steps, in percent. A scale is stored relative to the recommended step's place here.
const STEPS: [u32; 12] = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];

#[repr(C)]
#[derive(Default)]
struct GetScale {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    min: i32,
    cur: i32,
    max: i32,
}

#[repr(C)]
#[derive(Default)]
struct SetScale {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    rel: i32,
}

/// What Windows says of a source's scale, all relative to the recommended step (0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub min: i32,
    pub cur: i32,
    pub max: i32,
}

pub fn get(adapter: LUID, id: u32) -> Option<Range> {
    let mut r = GetScale { header: DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: GET_DPI_SCALE, size: size_of::<GetScale>() as u32, adapterId: adapter, id }, ..Default::default() };
    // SAFETY: a request that says its own size, in a struct of the layout Windows reads.
    (unsafe { DisplayConfigGetDeviceInfo(&mut r.header) } == 0).then_some(Range { min: r.min, cur: r.cur, max: r.max })
}

fn set(adapter: LUID, id: u32, rel: i32) -> bool {
    let mut r = SetScale { header: DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: SET_DPI_SCALE, size: size_of::<SetScale>() as u32, adapterId: adapter, id }, rel };
    // SAFETY: as above.
    unsafe { DisplayConfigSetDeviceInfo(&mut r.header) == 0 }
}

/// The step (an index into [`STEPS`]) nearest a scale; halfway goes up.
fn nearest(scale: f32) -> usize {
    let pct = scale * 100.0;
    let mut best = 0;
    for (i, &s) in STEPS.iter().enumerate() {
        if (s as f32 - pct).abs() <= (STEPS[best] as f32 - pct).abs() {
            best = i;
        }
    }
    best
}

/// The relative scale to set for a device's scale, given what Windows allows; `None`: nothing to
/// do (no scale asked, or it is the one already set). The recommended step sits `-min` steps up
/// the table, as the lowest step is always the table's first.
pub fn plan(scale: f32, r: Range) -> Option<i32> {
    if !scale.is_finite() || (scale - 1.0).abs() < 0.005 || r.min > 0 || r.max < r.min {
        return None;
    }
    let recommended = -r.min;
    let rel = (nearest(scale) as i32 - recommended).clamp(r.min, r.max);
    (rel != r.cur).then_some(rel)
}

/// The percentage a relative scale stands for.
fn percent(rel: i32, r: Range) -> u32 {
    STEPS[((-r.min + rel).clamp(0, STEPS.len() as i32 - 1)) as usize]
}

/// Give the device's monitor (an output, `\\.\DISPLAY5`) the scale the device asked for. Best
/// effort; only a monitor of the driver's is ever changed. Not in mirror mode (the caller's).
pub fn apply(device: &str, scale: f32) {
    if !scale.is_finite() || (scale - 1.0).abs() < 0.005 {
        return;
    }
    let sources = match crate::ccd::sources() {
        Ok(s) => s,
        Err(e) => return tracing::warn!("the device's scale isn't set: {e:#}"),
    };
    let Some((_, adapter, id, monitor)) = sources.into_iter().find(|s| s.0 == device) else {
        return tracing::warn!(device, "the device's scale isn't set: the output isn't on the desktop");
    };
    if !crate::vdd::MONITOR_HINTS.iter().any(|h| monitor.contains(h)) {
        return tracing::warn!(device, "the device's scale isn't set: not a monitor of the driver's");
    }
    let Some(r) = get(adapter, id) else {
        return tracing::warn!(device, "the device's scale isn't set: Windows won't say what scales it has");
    };
    match plan(scale, r) {
        None => tracing::info!(device, wanted = scale, now = percent(r.cur, r), "the device's monitor already has the scale asked for"),
        Some(rel) => {
            if set(adapter, id, rel) {
                tracing::info!(device, wanted = scale, from = percent(r.cur, r), to = percent(rel, r), "the device's monitor scale set");
            } else {
                tracing::warn!(device, wanted = scale, to = percent(rel, r), "Windows refused the scale for the device's monitor");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn r(min: i32, cur: i32, max: i32) -> Range {
        Range { min, cur, max }
    }

    #[test]
    fn nearest_steps() {
        assert_eq!(STEPS[nearest(1.0)], 100);
        assert_eq!(STEPS[nearest(2.0)], 200);
        assert_eq!(STEPS[nearest(1.3)], 125);
        assert_eq!(STEPS[nearest(1.9)], 200);
        assert_eq!(STEPS[nearest(2.75)], 300);
        assert_eq!(STEPS[nearest(3.0)], 300);
        assert_eq!(STEPS[nearest(7.0)], 500);
        assert_eq!(STEPS[nearest(0.5)], 100);
    }

    #[test]
    fn relative_to_recommended() {
        // Recommended 100% (min 0): 200% is four steps up.
        assert_eq!(plan(2.0, r(0, 0, 11)), Some(4));
        // Recommended 150% (index 2): 200% is two up, 125% one down.
        assert_eq!(plan(2.0, r(-2, 0, 9)), Some(2));
        assert_eq!(plan(1.25, r(-2, 0, 9)), Some(-1));
        // Already there.
        assert_eq!(plan(2.0, r(-2, 2, 9)), None);
    }

    #[test]
    fn clamped_to_what_windows_allows() {
        // A small mode where Windows allows only up to 150%.
        assert_eq!(plan(3.0, r(0, 0, 2)), Some(2));
        assert_eq!(plan(3.0, r(0, 2, 2)), None);
        // Recommended 200% (index 4), two steps above allowed: asking for 500% gets 300%.
        assert_eq!(plan(5.0, r(-4, 0, 2)), Some(2));
    }

    #[test]
    fn no_scale_leaves_it() {
        assert_eq!(plan(1.0, r(-2, 1, 9)), None);
        assert_eq!(plan(f32::NAN, r(-2, 1, 9)), None);
    }

    #[test]
    fn percent_of_rel() {
        assert_eq!(percent(2, r(-2, 0, 9)), 200);
        assert_eq!(percent(-2, r(-2, 0, 9)), 100);
    }

    /// Read-only: what Windows says of each real monitor's scale (never sets anything).
    #[test]
    fn reads_the_real_monitors() {
        for (name, adapter, id, monitor) in crate::ccd::sources().unwrap_or_default() {
            let got = get(adapter, id);
            println!("{name} source {id} {monitor}: {got:?}");
        }
    }
}
