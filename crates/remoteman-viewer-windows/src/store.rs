//! What the viewer remembers, in the state folder: the paired computers (`viewer-computers`)
//! and the settings (`viewer-settings`). Both are plain `key=value` lines, the same files the
//! Linux viewer writes; this one adds a line or two (`bitrate`, more values of `quality`) that
//! the Linux viewer reads past.
//!
//! The keys themselves are the core's: `client/hosts/<address>` holds the fingerprint of the
//! computer at that address, and a computer without that file isn't paired as far as the core
//! is concerned. So forgetting a computer deletes its file, and a new address moves it.

use std::path::{Path, PathBuf};

use remoteman_core::{Codec, Mode, Placement};

/// The placements, as they are written on the command line and in the state folder.
pub const PLACEMENTS: [(Placement, &str); 3] = [(Placement::Extend, "extend"), (Placement::Main, "main"), (Placement::Only, "only")];

/// Which stream to ask for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    /// The decoders in their usual order.
    Automatic,
    /// Full colour detail (4:4:4) first. Not offered here yet (`Features::sharp_text`); kept
    /// so that a file written by the Linux viewer reads back as it was.
    Sharp,
    Hevc,
    Av1,
}

impl Quality {
    pub fn key(self) -> &'static str {
        match self {
            Quality::Automatic => "auto",
            Quality::Sharp => "sharp",
            Quality::Hevc => "hevc",
            Quality::Av1 => "av1",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "sharp" => Quality::Sharp,
            "hevc" => Quality::Hevc,
            "av1" => Quality::Av1,
            _ => Quality::Automatic,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Quality::Automatic => "Automatic",
            Quality::Sharp => "Sharp text (4:4:4)",
            Quality::Hevc => "HEVC",
            Quality::Av1 => "AV1",
        }
    }

    /// The decoders to offer, most wanted first: the chosen codec in front, the rest behind it
    /// in case the computer can't make that one.
    pub fn order(self, decoders: &[Mode]) -> Vec<Mode> {
        let mut out = decoders.to_vec();
        let first = match self {
            Quality::Hevc => Some(Codec::Hevc),
            Quality::Av1 => Some(Codec::Av1),
            _ => None,
        };
        if let Some(codec) = first {
            // A stable sort: the others stay as they were.
            out.sort_by_key(|m| m.codec != codec);
        }
        out
    }
}

pub fn placement_key(p: Placement) -> &'static str {
    PLACEMENTS.iter().find(|x| x.0 == p).map_or("extend", |x| x.1)
}

fn placement_from_key(key: &str) -> Option<Placement> {
    PLACEMENTS.iter().find(|x| x.1 == key).map(|x| x.0)
}

#[derive(Clone, Debug)]
pub struct Computer {
    pub name: String,
    /// `host:port`, exactly as the core is given it.
    pub address: String,
    /// What the computer said it runs, the last time it answered.
    pub os: String,
    /// When it was paired and last used, in seconds since 1970 (0: not known, never).
    pub paired: i64,
    pub used: i64,
    pub placement: Placement,
    /// `None`: whatever Settings says.
    pub quality: Option<Quality>,
    /// The most the picture may use, in megabits a second (0: the computer's own choice).
    pub bitrate: u32,
}

pub struct Settings {
    /// The placement last used, for a computer that isn't in the list.
    pub placement: Placement,
    /// No title bar while connected; a bar comes out at the top edge.
    pub hover_bar: bool,
    /// The figures over the picture.
    pub figures: bool,
    /// Go to full screen when a session starts.
    pub fullscreen: bool,
    /// Alt+Tab, the Windows key and the like go to the other computer.
    pub shortcuts: bool,
    pub quality: Quality,
}

pub struct Store {
    dir: PathBuf,
    pub computers: Vec<Computer>,
    pub settings: Settings,
    /// Nothing is written: the list is made up, for pictures of the window.
    pub pretend: bool,
}

/// The address as the core wants it: with the usual port if none was given.
pub fn with_port(address: &str) -> String {
    let address = address.trim();
    if address.contains(':') { address.to_owned() } else { format!("{address}:{}", remoteman_core::PORT) }
}

/// The address without its port, for showing: `studio:7791` is `studio`.
pub fn host_part(address: &str) -> &str {
    address.rsplit_once(':').map_or(address, |(h, _)| h)
}

