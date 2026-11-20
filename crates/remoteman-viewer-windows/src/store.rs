//! What the viewer remembers, in the state folder: the paired computers (`viewer-computers`)
//! and the settings (`viewer-settings`). Both are plain `key=value` lines, the same files the
//! Linux viewer writes; this one adds a line or two (`bitrate`, more values of `quality`) that
//! the Linux viewer reads past.
//!
//! The keys themselves are the core's: `client/hosts/<address>` holds the fingerprint of the
//! computer at that address, and a computer without that file isn't paired as far as the core
//! is concerned. So forgetting a computer deletes its file, and a new address moves it.

use std::path::{Path, PathBuf};

use remoteman_core::{Codec, Mode, Placement, Side};

/// The sides a second display can sit on, in the order they are offered, with their names in
/// the file and in the settings.
pub const SIDES: [(Side, &str, &str); 4] = [(Side::Right, "right", "To the right"), (Side::Left, "left", "To the left"), (Side::Above, "above", "Above"), (Side::Below, "below", "Below")];

/// The placements, as they are written on the command line and in the state folder.
pub const PLACEMENTS: [(Placement, &str); 4] = [(Placement::Extend, "extend"), (Placement::Main, "main"), (Placement::Only, "only"), (Placement::Mirror, "mirror")];

/// The placements as the menus and the settings name them, in the order they are offered.
pub const PLACEMENT_NAMES: [(Placement, &str, &str); 4] = [
    (Placement::Extend, "Extra display", "Beside the computer's own displays, which stay on"),
    (Placement::Main, "Main display", "Its own displays stay on as secondary ones"),
    (Placement::Only, "Only display", "Its own screens turn off while you are connected"),
    (Placement::Mirror, "Mirror its main monitor", "The computer's own picture, at that monitor's size"),
];

/// How large a picture to ask a computer for: the most lines (0: this screen's own), or
/// [`WINDOW`]. The same values, under the same key, as the Linux viewer writes.
pub const SIZES: [(u32, &str); 4] = [(0, "This screen's own"), (WINDOW, "The window's size"), (1440, "Up to 1440 lines"), (1080, "Up to 1080 lines")];

/// In place of a number of lines: a display as large as the window's picture, asked for again
/// when the window has changed size and been still for a second.
pub const WINDOW: u32 = 1;

/// A display no taller than `most` lines, with the same shape and the same apparent size of
/// things on it (fewer pixels, each drawn larger here).
pub fn fit_lines(width: u32, height: u32, scale: f32, most: u32) -> (u32, u32, f32) {
    if most == 0 || most == WINDOW || height <= most {
        return (width, height, scale);
    }
    let by = most as f64 / height as f64;
    (((width as f64 * by).round() as u32) & !1, most & !1, (scale as f64 * by) as f32)
}

/// Which stream to ask for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    /// The decoders in their usual order.
    Automatic,
    /// Full colour detail (4:4:4) first, even where it is decoded on the processor and may
    /// not keep up (it was asked for).
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
        let full = |m: &Mode| m.chroma == remoteman_core::Chroma::C444;
        if self == Quality::Sharp {
            // Full colour detail first; the rest behind it in case the computer can't.
            out.sort_by_key(|m| !full(m));
            return out;
        }
        // Otherwise 4:4:4 only as the last resort: it costs bitrate and, here, the CPU.
        out.sort_by_key(full);
        if let Some(codec) = first {
            // A stable sort: the others stay as they were.
            out.sort_by_key(|m| m.codec != codec || full(m));
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
    /// The most lines of picture to ask for (0: this screen's own; [`WINDOW`]: the window's).
    pub max_height: u32,
    /// What a session with it starts with: its sound played here, one clipboard for both.
    pub sound: bool,
    pub clipboard: bool,
    /// Which side of the computer's other displays a second display here is put on.
    pub second_side: Side,
    /// The hardware addresses of its network cards (`aa:bb:cc:dd:ee:ff`, comma-separated), as
    /// it last advertised them on this network: what waking it is sent to. The Linux viewer's
    /// key, `cards`.
    pub cards: String,
    /// "Wake it over the network": it may be sent the waking packet.
    pub wake: bool,
    /// "Getting in": only a session that is already running, never its login screen (the
    /// hello's `way_in: attach`; the Linux viewer's key, `way-in`). A computer with one way in
    /// takes no notice.
    pub attach_only: bool,
    /// Lines this viewer doesn't use (the Linux viewer's), kept as they were.
    pub other: Vec<(String, String)>,
}

