//! HDR on a monitor, as Windows' "Use HDR" switch has it: whether the monitor can show it,
//! whether it is on, switching it, and where Windows puts SDR white on it. Through the display
//! configuration interface (`DisplayConfigGetDeviceInfo` / `SetDeviceInfo`), found by the
//! monitor's GDI name (`\\.\DISPLAY3`).
//!
//! The display driver's monitors say in their EDID that they take HDR10 at 10 bits, so Windows
//! offers the switch for them; with it on, the driver's frames come as a wide surface (16-bit
//! float scRGB, or 10-bit PQ) marked HDR.

use anyhow::{Result, bail};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, LUID};

/// Windows' white for SDR content on an HDR monitor when it can't be asked: 200 nits is what
/// it uses until the user moves the slider (and 80 is the floor).
pub const USUAL_SDR_WHITE: f32 = 200.0;

/// The display path of the monitor showing `device`: its target's adapter and number.
fn target(device: &str) -> Option<(LUID, u32)> {
    // SAFETY: the arrays are as long as Windows said, and each request says its own size.
    unsafe {
        let (mut paths, mut modes) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths, &mut modes) != ERROR_SUCCESS {
            return None;
        }
        let mut path = vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize];
        let mut mode = vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut paths, path.as_mut_ptr(), &mut modes, mode.as_mut_ptr(), None) != ERROR_SUCCESS {
            return None;
        }
        path.truncate(paths as usize);
        for p in path {
            let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME { header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(), p.sourceInfo.adapterId, p.sourceInfo.id), ..Default::default() };
            if DisplayConfigGetDeviceInfo(&mut name.header) != 0 {
                continue;
            }
            let gdi = &name.viewGdiDeviceName;
            let gdi = String::from_utf16_lossy(&gdi[..gdi.iter().position(|&c| c == 0).unwrap_or(gdi.len())]);
            if gdi.eq_ignore_ascii_case(device) {
                return Some((p.targetInfo.adapterId, p.targetInfo.id));
            }
        }
        None
    }
}

fn header(kind: DISPLAYCONFIG_DEVICE_INFO_TYPE, size: usize, adapter: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id }
}

/// Whether the monitor showing `device` can show HDR, and whether it is on. `None`: it isn't
/// part of the desktop, or Windows won't say.
pub fn state(device: &str) -> Option<(bool, bool)> {
    let (adapter, id) = target(device)?;
    let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO { header: header(DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(), adapter, id), ..Default::default() };
    // SAFETY: a request that says its own size; the bit fields are read from the plain value.
    unsafe {
        if DisplayConfigGetDeviceInfo(&mut info.header) != 0 {
            return None;
        }
        let bits = info.Anonymous.value;
        Some((bits & 1 != 0, bits & 2 != 0))
    }
}

/// Switch HDR on or off for the monitor showing `device`.
pub fn set(device: &str, on: bool) -> Result<()> {
    let Some((adapter, id)) = target(device) else { bail!("{device} isn't part of the desktop") };
    let mut set = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE { header: header(DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE, size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>(), adapter, id), ..Default::default() };
    set.Anonymous.value = on as u32;
    // SAFETY: a request that says its own size.
    let said = unsafe { DisplayConfigSetDeviceInfo(&set.header) };
    if said != 0 {
        bail!("Windows refused to switch HDR {} on {device} (error {said})", if on { "on" } else { "off" });
    }
    Ok(())
}

/// Where Windows puts SDR white on the monitor showing `device`, in nits.
pub fn sdr_white(device: &str) -> f32 {
    let Some((adapter, id)) = target(device) else { return USUAL_SDR_WHITE };
    let mut level = DISPLAYCONFIG_SDR_WHITE_LEVEL { header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(), adapter, id), SDRWhiteLevel: 0 };
    // SAFETY: a request that says its own size.
    if unsafe { DisplayConfigGetDeviceInfo(&mut level.header) } != 0 || level.SDRWhiteLevel == 0 {
        return USUAL_SDR_WHITE;
    }
    // In thousandths of 80 nits.
    level.SDRWhiteLevel as f32 * 80.0 / 1000.0
}

/// The monitor's HDR as it was before a device asked for it, put back when the device goes.
pub struct Switched {
    device: String,
    was: bool,
}

impl Switched {
    /// Bring HDR on `device` to `on`; `None` if it was so already or can't be.
    pub fn to(device: &str, on: bool) -> Option<Self> {
        let (can, is) = state(device)?;
        if is == on || (on && !can) {
            if on && !can {
                tracing::info!(device, "this monitor can't show HDR");
            }
            return None;
        }
        match set(device, on) {
            Ok(()) => {
                tracing::info!(device, on, "HDR switched on the monitor");
                Some(Self { device: device.into(), was: is })
            }
            Err(e) => {
                tracing::warn!("{e:#}");
                None
            }
        }
    }
}

impl Drop for Switched {
    fn drop(&mut self) {
        match set(&self.device, self.was) {
            Ok(()) => tracing::info!(device = self.device, on = self.was, "HDR on the monitor is as it was"),
            Err(e) => tracing::warn!("couldn't put HDR back: {e:#}"),
        }
    }
}
