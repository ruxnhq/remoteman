//! What each paired device may do, kept with the host's other state, and the host's log of who
//! connected when.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

/// What one device may do here. Everything is allowed until its owner says otherwise, so a
/// file that predates a field (or no file) means "allowed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Permissions {
    /// False: view-only. The device is shown the picture and nothing it types is used.
    pub input: bool,
    pub clipboard: bool,
    pub files: bool,
    pub sound: bool,
    pub mic: bool,
    pub camera: bool,
}

impl Default for Permissions {
    fn default() -> Self {
        Self { input: true, clipboard: true, files: true, sound: true, mic: true, camera: true }
    }
}

impl Permissions {
    /// The host's features minus what this device may not have. `input` takes `pen` with it.
    pub fn filter(&self, features: &[String]) -> Vec<String> {
        features
            .iter()
            .filter(|f| match f.as_str() {
                "input" | "pen" => self.input,
                "clipboard" => self.clipboard,
                "files" => self.files,
                "sound" => self.sound,
                "mic" => self.mic,
                "camera" => self.camera,
                _ => true,
            })
            .cloned()
            .collect()
    }
}

/// Every device's permissions, in `permissions.json` (fingerprint to permissions; a device
/// that isn't in it may do everything).
pub struct PermissionStore {
    path: Option<PathBuf>,
    map: Mutex<BTreeMap<String, Permissions>>,
}

impl PermissionStore {
    pub fn in_memory() -> Self {
        Self { path: None, map: Mutex::default() }
    }

    pub fn load(path: &Path) -> Self {
        let map = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Self { path: Some(path.to_owned()), map: Mutex::new(map) }
    }

    pub fn get(&self, fingerprint: &str) -> Permissions {
        self.map.lock().unwrap().get(fingerprint).copied().unwrap_or_default()
    }

    pub fn set(&self, fingerprint: &str, permissions: Permissions) -> Result<()> {
        let mut map = self.map.lock().unwrap();
        if permissions == Permissions::default() {
            map.remove(fingerprint);
        } else {
            map.insert(fingerprint.to_owned(), permissions);
        }
        self.save(&map)
    }

    /// Forget a device (it was revoked): paired again, it starts with everything allowed.
    pub fn forget(&self, fingerprint: &str) -> Result<()> {
        let mut map = self.map.lock().unwrap();
        if map.remove(fingerprint).is_some() {
            self.save(&map)?;
        }
        Ok(())
    }

    fn save(&self, map: &BTreeMap<String, Permissions>) -> Result<()> {
        if let Some(path) = &self.path {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_string_pretty(map)?).with_context(|| format!("write {}", tmp.display()))?;
            std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))?;
        }
        Ok(())
    }
}

/// One line of the connection log.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConnectionEvent {
    /// UTC, `2026-11-19T10:15:00Z`.
    pub time: String,
    /// `paired`, `connected`, `left` or `refused`.
    pub event: String,
    pub name: String,
    pub fingerprint: String,
    pub address: String,
    /// With `left`: how long the device was here, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    /// Why it left or was refused, if known.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// The device joined another device's session (docs/PROTOCOL.md, "More than one device").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub joined: bool,
}

