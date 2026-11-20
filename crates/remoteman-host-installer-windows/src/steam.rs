//! Steam's sound devices, which the host uses when they are there: "Steam Streaming Microphone"
//! (a device's microphone is played into it) and "Steam Streaming Speakers" (the default output
//! while a device listens, so that the PC's own speakers go quiet). remoteman doesn't carry
//! Valve's drivers: when Steam is installed and they are missing, and the owner agrees (a
//! checkbox on the first page), setup.ps1's `steam` verb (elevated) installs them from the
//! owner's own Steam folder.
//!
//! What was found about Steam's drivers (a Steam install, read only): there are no .inf files
//! in Steam's folder. `bin\drivers.exe` is "Steam Drivers Archive", a self-extracting program of
//! Valve's (signed by Valve Corp.) with an ordinary ZIP appended (a stub, then a ZIP whose
//! offsets count from its own start). In it, `drivers/Windows10/x64/` has
//! SteamStreamingSpeakers.inf/.sys/.cat and SteamStreamingMicrophone.inf/.sys/.cat (the
//! catalogs signed by Microsoft's hardware publisher, the .sys by Valve), and the same for
//! Windows 8.1 and x86, and for steamxbox (not used). The INFs are class MEDIA, models
//! `ROOT\SteamStreamingSpeakers` and `ROOT\SteamStreamingMicrophone` (root-enumerated virtual
//! devices), and need no coinstaller. setup.ps1 extracts those six files (drivers.exe is never
//! run), adds each package with `pnputil /add-driver`, makes the root device with SetupDi (as
//! it does for the display driver) and puts the driver on it. Once installed the drivers are in
//! the driver store as `steamstreamingmicrophone.inf_*` and `steamstreamingspeakers.inf_*`
//! folders under `FileRepository`, which is how "installed" is told here. This file only
//! looks (for a dry run it reads drivers.exe's directory); it installs nothing itself.

use std::io::{Read, Seek, SeekFrom};
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

impl Found {
    /// Steam is here with its driver archive, and some of its sound devices aren't: they can be
    /// installed from it.
    pub fn installable(&self) -> bool {
        self.steam.is_some() && self.drivers_exe.is_some() && !(self.microphone && self.speakers)
    }
}

/// The entries of drivers.exe's ZIP that are installed: each device's .inf, .sys and .cat, for
/// Windows 10 on x64 (not steamxbox, not the older or 32-bit folders), as the names are in the
/// ZIP. The hardware ids of the INFs are told by `hardware_ids`.
pub fn wanted(names: &[String]) -> Vec<String> {
    const DIR: &str = "drivers/windows10/x64/";
    names
        .iter()
        .filter(|n| {
            let l = n.to_lowercase();
            l.strip_prefix(DIR).is_some_and(|f| ["steamstreamingspeakers", "steamstreamingmicrophone"].iter().any(|d| ["inf", "sys", "cat"].iter().any(|e| f == format!("{d}.{e}"))))
        })
        .cloned()
        .collect()
}

/// The names in a ZIP's central directory (`cd`: its bytes).
pub fn central_directory_names(cd: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut at = 0usize;
    // Each record: "PK\1\2", fields (name length at 28, extra at 30, comment at 32) up to 46 bytes, then the name, the extra, the comment.
    while at + 46 <= cd.len() && cd[at..at + 4] == [0x50, 0x4B, 1, 2] {
        let le16 = |o: usize| u16::from_le_bytes([cd[at + o], cd[at + o + 1]]) as usize;
        let (name, extra, comment) = (le16(28), le16(30), le16(32));
        if at + 46 + name > cd.len() {
            break;
        }
        names.push(String::from_utf8_lossy(&cd[at + 46..at + 46 + name]).into_owned());
        at += 46 + name + extra + comment;
    }
    names
}

/// The names in the ZIP appended to an exe (a self-extracting program): the end record is looked
/// for in the last 64 KB, and the directory is read from just before it, so that the stub in
/// front doesn't matter. Nothing is inflated.
pub fn appended_zip_names(file: &mut (impl Read + Seek)) -> std::io::Result<Vec<String>> {
    let bad = |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string());
    let len = file.seek(SeekFrom::End(0))?;
    let tail_len = len.min(22 + 65535);
    file.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = vec![0u8; tail_len as usize];
    file.read_exact(&mut tail)?;
    let eocd = (0..tail.len().saturating_sub(21)).rev().find(|&i| tail[i..i + 4] == [0x50, 0x4B, 5, 6]).ok_or_else(|| bad("no ZIP in it"))?;
    let le32 = |o: usize| u32::from_le_bytes([tail[eocd + o], tail[eocd + o + 1], tail[eocd + o + 2], tail[eocd + o + 3]]) as u64;
    let (size, offset) = (le32(12), le32(16));
    let cd_at = (len - tail_len + eocd as u64).checked_sub(size).ok_or_else(|| bad("the ZIP's directory is longer than the file"))?;
    // The stub's length is cd_at - offset; a negative one means the offsets don't line up.
    if cd_at < offset {
        return Err(bad("the ZIP's offsets do not line up"));
    }
    file.seek(SeekFrom::Start(cd_at))?;
    let mut cd = vec![0u8; size as usize];
    file.read_exact(&mut cd)?;
    Ok(central_directory_names(&cd))
}

