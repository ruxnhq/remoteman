//! Steam's sound devices, which the host uses when they are there: "Steam Streaming Microphone"
//! (a device's microphone is played into it) and "Steam Streaming Speakers" (the default output
//! while a device listens, so that the PC's own speakers go quiet). remoteman doesn't carry
//! Valve's drivers, and doesn't install them: this only looks, and says what it found.
//!
//! What was found about Steam's drivers (a Steam install, read only): there are no .inf files
//! in Steam's folder. `bin\drivers.exe` is "Steam Drivers Archive", a self-contained program of
//! Valve's that has the .inf, .sys and .cat files (Windows10\x64\SteamStreamingMicrophone.inf,
//! SteamStreamingSpeakers.inf, and others for Windows 8.1 and x86) packed inside; its own text
//! says "Driver installation automatically handled by Steam". Steam runs it itself, elevated,
//! when Remote Play or In-Home Streaming is first used. Its arguments aren't documented and the
//! files inside can't be reached without running it, so there is nothing for `pnputil
//! /add-driver` to be given: the installer only detects. Once installed the drivers are in the
//! driver store as `steamstreamingmicrophone.inf_*` and `steamstreamingspeakers.inf_*` folders
//! under `FileRepository`, which is how "installed" is told here.

use std::path::{Path, PathBuf};

/// What was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Steam's folder, if Steam is installed.
    pub steam: Option<PathBuf>,
    /// Steam's own driver installer, if it is there.
    pub drivers_exe: Option<PathBuf>,
    pub microphone: bool,
    pub speakers: bool,
}

/// Steam's folder from what the registry says: `HKLM\SOFTWARE\WOW6432Node\Valve\Steam`
/// `InstallPath`, else `HKCU\Software\Valve\Steam` `SteamPath` (written with forward slashes).
/// The first that is a folder.
pub fn steam_dir(hklm_install_path: Option<&str>, hkcu_steam_path: Option<&str>) -> Option<PathBuf> {
    [hklm_install_path, hkcu_steam_path].into_iter().flatten().map(|p| PathBuf::from(p.trim().replace('/', "\\"))).find(|p| !p.as_os_str().is_empty() && p.is_dir())
}

/// Is a driver with this .inf name in the driver store (`...\System32\DriverStore\FileRepository`)?
fn in_store(store: &Path, inf: &str) -> bool {
    let prefix = format!("{}.inf_", inf.to_lowercase());
    std::fs::read_dir(store).is_ok_and(|entries| entries.flatten().any(|e| e.file_name().to_string_lossy().to_lowercase().starts_with(&prefix)))
}

pub fn detect(steam: Option<PathBuf>, store: &Path) -> Found {
    let drivers_exe = steam.as_ref().map(|s| s.join(r"bin\drivers.exe")).filter(|p| p.is_file());
    Found { steam, drivers_exe, microphone: in_store(store, "steamstreamingmicrophone"), speakers: in_store(store, "steamstreamingspeakers") }
}

/// A word for the step, and the line under it.
pub fn say(found: &Found) -> (String, String) {
    match (found.microphone, found.speakers) {
        (true, true) => ("Found".into(), "Steam's Streaming Microphone and Streaming Speakers are installed: a device's microphone can be used, and the PC's speakers go quiet while a device listens".into()),
        (mic, speakers) => {
            let missing = match (mic, speakers) {
                (false, false) => "Steam's Streaming Microphone and Streaming Speakers aren't installed",
                (false, true) => "Steam's Streaming Microphone isn't installed (the Streaming Speakers are)",
                _ => "Steam's Streaming Speakers aren't installed (the Streaming Microphone is)",
            };
            let needs = "They are what a device's microphone is played into, and what makes the PC's own speakers go quiet while a device listens";
            let how = match (&found.steam, &found.drivers_exe) {
                (None, _) => "Steam isn't installed here; installing Steam provides them, and remoteman uses them once they are there".to_string(),
                (Some(_), Some(exe)) => format!("Steam installs them itself the first time its Remote Play is used, or run Steam's own installer, {}, as administrator (remoteman doesn't run it)", exe.display()),
                (Some(dir), None) => format!("Steam is at {}, but without its driver installer; Steam installs them itself the first time its Remote Play is used", dir.display()),
            };
            ("Not installed".into(), format!("{missing}. {needs}. {how}."))
        }
    }
}