impl ConnectionEvent {
    pub fn new(event: &str, name: &str, fingerprint: &str, address: &str) -> Self {
        Self { time: utc_now(), event: event.into(), name: name.into(), fingerprint: fingerprint.into(), address: address.into(), duration: None, reason: String::new(), joined: false }
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for now.
pub fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    utc(secs)
}

pub(crate) fn utc(secs: u64) -> String {
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `connections.log`: one JSON object a line, kept below [`LOG_LIMIT`] by moving the full
/// file to `connections.1.log` (the one before that is dropped).
pub struct ConnectionLog {
    path: Option<PathBuf>,
    limit: u64,
    lock: Mutex<()>,
}

pub const LOG_LIMIT: u64 = 1024 * 1024;

impl ConnectionLog {
    pub fn in_memory() -> Self {
        Self { path: None, limit: LOG_LIMIT, lock: Mutex::new(()) }
    }

    pub fn open(path: &Path) -> Self {
        Self { path: Some(path.to_owned()), limit: LOG_LIMIT, lock: Mutex::new(()) }
    }

    #[cfg(test)]
    pub(crate) fn with_limit(mut self, limit: u64) -> Self {
        self.limit = limit;
        self
    }

    fn older(path: &Path) -> PathBuf {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("connections");
        path.with_file_name(format!("{stem}.1.log"))
    }

    pub fn append(&self, event: &ConnectionEvent) {
        let Some(path) = &self.path else { return };
        let _held = self.lock.lock().unwrap();
        let Ok(mut line) = serde_json::to_string(event) else { return };
        line.push('\n');
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::metadata(path).is_ok_and(|m| m.len() + line.len() as u64 > self.limit) {
            let _ = std::fs::rename(path, Self::older(path));
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// The last `n` events, oldest first.
    pub fn last(&self, n: usize) -> Vec<ConnectionEvent> {
        let Some(path) = &self.path else { return Vec::new() };
        let _held = self.lock.lock().unwrap();
        let read = |p: &Path| -> Vec<ConnectionEvent> { std::fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect() };
        let mut all = read(path);
        if all.len() < n {
            let mut older = read(&Self::older(path));
            older.append(&mut all);
            all = older;
        }
        let skip = all.len().saturating_sub(n);
        all.split_off(skip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rm-perm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_old_or_partial_file_means_allowed() {
        let d = temp("old");
        let f = "ab".repeat(32);
        std::fs::write(d.join("p.json"), format!("{{\"{f}\":{{\"input\":false}}}}")).unwrap();
        let s = PermissionStore::load(&d.join("p.json"));
        let p = s.get(&f);
        assert!(!p.input && p.clipboard && p.files && p.sound && p.mic && p.camera);
        assert_eq!(s.get("someone else"), Permissions::default());
        assert_eq!(PermissionStore::load(&d.join("none.json")).get(&f), Permissions::default());
    }

    #[test]
    fn permissions_round_trip() {
        let d = temp("rt");
        let s = PermissionStore::load(&d.join("p.json"));
        let p = Permissions { input: false, mic: false, ..Permissions::default() };
        s.set("fp", p).unwrap();
        assert_eq!(PermissionStore::load(&d.join("p.json")).get("fp"), p);
        s.forget("fp").unwrap();
        assert_eq!(PermissionStore::load(&d.join("p.json")).get("fp"), Permissions::default());
    }

    #[test]
    fn view_only_drops_input_and_pen_from_features() {
        let all: Vec<String> = ["input", "pen", "clipboard", "files", "sound", "mic", "camera", "parity"].iter().map(|s| s.to_string()).collect();
        let p = Permissions { input: false, camera: false, ..Permissions::default() };
        assert_eq!(p.filter(&all), ["clipboard", "files", "sound", "mic", "parity"]);
        assert_eq!(Permissions::default().filter(&all), all);
    }

    #[test]
    fn the_log_appends_is_bounded_and_reads_back() {
        let d = temp("log");
        let log = ConnectionLog::open(&d.join("connections.log")).with_limit(600);
        for i in 0..20 {
            let mut e = ConnectionEvent::new("connected", &format!("Anna {i}"), "fp", "1.2.3.4:5");
            if i % 2 == 1 {
                e.event = "left".into();
                e.duration = Some(i);
                e.reason = "closed".into();
            }
            log.append(&e);
        }
        let last = log.last(5);
        assert_eq!(last.len(), 5);
        assert_eq!(last[4].name, "Anna 19");
        assert_eq!(last[4].duration, Some(19));
        assert_eq!(last[0].name, "Anna 15");
        let size = |n: &str| std::fs::metadata(d.join(n)).map(|m| m.len()).unwrap_or(0);
        assert!(size("connections.log") <= 600 && size("connections.1.log") > 0);
        assert!(log.last(1000).len() < 20);
    }

    #[test]
    fn time_is_utc() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(utc(1_709_164_800 + 86399), "2024-02-29T23:59:59Z");
    }
}