/// The address as it is shown and typed: without the port if it is the usual one.
pub fn short(address: &str) -> &str {
    address.strip_suffix(&format!(":{}", remoteman_core::PORT)).unwrap_or(address)
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Store {
    pub fn load(dir: &Path) -> Self {
        let text = std::fs::read_to_string(dir.join("viewer-settings")).unwrap_or_default();
        let get = |key: &str| text.lines().find_map(|l| l.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v.to_owned()));
        let yes = |key: &str, otherwise: bool| get(key).map_or(otherwise, |v| v == "yes");
        let settings = Settings {
            placement: get("placement").and_then(|v| placement_from_key(&v)).unwrap_or(Placement::Extend),
            hover_bar: yes("hover-bar", false),
            figures: yes("figures", false),
            fullscreen: yes("fullscreen", false),
            shortcuts: yes("shortcuts", true),
            quality: Quality::from_key(&get("quality").unwrap_or_default()),
        };
        let mut store = Self { dir: dir.to_owned(), computers: Vec::new(), settings, pretend: false };
        if let Ok(text) = std::fs::read_to_string(dir.join("viewer-computers")) {
            for line in text.lines() {
                if line == "[computer]" {
                    store.computers.push(Computer { name: String::new(), address: String::new(), os: String::new(), paired: 0, used: 0, placement: Placement::Extend, quality: None, bitrate: 0 });
                    continue;
                }
                let (Some(c), Some((key, value))) = (store.computers.last_mut(), line.split_once('=')) else { continue };
                match key {
                    "name" => c.name = value.to_owned(),
                    "address" => c.address = value.to_owned(),
                    "os" => c.os = value.to_owned(),
                    "paired" => c.paired = value.parse().unwrap_or(0),
                    "used" => c.used = value.parse().unwrap_or(0),
                    "placement" => c.placement = placement_from_key(value).unwrap_or(Placement::Extend),
                    "quality" => c.quality = (value != "same").then(|| Quality::from_key(value)),
                    "bitrate" => c.bitrate = value.parse().unwrap_or(0),
                    _ => {}
                }
            }
            store.computers.retain(|c| !c.address.is_empty());
            store.sort();
        }
        store
    }

    /// Where the core keeps the fingerprint of the computer at this address.
    fn known(&self, address: &str) -> PathBuf {
        self.dir.join("client/hosts").join(address.replace([':', '/'], "_"))
    }

    /// The computer's key fingerprint, if this PC is paired with it.
    pub fn key(&self, address: &str) -> Option<String> {
        std::fs::read_to_string(self.known(address)).ok().map(|k| k.trim().to_owned()).filter(|k| !k.is_empty())
    }

    pub fn find(&self, address: &str) -> Option<&Computer> {
        self.computers.iter().find(|c| c.address == address)
    }

    /// Change one computer and save.
    pub fn update(&mut self, address: &str, change: impl FnOnce(&mut Computer)) {
        if let Some(c) = self.computers.iter_mut().find(|c| c.address == address) {
            change(c);
            self.save_computers();
        }
    }

    /// A session with this computer has just started: put it in the list, or note the use.
    pub fn used(&mut self, address: &str, host_name: &str, os: &str) {
        let now = now();
        match self.computers.iter_mut().find(|c| c.address == address) {
            Some(c) => {
                c.used = now;
                c.os = os.to_owned();
                // One known only by its address takes the name the computer gives itself.
                if c.name == host_part(address) && !host_name.is_empty() {
                    c.name = host_name.to_owned();
                }
            }
            None => {
                let name = if host_name.is_empty() { host_part(address) } else { host_name };
                self.computers.push(Computer { name: name.to_owned(), address: address.to_owned(), os: os.to_owned(), paired: now, used: now, placement: Placement::Extend, quality: None, bitrate: 0 });
            }
        }
        self.sort();
        self.save_computers();
    }

    /// A computer has just been paired: as `used`, but nothing has been used yet.
    pub fn paired(&mut self, address: &str, host_name: &str, os: &str) {
        let had = self.find(address).map(|c| c.used);
        self.used(address, host_name, os);
        let now = now();
        self.update(address, |c| {
            c.paired = now;
            c.used = had.unwrap_or(0);
        });
        self.sort();
        self.save_computers();
    }

    /// Forget a computer: out of the list, and its key with it, so it has to be paired again.
    pub fn forget(&mut self, address: &str) {
        if !self.pretend {
            let _ = std::fs::remove_file(self.known(address));
        }
        self.computers.retain(|c| c.address != address);
        self.save_computers();
    }

    /// Give a computer a new address. Its key goes with it.
    pub fn readdress(&mut self, old: &str, new: &str) -> Result<(), String> {
        if old == new {
            return Ok(());
        }
        if self.find(new).is_some() {
            return Err("Another computer in the list has that address".into());
        }
        let (from, to) = (self.known(old), self.known(new));
        if from.exists() && !self.pretend {
            std::fs::rename(&from, &to).map_err(|e| format!("Couldn't move the key: {e}"))?;
        }
        self.update(old, |c| c.address = new.to_owned());
        Ok(())
    }

    /// Most recently used (or paired) first, as the list shows them.
    fn sort(&mut self) {
        self.computers.sort_by_key(|c| std::cmp::Reverse(c.used.max(c.paired)));
    }

    fn save_computers(&self) {
        if self.pretend {
            return;
        }
        let mut out = String::new();
        // One value a line: anything that would break that is left out of a name.
        let clean = |s: &str| s.replace(['\n', '\r'], " ");
        for c in &self.computers {
            out += &format!(
                "[computer]\nname={}\naddress={}\nos={}\npaired={}\nused={}\nplacement={}\nquality={}\nbitrate={}\n",
                clean(&c.name),
                clean(&c.address),
                clean(&c.os),
                c.paired,
                c.used,
                placement_key(c.placement),
                c.quality.map_or("same", |q| q.key()),
                c.bitrate
            );
        }
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(self.dir.join("viewer-computers"), out);
    }

    pub fn save_settings(&self) {
        if self.pretend {
            return;
        }
        let s = &self.settings;
        let yes = |b: bool| if b { "yes" } else { "no" };
        let out = format!(
            "placement={}\nhover-bar={}\nfigures={}\nfullscreen={}\nshortcuts={}\nquality={}\n",
            placement_key(s.placement),
            yes(s.hover_bar),
            yes(s.figures),
            yes(s.fullscreen),
            yes(s.shortcuts),
            s.quality.key()
        );
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(self.dir.join("viewer-settings"), out);
    }
}