/// Where Windows keeps the drivers it has installed.
pub fn driver_store() -> PathBuf {
    crate::system::windows_dir().join(r"System32\DriverStore\FileRepository")
}

/// Steam's folder on this PC, from the registry.
pub fn registry_steam_dir() -> Option<PathBuf> {
    let hklm = crate::system::open_key(r"SOFTWARE\Valve\Steam", crate::system::VIEW32).and_then(|k| k.get_str("InstallPath"));
    let hkcu = current_user_string(r"Software\Valve\Steam", "SteamPath");
    steam_dir(hklm.as_deref(), hkcu.as_deref())
}

fn current_user_string(path: &str, name: &str) -> Option<String> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::{HSTRING, PCWSTR};
    let (path, name) = (HSTRING::from(path), HSTRING::from(name));
    let mut buffer = [0u16; 1024];
    let mut bytes = (buffer.len() * 2) as u32;
    // SAFETY: the buffer is ours and its size in bytes is said.
    unsafe { RegGetValueW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_SZ, None, Some(buffer.as_mut_ptr() as *mut _), Some(&mut bytes)) }.ok().ok()?;
    let text = String::from_utf16_lossy(&buffer[..(bytes as usize / 2).min(buffer.len())]);
    Some(text.trim_end_matches('\0').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rm-steam-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn steam_is_found_from_either_registry_value() {
        let steam = folder("steam");
        let with_slashes = steam.display().to_string().replace('\\', "/");
        assert_eq!(steam_dir(Some(&steam.display().to_string()), None), Some(steam.clone()));
        assert_eq!(steam_dir(None, Some(&with_slashes)), Some(steam.clone()));
        // A value that points nowhere is passed over for the next.
        assert_eq!(steam_dir(Some(r"C:\no\such\steam"), Some(&with_slashes)), Some(steam.clone()));
        assert_eq!(steam_dir(Some(r"C:\no\such\steam"), None), None);
        assert_eq!(steam_dir(None, None), None);
        let _ = std::fs::remove_dir_all(&steam);
    }

    #[test]
    fn drivers_are_told_from_the_driver_store() {
        let steam = folder("layout-steam");
        let store = folder("layout-store");
        std::fs::create_dir_all(steam.join("bin")).unwrap();
        std::fs::write(steam.join(r"bin\drivers.exe"), b"MZ").unwrap();

        // Steam, not its drivers.
        let found = detect(Some(steam.clone()), &store);
        assert_eq!(found, Found { steam: Some(steam.clone()), drivers_exe: Some(steam.join(r"bin\drivers.exe")), microphone: false, speakers: false });
        let (word, line) = say(&found);
        assert_eq!(word, "Not installed");
        assert!(line.contains("drivers.exe") && line.contains("microphone") || line.contains("Microphone"));

        // The speakers only.
        std::fs::create_dir_all(store.join("steamstreamingspeakers.inf_amd64_0123456789abcdef")).unwrap();
        let found = detect(Some(steam.clone()), &store);
        assert!(!found.microphone && found.speakers);
        assert!(say(&found).1.contains("Streaming Microphone isn't installed"));

        // Both.
        std::fs::create_dir_all(store.join("SteamStreamingMicrophone.inf_amd64_fedcba9876543210")).unwrap();
        let found = detect(Some(steam.clone()), &store);
        assert!(found.microphone && found.speakers);
        assert_eq!(say(&found).0, "Found");

        // No Steam at all.
        let none = detect(None, &folder("empty-store"));
        assert_eq!(none.drivers_exe, None);
        assert!(say(&none).1.contains("Steam isn't installed"));
        for d in [&steam, &store] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