impl Computer {
    fn new(name: &str, address: &str, os: &str, now: i64, settings: &Settings) -> Self {
        // A new computer's display follows the window: as large as the picture's place, and
        // asked for again when the window changes size (full screen included).
        Self { name: name.to_owned(), address: address.to_owned(), os: os.to_owned(), paired: now, used: now, placement: Placement::Extend, quality: None, bitrate: 0, max_height: WINDOW, sound: settings.sound, clipboard: settings.clipboard, second_side: Side::Right, cards: String::new(), wake: true, attach_only: false, other: Vec::new() }
    }

    /// The network cards it can be woken through.
    pub fn card_list(&self) -> Vec<String> {
        self.cards.split(',').map(str::trim).filter(|c| !c.is_empty()).map(str::to_owned).collect()
    }
}

/// A key on this keyboard that can act as the Windows key (Super, Command) on the computer,
/// as in the Linux viewer: the Windows key itself always does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stand {
    /// No other key.
    Own,
    RightAlt,
    RightCtrl,
    CapsLock,
}

impl Stand {
    /// Each with its settings word and its name in the drop-down (the first is the default).
    pub const ALL: [(Stand, &str, &str); 4] = [(Stand::Own, "own", "Super / Command"), (Stand::RightAlt, "right-alt", "Right Alt"), (Stand::RightCtrl, "right-ctrl", "Right Ctrl"), (Stand::CapsLock, "caps-lock", "Caps Lock")];

    pub fn from_key(key: &str) -> Self {
        Self::ALL.iter().find(|s| s.1 == key).map_or(Stand::Own, |s| s.0)
    }

    pub fn key(self) -> &'static str {
        Self::ALL.iter().find(|s| s.0 == self).map_or("own", |s| s.1)
    }

    /// The key's USB usage.
    fn usage(self) -> Option<u16> {
        match self {
            Stand::Own => None,
            Stand::RightAlt => Some(0xE6),
            Stand::RightCtrl => Some(0xE4),
            Stand::CapsLock => Some(0x39),
        }
    }
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
    /// Ask for this monitor's own refresh rate; otherwise 60.
    pub match_refresh: bool,
    /// The wheel scrolls the other way on the computer.
    pub scroll_reversed: bool,
    /// On a Mac, Ctrl on this keyboard is Command there (and the Windows key is Ctrl).
    pub ctrl_as_command: bool,
    /// The key that acts as the Windows key (Super, Command) besides the Windows key itself.
    pub super_key: Stand,
    /// The pointer is held in the window and its raw movement sent.
    pub capture: bool,
    /// What a newly paired computer's sessions start with.
    pub sound: bool,
    pub clipboard: bool,
    /// Where files from a computer go; empty: the Downloads folder.
    pub folder: String,
    /// Ask for HDR when this monitor is in HDR (and the computer offers it).
    pub hdr: bool,
    /// In full screen on a monitor that can vary its refresh, show each frame as it arrives.
    pub variable_refresh: bool,
    /// How fast this PC's processor decodes 4:4:4, in millions of pixels a second, as last
    /// measured in a session (0: never measured).
    pub cpu_444: u32,
    /// Touchscreen touches go to the computer as touches, not as the pointer (the Linux
    /// viewer's key, `touch`).
    pub touch: bool,
    /// Game controllers go to the computer.
    pub gamepad: bool,
    /// Computers may be lent this PC's microphone and camera; each is still switched on in a
    /// session by the person here.
    pub allow_microphone: bool,
    pub allow_camera: bool,
    /// Lines this viewer doesn't use, kept as they were.
    pub other: Vec<(String, String)>,
}