/// The hardware ids an INF's models name: in each section `[Manufacturer]` leads to, the lines
/// `%description% = install-section, HARDWAREID[, compatible id...]`; each once, in order.
#[cfg_attr(not(test), allow(dead_code))] // setup.ps1 knows the two ids itself; this checks them against the INFs
pub fn hardware_ids(inf: &str) -> Vec<String> {
    let lines: Vec<String> = inf.lines().map(|l| l.split(';').next().unwrap_or("").trim().to_string()).collect();
    let section = |name: &str| -> Vec<String> {
        let mut body = Vec::new();
        let mut inside = false;
        for l in &lines {
            if l.starts_with('[') && l.ends_with(']') {
                inside = l[1..l.len() - 1].eq_ignore_ascii_case(name);
            } else if inside && !l.is_empty() {
                body.push(l.clone());
            }
        }
        body
    };
    // [Manufacturer] lines: `%name% = models[, TargetOS...]`; a model section is `models.TargetOS`.
    let mut sections = Vec::new();
    for l in section("Manufacturer") {
        let Some((_, rhs)) = l.split_once('=') else { continue };
        let mut parts = rhs.split(',').map(str::trim);
        let Some(models) = parts.next().filter(|m| !m.is_empty()) else { continue };
        let targets: Vec<&str> = parts.collect();
        sections.push(models.to_string());
        sections.extend(targets.iter().map(|t| format!("{models}.{t}")));
    }
    let mut ids: Vec<String> = Vec::new();
    for name in sections {
        for l in section(&name) {
            let Some((_, rhs)) = l.split_once('=') else { continue };
            for id in rhs.split(',').skip(1).map(str::trim).filter(|i| !i.is_empty()) {
                if !ids.iter().any(|x| x.eq_ignore_ascii_case(id)) {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids
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

    /// The parts of Valve's SteamStreamingSpeakers.inf (Windows10\x64, read from this PC's
    /// drivers.exe) that name its devices.
    const SPEAKERS_INF: &str = "[Version]\r\nSignature=\"$CHICAGO$\"\r\nClass=MEDIA\r\n\r\n[Manufacturer]\r\n%Manufacturer%=SteamStreamingSpeakersDevice,NTamd64\r\n\r\n[SteamStreamingSpeakersDevice.NTamd64]\r\n%SteamStreamingSpeakers.DeviceDesc%=SteamStreamingSpeakers,ROOT\\SteamStreamingSpeakers\r\n\r\n[DestinationDirs]\r\nSteamStreamingSpeakers.CopyList=10,system32\\drivers\r\n\r\n[DeviceInstall32]\r\nAddDevice = ROOT\\SteamStreamingSpeakers\\0000,,SteamStreamingSpeakers_RootDevice_Inst\r\n\r\n[SteamStreamingSpeakers_RootDevice_Inst]\r\nHardwareIds = ROOT\\SteamStreamingSpeakers\r\n";

    #[test]
    fn hardware_ids_are_read_from_the_models() {
        assert_eq!(hardware_ids(SPEAKERS_INF), vec![r"ROOT\SteamStreamingSpeakers".to_string()]);
        // Compatible ids too, once each; comments and other sections' lines are not models.
        let inf = "[Manufacturer]\n%m%=Dev,NTamd64,NTamd64.10.0\n[Dev.NTamd64]\n%d%=Inst,ROOT\\A,ROOT\\B ; trailing\n[Dev.NTamd64.10.0]\n%d%=Inst,root\\a\n[Other]\n%x%=Y,ROOT\\NOT\n";
        assert_eq!(hardware_ids(inf), vec![r"ROOT\A".to_string(), r"ROOT\B".to_string()]);
        assert!(hardware_ids("[Version]\nClass=MEDIA\n").is_empty());
    }

    /// A central directory record for `name`, and a ZIP of stored empty files behind a stub.
    fn zip_with_stub(stub: usize, names: &[&str]) -> Vec<u8> {
        let mut data = vec![0xAAu8; stub];
        let mut cd = Vec::new();
        for n in names {
            let offset = (data.len() - stub) as u32;
            data.extend_from_slice(&[0x50, 0x4B, 3, 4]);
            data.extend_from_slice(&[0u8; 22]);
            data.extend_from_slice(&(n.len() as u16).to_le_bytes());
            data.extend_from_slice(&[0, 0]);
            data.extend_from_slice(n.as_bytes());
            cd.extend_from_slice(&[0x50, 0x4B, 1, 2]);
            let mut fields = [0u8; 42];
            fields[24..26].copy_from_slice(&(n.len() as u16).to_le_bytes());
            fields[38..42].copy_from_slice(&offset.to_le_bytes());
            cd.extend_from_slice(&fields);
            cd.extend_from_slice(n.as_bytes());
        }
        let cd_offset = (data.len() - stub) as u32;
        data.extend_from_slice(&cd);
        data.extend_from_slice(&[0x50, 0x4B, 5, 6, 0, 0, 0, 0]);
        data.extend_from_slice(&(names.len() as u16).to_le_bytes());
        data.extend_from_slice(&(names.len() as u16).to_le_bytes());
        data.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        data.extend_from_slice(&cd_offset.to_le_bytes());
        data.extend_from_slice(&[0, 0]);
        data
    }

    #[test]
    fn the_zip_after_a_stub_is_listed_and_the_wanted_files_picked() {
        let names = [
            "drivers/",
            "drivers/Windows10/x64/steamstreamingmicrophone.cat",
            "drivers/Windows10/x64/SteamStreamingMicrophone.inf",
            "drivers/Windows10/x64/SteamStreamingMicrophone.sys",
            "drivers/Windows10/x64/SteamStreamingSpeakers.inf",
            "drivers/Windows10/x64/steamxbox.inf",
            "drivers/Windows10/x64/WdfCoinstaller01009.dll",
            "drivers/Windows10/x86/SteamStreamingSpeakers.inf",
            "drivers/Windows8.1/x64/SteamStreamingSpeakers.inf",
        ];
        for stub in [0, 1, 430232] {
            let listed = appended_zip_names(&mut std::io::Cursor::new(zip_with_stub(stub, &names))).unwrap();
            assert_eq!(listed, names, "stub {stub}");
        }
        let picked = wanted(&names.map(String::from));
        assert_eq!(
            picked,
            [
                "drivers/Windows10/x64/steamstreamingmicrophone.cat",
                "drivers/Windows10/x64/SteamStreamingMicrophone.inf",
                "drivers/Windows10/x64/SteamStreamingMicrophone.sys",
                "drivers/Windows10/x64/SteamStreamingSpeakers.inf",
            ]
        );
        // Not a ZIP, and a ZIP whose offsets can't be right.
        assert!(appended_zip_names(&mut std::io::Cursor::new(vec![0u8; 100])).is_err());
        let mut broken = zip_with_stub(0, &names);
        let end = broken.len();
        broken[end - 6..end - 2].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(appended_zip_names(&mut std::io::Cursor::new(broken)).is_err());
    }

    /// Steam's own drivers.exe, if this PC has Steam (read only).
    #[test]
    fn this_pcs_steam_archive_has_what_is_installed() {
        let Some(exe) = registry_steam_dir().map(|s| s.join(r"bin\drivers.exe")).filter(|p| p.is_file()) else {
            eprintln!("no Steam here: skipped");
            return;
        };
        let names = appended_zip_names(&mut std::fs::File::open(exe).unwrap()).unwrap();
        let mut picked = wanted(&names);
        picked.sort_by_key(|n| n.to_lowercase());
        let lower: Vec<String> = picked.iter().map(|n| n.to_lowercase()).collect();
        assert_eq!(
            lower,
            [
                "drivers/windows10/x64/steamstreamingmicrophone.cat",
                "drivers/windows10/x64/steamstreamingmicrophone.inf",
                "drivers/windows10/x64/steamstreamingmicrophone.sys",
                "drivers/windows10/x64/steamstreamingspeakers.cat",
                "drivers/windows10/x64/steamstreamingspeakers.inf",
                "drivers/windows10/x64/steamstreamingspeakers.sys",
            ]
        );
    }

    #[test]
    fn installable_needs_steam_its_archive_and_something_missing() {
        let all = |steam: bool, exe: bool, mic: bool, speakers: bool| Found { steam: steam.then(|| PathBuf::from("s")), drivers_exe: exe.then(|| PathBuf::from("s/d")), microphone: mic, speakers };
        assert!(all(true, true, false, false).installable());
        assert!(all(true, true, true, false).installable());
        assert!(!all(true, true, true, true).installable());
        assert!(!all(true, false, false, false).installable());
        assert!(!all(false, false, false, false).installable());
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