/// What a computer says it runs, as the design names it: `linux` is Linux, `macos` is Mac.
pub fn os_name(os: &str) -> String {
    match os.to_ascii_lowercase().as_str() {
        "" => String::new(),
        "linux" => "Linux".into(),
        "macos" | "mac" | "darwin" => "Mac".into(),
        "windows" => "Windows".into(),
        "ios" => "iOS".into(),
        "ipados" => "iPadOS".into(),
        _ => os.to_owned(),
    }
}

/// A fingerprint as it is shown: the first `groups` groups of four, in capitals.
pub fn key_words(fingerprint: &str, groups: usize) -> String {
    let hex: Vec<char> = fingerprint.chars().filter(|c| c.is_ascii_hexdigit()).map(|c| c.to_ascii_uppercase()).collect();
    hex.chunks(4).take(groups).map(|g| g.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

/// How far this place's clock is ahead of UTC now, in seconds.
fn local_offset() -> i64 {
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    // SAFETY: plain queries.
    let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let seconds = |t: windows::Win32::Foundation::SYSTEMTIME| days_from_civil(t.wYear as i64, t.wMonth as i64, t.wDay as i64) * 86400 + t.wHour as i64 * 3600 + t.wMinute as i64 * 60;
    seconds(local) - seconds(utc)
}

/// Days since 1970 of a date (the usual algorithm, from Howard Hinnant's notes).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The year, month and day of a count of days since 1970.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (d, m) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// A moment as the design says it, to follow "used" or "paired": `today`, `yesterday`,
/// `3 days ago`, `12 November`.
pub fn when(unix: i64) -> String {
    let offset = local_offset();
    let day = |t: i64| (t + offset).div_euclid(86400);
    let (then, today) = (day(unix), day(now()));
    match today - then {
        i64::MIN..=0 => "today".into(),
        1 => "yesterday".into(),
        n @ 2..=6 => format!("{n} days ago"),
        _ => {
            let ((y, m, d), (this_year, _, _)) = (civil_from_days(then), civil_from_days(today));
            let month = MONTHS[(m - 1).clamp(0, 11) as usize];
            if y == this_year { format!("{d} {month}") } else { format!("{d} {month} {y}") }
        }
    }
}