impl Settings {
    /// The usage to send for a key on this keyboard, for a computer that runs `os`.
    pub fn map_key(&self, usage: u16, os: &str) -> u16 {
        // A stand-in first: it is then the Windows key like the real one, and goes on
        // through whatever the Mac setting does to that.
        let usage = if self.super_key.usage() == Some(usage) { 0xE3 } else { usage };
        let mac = matches!(os.to_ascii_lowercase().as_str(), "macos" | "mac" | "darwin");
        if !(self.ctrl_as_command && mac) {
            return usage;
        }
        match usage {
            0xE0 => 0xE3,
            0xE4 => 0xE7,
            0xE3 => 0xE0,
            0xE7 => 0xE4,
            other => other,
        }
    }
}

/// The picture's own keys (this viewer's alone so far).
const PICTURE_KEYS: [&str; 3] = ["hdr", "variable-refresh", "cpu-444"];

const SETTINGS_KEYS: [&str; 18] = ["game-controllers", "super-key", "capture-pointer", "placement", "hover-bar", "figures", "fullscreen", "shortcuts", "quality", "match-refresh", "scroll-reversed", "ctrl-as-command", "sound", "clipboard", "folder", "touch", "allow-microphone", "allow-camera"];

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
            match_refresh: yes("match-refresh", true),
            scroll_reversed: yes("scroll-reversed", false),
            ctrl_as_command: yes("ctrl-as-command", false),
            super_key: Stand::from_key(&get("super-key").unwrap_or_default()),
            capture: yes("capture-pointer", false),
            sound: yes("sound", false),
            clipboard: yes("clipboard", false),
            folder: get("folder").unwrap_or_default(),
            hdr: yes("hdr", true),
            variable_refresh: yes("variable-refresh", true),
            cpu_444: get("cpu-444").and_then(|v| v.parse().ok()).unwrap_or(0),
            touch: yes("touch", false),
            gamepad: yes("game-controllers", true),
            allow_microphone: yes("allow-microphone", false),
            allow_camera: yes("allow-camera", false),
            other: text.lines().filter_map(|l| l.split_once('=')).filter(|(k, _)| !SETTINGS_KEYS.contains(k) && !PICTURE_KEYS.contains(k)).map(|(k, v)| (k.to_owned(), v.to_owned())).collect(),
        };
        let mut store = Self { dir: dir.to_owned(), computers: Vec::new(), settings, pretend: false };
        if let Ok(text) = std::fs::read_to_string(dir.join("viewer-computers")) {
            for line in text.lines() {
                if line == "[computer]" {
                    store.computers.push(Computer { sound: false, clipboard: false, ..Computer::new("", "", "", 0, &store.settings) });
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
                    "picture-lines" => c.max_height = if value == "window" { WINDOW } else { value.parse().unwrap_or(0) },
                    "sound" => c.sound = value == "yes",
                    "clipboard" => c.clipboard = value == "yes",
                    "second-side" => c.second_side = SIDES.iter().find(|s| s.1 == value).map_or(Side::Right, |s| s.0),
                    "cards" => c.cards = value.to_owned(),
                    "wake" => c.wake = value != "no",
                    "way-in" => c.attach_only = value == "attach",
                    other => c.other.push((other.to_owned(), value.to_owned())),
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

    /// Where files from a computer go: the folder chosen, or Downloads.
    pub fn receive_folder(&self) -> PathBuf {
        if !self.settings.folder.is_empty() {
            return PathBuf::from(&self.settings.folder);
        }
        PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_else(|| ".".into())).join("Downloads")
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
                self.computers.push(Computer::new(name, address, os, now, &self.settings));
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
                "[computer]\nname={}\naddress={}\nos={}\npaired={}\nused={}\nplacement={}\nquality={}\nbitrate={}\npicture-lines={}\nsound={}\nclipboard={}\nsecond-side={}\ncards={}\nwake={}\nway-in={}\n",
                clean(&c.name),
                clean(&c.address),
                clean(&c.os),
                c.paired,
                c.used,
                placement_key(c.placement),
                c.quality.map_or("same", |q| q.key()),
                c.bitrate,
                if c.max_height == WINDOW { "window".to_owned() } else { c.max_height.to_string() },
                if c.sound { "yes" } else { "no" },
                if c.clipboard { "yes" } else { "no" },
                SIDES.iter().find(|s| s.0 == c.second_side).map_or("right", |s| s.1),
                clean(&c.cards),
                if c.wake { "yes" } else { "no" },
                if c.attach_only { "attach" } else { "any" },
            );
            for (key, value) in &c.other {
                out += &format!("{key}={}\n", clean(value));
            }
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
        let mut out = format!(
            "placement={}\nhover-bar={}\nfigures={}\nfullscreen={}\nshortcuts={}\nquality={}\nmatch-refresh={}\nscroll-reversed={}\nctrl-as-command={}\nsuper-key={}\ncapture-pointer={}\nsound={}\nclipboard={}\nfolder={}\ntouch={}\nallow-microphone={}\nallow-camera={}\ngame-controllers={}\n",
            placement_key(s.placement),
            yes(s.hover_bar),
            yes(s.figures),
            yes(s.fullscreen),
            yes(s.shortcuts),
            s.quality.key(),
            yes(s.match_refresh),
            yes(s.scroll_reversed),
            yes(s.ctrl_as_command),
            s.super_key.key(),
            yes(s.capture),
            yes(s.sound),
            yes(s.clipboard),
            s.folder.replace(['\n', '\r'], " "),
            yes(s.touch),
            yes(s.allow_microphone),
            yes(s.allow_camera),
            yes(s.gamepad),
        );
        out += &format!("hdr={}\nvariable-refresh={}\ncpu-444={}\n", yes(s.hdr), yes(s.variable_refresh), s.cpu_444);
        for (key, value) in &s.other {
            out += &format!("{key}={value}\n");
        }
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

/// How fast a processor is taken to decode HEVC 4:4:4 before this PC's has been measured, in
/// millions of pixels a second: about half of what one of the fastest desktop processors did
/// (a Ryzen 9 9950X3D: 228 at 1920x1080, 243 at 2560x1440, 330 at 3840x2160, with the upload,
/// on one core: NVENC's pictures are one slice each, and frame threads would hold pictures
/// back). Enough for 1280x720 at 60, not for 1920x1080 at 60 until measured.
pub const CPU_444_GUESS: u32 = 120;

/// What is kept of a new measurement of the processor's 4:4:4 rate: the lowest seen, since a
/// rate measured on a larger picture (fewer bits a pixel) must not vouch for a smaller one.
pub fn kept_cpu_444(kept: u32, measured: u32) -> u32 {
    if kept == 0 { measured } else { kept.min(measured) }
}

/// Whether 4:4:4 decoded on the processor keeps up with a picture of `width` x `height` at
/// `refresh` a second, for a processor that decodes `rate` million pixels a second (measured,
/// or the guess above): with a fifth of each frame's time left over for the rest of the work.
pub fn cpu_keeps_up(width: u32, height: u32, refresh: u32, rate: u32) -> bool {
    let wanted = width as f64 * height as f64 * refresh.max(1) as f64;
    wanted * 1.25 <= rate as f64 * 1e6
}

#[cfg(test)]
mod tests {
    use super::*;
    use remoteman_core::Chroma;

    fn modes() -> Vec<Mode> {
        vec![Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 }, Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 }, Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 }]
    }

    fn settings_with(super_key: Stand, ctrl_as_command: bool) -> Settings {
        let mut s = Store::load(&std::env::temp_dir().join("remoteman-no-such-settings")).settings;
        s.super_key = super_key;
        s.ctrl_as_command = ctrl_as_command;
        s
    }

    #[test]
    fn keys_go_as_they_are_by_default() {
        let s = settings_with(Stand::Own, false);
        for os in ["linux", "macos", "windows"] {
            for usage in [0xE0, 0xE3, 0xE4, 0xE6, 0xE7, 0x39, 0x04] {
                assert_eq!(s.map_key(usage, os), usage);
            }
        }
    }

    #[test]
    fn ctrl_acts_as_command_on_a_mac_only() {
        let s = settings_with(Stand::Own, true);
        // Ctrl and the Windows key swap, each side with its own.
        assert_eq!((s.map_key(0xE0, "macos"), s.map_key(0xE4, "macos")), (0xE3, 0xE7));
        assert_eq!((s.map_key(0xE3, "macos"), s.map_key(0xE7, "Darwin")), (0xE0, 0xE4));
        assert_eq!(s.map_key(0xE6, "macos"), 0xE6);
        for os in ["linux", "windows", ""] {
            assert_eq!((s.map_key(0xE0, os), s.map_key(0xE3, os)), (0xE0, 0xE3));
        }
    }

    #[test]
    fn another_key_stands_in_for_the_windows_key() {
        for (stand, usage) in [(Stand::RightAlt, 0xE6), (Stand::RightCtrl, 0xE4), (Stand::CapsLock, 0x39)] {
            let s = settings_with(stand, false);
            // It is the left Windows key (Super, Command) everywhere, and the real one still is.
            for os in ["linux", "macos", "windows"] {
                assert_eq!(s.map_key(usage, os), 0xE3);
                assert_eq!(s.map_key(0xE3, os), 0xE3);
                assert_eq!(s.map_key(0x04, os), 0x04);
            }
        }
        // The others aren't touched.
        let s = settings_with(Stand::CapsLock, false);
        assert_eq!((s.map_key(0xE6, "linux"), s.map_key(0xE4, "linux")), (0xE6, 0xE4));
    }

    #[test]
    fn a_stand_in_goes_through_the_mac_swap_too() {
        // Caps Lock as the Windows key, Ctrl as Command: on a Mac Caps Lock is Ctrl, as in the Linux viewer.
        let s = settings_with(Stand::CapsLock, true);
        assert_eq!(s.map_key(0x39, "macos"), 0xE0);
        assert_eq!(s.map_key(0x39, "linux"), 0xE3);
        // Right Ctrl as the Windows key: it is no Ctrl any more, and so no Command.
        let s = settings_with(Stand::RightCtrl, true);
        assert_eq!(s.map_key(0xE4, "macos"), 0xE0);
        assert_eq!(s.map_key(0xE0, "macos"), 0xE3);
    }

    #[test]
    fn the_stand_in_is_kept_in_the_settings_file() {
        let dir = std::env::temp_dir().join(format!("remoteman-stand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::load(&dir);
        assert_eq!(store.settings.super_key, Stand::Own);
        store.settings.super_key = Stand::RightAlt;
        store.save_settings();
        let text = std::fs::read_to_string(dir.join("viewer-settings")).unwrap();
        assert!(text.contains("super-key=right-alt\n"));
        assert_eq!(Store::load(&dir).settings.super_key, Stand::RightAlt);
        // A word nobody knows is the Windows key alone.
        std::fs::write(dir.join("viewer-settings"), "super-key=f13\n").unwrap();
        assert_eq!(Store::load(&dir).settings.super_key, Stand::Own);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_way_in_is_kept_per_computer_as_the_linux_viewer_keeps_it() {
        let dir = std::env::temp_dir().join(format!("remoteman-wayin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("viewer-computers"), "[computer]\nname=a\naddress=a:7791\nway-in=attach\n[computer]\nname=b\naddress=b:7791\nway-in=any\n[computer]\nname=c\naddress=c:7791\n").unwrap();
        let mut store = Store::load(&dir);
        let by = |s: &Store, n: &str| s.computers.iter().find(|c| c.name == n).map(|c| c.attach_only);
        assert_eq!((by(&store, "a"), by(&store, "b"), by(&store, "c")), (Some(true), Some(false), Some(false)));
        store.update("b:7791", |c| c.attach_only = true);
        let again = Store::load(&dir);
        assert_eq!((by(&again, "a"), by(&again, "b"), by(&again, "c")), (Some(true), Some(true), Some(false)));
        // It is a key of its own, not one of the lines kept as they were.
        assert!(again.computers.iter().all(|c| c.other.iter().all(|(k, _)| k != "way-in")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_processor_keeps_up_where_its_rate_allows() {
        // The guess: 720p60, not 1080p60.
        assert!(cpu_keeps_up(1280, 720, 60, CPU_444_GUESS));
        assert!(!cpu_keeps_up(1920, 1080, 60, CPU_444_GUESS));
        // This PC's processor as measured at 1080p (228): 1080p60, not 1080p120, not 1440p60
        // with the fifth to spare, never 4K, nor its monitors' 240 Hz.
        assert!(cpu_keeps_up(1920, 1080, 60, 228));
        assert!(!cpu_keeps_up(1920, 1080, 120, 228));
        assert!(!cpu_keeps_up(2560, 1440, 60, 228));
        assert!(!cpu_keeps_up(3840, 2160, 60, 228));
        assert!(!cpu_keeps_up(1920, 1080, 240, 228));
        // No rate: nothing.
        assert!(!cpu_keeps_up(640, 480, 30, 0));
    }

    #[test]
    fn a_rate_measured_on_a_larger_picture_does_not_vouch_for_a_smaller_one() {
        // 4K measured first (330 a microsecond's worth), then 1080p (228): 228 is kept, and a
        // 1080p at 120 (which measured about 105 a second) isn't offered.
        let kept = kept_cpu_444(kept_cpu_444(0, 330), 228);
        assert_eq!(kept, 228);
        assert!(!cpu_keeps_up(1920, 1080, 120, kept));
        assert_eq!(kept_cpu_444(228, 330), 228);
    }

    #[test]
    fn sharp_text_puts_four_four_four_first_and_automatic_last() {
        let sharp = Quality::Sharp.order(&modes());
        assert_eq!(sharp[0].chroma, Chroma::C444);
        let auto = Quality::Automatic.order(&modes());
        assert_eq!(auto.last().unwrap().chroma, Chroma::C444);
        let hevc = Quality::Hevc.order(&modes());
        assert_eq!((hevc[0].codec, hevc[0].chroma), (Codec::Hevc, Chroma::C420));
    }

    #[test]
    fn the_picture_settings_are_kept_and_read_back() {
        let dir = std::env::temp_dir().join(format!("remoteman-store-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A file from before these keys: HDR and variable refresh start on, the rate unknown,
        // and a line of the Linux viewer's is kept.
        std::fs::write(dir.join("viewer-settings"), "figures=yes\npicture-lines=1080\n").unwrap();
        let mut store = Store::load(&dir);
        assert!(store.settings.hdr && store.settings.variable_refresh);
        assert_eq!(store.settings.cpu_444, 0);
        store.settings.hdr = false;
        store.settings.variable_refresh = false;
        store.settings.cpu_444 = 287;
        store.save_settings();
        let again = Store::load(&dir);
        assert!(!again.settings.hdr && !again.settings.variable_refresh);
        assert_eq!(again.settings.cpu_444, 287);
        assert!(again.settings.figures);
        assert_eq!(again.settings.other, vec![("picture-lines".to_owned(), "1080".to_owned())]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
