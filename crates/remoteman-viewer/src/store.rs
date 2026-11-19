//! What the viewer remembers, in the state folder: the paired computers (`viewer-computers`)
//! and the preferences (`viewer-settings`). Both are plain `key=value` lines.
//!
//! The keys themselves are the core's: `client/hosts/<address>` holds the fingerprint of the
//! computer at that address, and a computer without that file isn't paired as far as the core
//! is concerned. So forgetting a computer deletes its file, and a new address moves it.

use std::path::{Path, PathBuf};

use gtk::glib;
use remoteman_core::{Chroma, Mode, Placement, Side};

use crate::PLACEMENTS;

/// Which stream to ask for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    /// The decoders in their usual order.
    Automatic,
    /// Full colour detail (4:4:4) first, which keeps small text sharp.
    Sharp,
}

impl Quality {
    fn key(self) -> &'static str {
        match self {
            Quality::Automatic => "auto",
            Quality::Sharp => "sharp",
        }
    }

    fn from_key(key: &str) -> Self {
        if key == "sharp" { Quality::Sharp } else { Quality::Automatic }
    }

    /// The decoders to offer, most wanted first.
    pub fn order(self, decoders: &[Mode]) -> Vec<Mode> {
        let mut out = decoders.to_vec();
        if self == Quality::Sharp {
            // A stable sort: the 4:4:4 ones first, the rest as they were.
            out.sort_by_key(|m| m.chroma != Chroma::C444);
        }
        out
    }
}

fn placement_key(p: Placement) -> &'static str {
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
    pub quality: Quality,
    /// What a session with this computer starts with.
    pub media: Media,
    /// Only into a session that is already running there: never its login screen.
    pub attach_only: bool,
    /// Which side of the computer's other displays a second display here is put on.
    pub second_side: Side,
    /// The most lines of picture to ask this computer for (0: this screen's own). Fewer
    /// pixels are encoded sooner: a computer whose encoder can't keep up with this screen at
    /// its full size is quicker with a smaller picture, which is scaled up here.
    pub max_height: u32,
    /// The hardware addresses of its network cards, as it last advertised them on this
    /// network (comma-separated; empty: never seen): what waking it needs.
    pub cards: String,
}

/// What a session starts with switched on: the computer's sound played here, this device's
/// microphone and camera lent to it, one clipboard for both.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Media {
    pub sound: bool,
    pub microphone: bool,
    pub camera: bool,
    pub clipboard: bool,
}

impl Media {
    const KEYS: [&str; 4] = ["sound", "microphone", "camera", "clipboard"];

    fn field(&mut self, key: &str) -> Option<&mut bool> {
        match key {
            "sound" => Some(&mut self.sound),
            "microphone" => Some(&mut self.microphone),
            "camera" => Some(&mut self.camera),
            "clipboard" => Some(&mut self.clipboard),
            _ => None,
        }
    }

    fn lines(mut self) -> String {
        Self::KEYS.iter().map(|k| format!("{k}={}\n", if *self.field(k).unwrap() { "yes" } else { "no" })).collect()
    }
}

/// A key on this keyboard that can stand in for another.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Stand {
    /// The key itself.
    Own,
    RightAlt,
    RightCtrl,
    CapsLock,
}

impl Stand {
    pub const ALL: [(Stand, &str, &str); 4] = [(Stand::Own, "own", ""), (Stand::RightAlt, "right-alt", "Right Alt"), (Stand::RightCtrl, "right-ctrl", "Right Ctrl"), (Stand::CapsLock, "caps-lock", "Caps Lock")];

    fn from_key(key: &str) -> Self {
        Self::ALL.iter().find(|s| s.1 == key).map_or(Stand::Own, |s| s.0)
    }

    fn key(self) -> &'static str {
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

/// How keys, the pointer and touches are sent.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Input {
    /// The key that acts as Super on the computer, and the one that acts as Ctrl.
    pub super_key: Stand,
    pub ctrl_key: Stand,
    /// On a Mac, Ctrl on this keyboard is Command there (and Super is Ctrl), so Ctrl+C copies.
    pub ctrl_as_command: bool,
    /// The wheel and the touchpad scroll the other way on the computer.
    pub scroll_reversed: bool,
    /// The pointer is this window's own, wearing the computer's cursor; otherwise the
    /// computer draws it into the picture.
    pub draw_pointer: bool,
    /// The pointer is held in the window and its raw movement sent.
    pub capture: bool,
    /// Touchscreen touches are sent as touches, not as a pointer.
    pub touch: bool,
}

impl Default for Input {
    fn default() -> Self {
        Self { super_key: Stand::Own, ctrl_key: Stand::Own, ctrl_as_command: false, scroll_reversed: false, draw_pointer: true, capture: false, touch: false }
    }
}

impl Input {
    /// The usage to send for a key on this keyboard, for a computer that runs `os`.
    pub fn map(&self, usage: u16, os: &str) -> u16 {
        const LEFT_CTRL: u16 = 0xE0;
        const LEFT_SUPER: u16 = 0xE3;
        const RIGHT_CTRL: u16 = 0xE4;
        const RIGHT_SUPER: u16 = 0xE7;
        let mut usage = usage;
        // A stand-in first: it is then Super or Ctrl like the real one.
        if self.super_key.usage() == Some(usage) {
            usage = LEFT_SUPER;
        } else if self.ctrl_key.usage() == Some(usage) {
            usage = LEFT_CTRL;
        }
        let mac = matches!(os.to_ascii_lowercase().as_str(), "macos" | "mac" | "darwin");
        if self.ctrl_as_command && mac {
            usage = match usage {
                LEFT_CTRL => LEFT_SUPER,
                RIGHT_CTRL => RIGHT_SUPER,
                LEFT_SUPER => LEFT_CTRL,
                RIGHT_SUPER => RIGHT_CTRL,
                other => other,
            };
        }
        usage
    }
}

pub struct Settings {
    /// The placement last used, for a computer that isn't in the list.
    pub placement: Placement,
    /// No title bar while connected; it comes out at the top edge.
    pub hover_bar: bool,
    /// The figures over the picture.
    pub figures: bool,
    /// Go to full screen when a session starts.
    pub fullscreen: bool,
    /// Super, Alt+Tab and the like go to the other computer.
    pub shortcuts: bool,
    /// The quality a newly paired computer starts with.
    pub quality: Quality,
    /// What a newly paired computer's sessions start with.
    pub media: Media,
    pub input: Input,
    /// Ask for this display's own refresh rate (up to 120); otherwise 60.
    pub match_refresh: bool,
    /// Where files from a computer go; empty: the Downloads folder.
    pub folder: String,
}

pub struct Store {
    dir: PathBuf,
    pub computers: Vec<Computer>,
    pub settings: Settings,
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

fn now() -> i64 {
    glib::DateTime::now_local().map_or(0, |d| d.to_unix())
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
            folder: get("folder").unwrap_or_default(),
            media: Media { sound: yes("sound", false), microphone: yes("microphone", false), camera: yes("camera", false), clipboard: yes("clipboard", false) },
            input: Input {
                super_key: Stand::from_key(&get("super-key").unwrap_or_default()),
                ctrl_key: Stand::from_key(&get("ctrl-key").unwrap_or_default()),
                ctrl_as_command: yes("ctrl-as-command", false),
                scroll_reversed: yes("scroll-reversed", false),
                draw_pointer: yes("draw-pointer", true),
                capture: yes("capture-pointer", false),
                touch: yes("touch", false),
            },
        };
        let mut store = Self { dir: dir.to_owned(), computers: Vec::new(), settings };
        match std::fs::read_to_string(dir.join("viewer-computers")) {
            Ok(text) => {
                for line in text.lines() {
                    if line == "[computer]" {
                        store.computers.push(Computer { name: String::new(), address: String::new(), os: String::new(), paired: 0, used: 0, placement: Placement::Extend, quality: Quality::Automatic, media: Media::default(), cards: String::new(), attach_only: false, second_side: Side::Right, max_height: 0 });
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
                        "quality" => c.quality = Quality::from_key(value),
                        "cards" => c.cards = value.to_owned(),
                        "way-in" => c.attach_only = value == "attach",
                        "picture-lines" => c.max_height = if value == "window" { WINDOW } else { value.parse().unwrap_or(0) },
                        "second-side" => c.second_side = SIDES.iter().find(|(_, key)| *key == value).map_or(Side::Right, |(side, _)| *side),
                        other => {
                            if let Some(field) = c.media.field(other) {
                                *field = value == "yes";
                            }
                        }
                    }
                }
                store.computers.retain(|c| !c.address.is_empty());
                store.sort();
            }
            Err(_) => {
                // From before there was a list: the one address the first page remembered.
                let last = std::fs::read_to_string(dir.join("last-host")).unwrap_or_default();
                let last = last.trim();
                if !last.is_empty() && store.known(last).exists() {
                    let paired = std::fs::metadata(store.known(last)).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
                    store.computers.push(Computer { name: host_part(last).to_owned(), address: last.to_owned(), os: String::new(), paired, used: paired, placement: store.settings.placement, quality: store.settings.quality, media: store.settings.media, cards: String::new(), attach_only: false, second_side: Side::Right, max_height: 0 });
                }
            }
        }
        store
    }

    /// Where files from a computer go: the folder chosen, or Downloads.
    pub fn receive_folder(&self) -> PathBuf {
        if !self.settings.folder.is_empty() {
            return PathBuf::from(&self.settings.folder);
        }
        glib::user_special_dir(glib::UserDirectory::Downloads).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Downloads"))
    }

    /// Where the core keeps the fingerprint of the computer at this address.
    fn known(&self, address: &str) -> PathBuf {
        self.dir.join("client/hosts").join(address.replace([':', '/'], "_"))
    }

    /// The computer's key fingerprint, if this device is paired with it.
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
                // One that came from before there was a list has only its address for a name.
                if c.name == host_part(address) && !host_name.is_empty() {
                    c.name = host_name.to_owned();
                }
            }
            None => {
                let name = if host_name.is_empty() { host_part(address) } else { host_name };
                self.computers.push(Computer { name: name.to_owned(), address: address.to_owned(), os: os.to_owned(), paired: now, used: now, placement: Placement::Extend, quality: self.settings.quality, media: self.settings.media, cards: String::new(), attach_only: false, second_side: Side::Right, max_height: 0 });
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
        let _ = std::fs::remove_file(self.known(address));
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
        if from.exists() {
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
        let mut out = String::new();
        // One value a line: anything that would break that is left out of a name.
        let clean = |s: &str| s.replace(['\n', '\r'], " ");
        for c in &self.computers {
            out += &format!(
                "[computer]\nname={}\naddress={}\nos={}\npaired={}\nused={}\nplacement={}\nquality={}\ncards={}\nway-in={}\nsecond-side={}\npicture-lines={}\n{}",
                clean(&c.name),
                clean(&c.address),
                clean(&c.os),
                c.paired,
                c.used,
                placement_key(c.placement),
                c.quality.key(),
                clean(&c.cards),
                if c.attach_only { "attach" } else { "any" },
                SIDES.iter().find(|(side, _)| *side == c.second_side).map_or("right", |(_, key)| key),
                if c.max_height == WINDOW { "window".to_owned() } else { c.max_height.to_string() },
                c.media.lines()
            );
        }
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(self.dir.join("viewer-computers"), out);
    }

    pub fn save_settings(&self) {
        let s = &self.settings;
        let yes = |b: bool| if b { "yes" } else { "no" };
        let out = format!(
            "placement={}\nhover-bar={}\nfigures={}\nfullscreen={}\nshortcuts={}\nquality={}\n{}super-key={}\nctrl-key={}\nctrl-as-command={}\nscroll-reversed={}\ndraw-pointer={}\ncapture-pointer={}\ntouch={}\nmatch-refresh={}\nfolder={}\n",
            placement_key(s.placement),
            yes(s.hover_bar),
            yes(s.figures),
            yes(s.fullscreen),
            yes(s.shortcuts),
            s.quality.key(),
            s.media.lines(),
            s.input.super_key.key(),
            s.input.ctrl_key.key(),
            yes(s.input.ctrl_as_command),
            yes(s.input.scroll_reversed),
            yes(s.input.draw_pointer),
            yes(s.input.capture),
            yes(s.input.touch),
            yes(s.match_refresh),
            s.folder.replace(['\n', '\r'], " ")
        );
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(self.dir.join("viewer-settings"), out);
    }
}

/// What a computer says it runs, as a name: `linux` is Linux.
pub fn os_name(os: &str) -> String {
    match os.to_ascii_lowercase().as_str() {
        "" => String::new(),
        "linux" => "Linux".into(),
        "macos" | "mac" | "darwin" => "macOS".into(),
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

/// A moment as people say it, to follow "Used" or "Paired": `today at 09:12`, `yesterday`,
/// `Friday`, `2 November`.
pub fn when(unix: i64) -> String {
    let (Ok(then), Ok(now)) = (glib::DateTime::from_unix_local(unix), glib::DateTime::now_local()) else { return String::new() };
    if now.to_unix() - unix < 120 {
        return "just now".into();
    }
    // Whole days since 1970, by this place's clock.
    let day = |d: &glib::DateTime| (d.to_unix() + d.utc_offset().as_seconds()).div_euclid(86400);
    let text = |d: &glib::DateTime, f: &str| d.format(f).map(|s| s.to_string()).unwrap_or_default();
    match day(&now) - day(&then) {
        0 => format!("today at {}", text(&then, "%H:%M")),
        1 => "yesterday".into(),
        2..=6 => text(&then, "%A"),
        _ if then.year() == now.year() => format!("{} {}", then.day_of_month(), text(&then, "%B")),
        _ => format!("{} {}", then.day_of_month(), text(&then, "%B %Y")),
    }
}

/// The picture sizes a computer can be asked for, in the order they are offered: the most
/// lines (0: this screen's own) and what the choice is called.
pub const SIZES: [(u32, &str); 4] = [(0, "This screen's own"), (WINDOW, "The window's size"), (1440, "Up to 1440 lines"), (1080, "Up to 1080 lines")];

/// In place of a number of lines: a display of exactly the pixels the picture has in the
/// viewer's window, asked for again when the window has changed size ("window" in the file).
pub const WINDOW: u32 = 1;

/// A screen of `width` by `height` pixels drawn at `scale`, as it is asked for when the
/// picture may have `most` lines at most (0: no limit): the same shape and the same desktop
/// (what was one point is still one point), with fewer pixels.
pub fn fit_lines(width: u32, height: u32, scale: f32, most: u32) -> (u32, u32, f32) {
    if most == 0 || most == WINDOW || height <= most {
        return (width, height, scale);
    }
    let by = most as f64 / height as f64;
    (((width as f64 * by).round() as u32) & !1, most & !1, (scale as f64 * by) as f32)
}

/// The sides a second display can sit on, in the order they are offered, with their names in
/// the file.
pub const SIDES: [(Side, &str); 4] = [(Side::Right, "right"), (Side::Left, "left"), (Side::Above, "above"), (Side::Below, "below")];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_stand_in_and_swap_for_a_mac() {
        let plain = Input::default();
        assert_eq!(plain.map(0xE0, "macos"), 0xE0);
        assert_eq!(plain.map(0x39, "linux"), 0x39);
        let caps = Input { super_key: Stand::CapsLock, ctrl_key: Stand::RightAlt, ..Input::default() };
        assert_eq!(caps.map(0x39, "linux"), 0xE3);
        assert_eq!(caps.map(0xE6, "linux"), 0xE0);
        assert_eq!(caps.map(0x04, "linux"), 0x04);
        let mac = Input { ctrl_as_command: true, ..Input::default() };
        assert_eq!(mac.map(0xE0, "macOS"), 0xE3);
        assert_eq!(mac.map(0xE4, "macos"), 0xE7);
        assert_eq!(mac.map(0xE3, "macos"), 0xE0);
        assert_eq!(mac.map(0xE0, "linux"), 0xE0);
        // A stand-in for Ctrl is Command on a Mac too.
        let both = Input { ctrl_key: Stand::CapsLock, ctrl_as_command: true, ..Input::default() };
        assert_eq!(both.map(0x39, "macos"), 0xE3);
    }

    #[test]
    fn choices_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("remoteman-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::load(&dir);
        assert_eq!((store.settings.media, store.settings.input), (Media::default(), Input::default()));
        store.settings.media = Media { sound: true, microphone: false, camera: false, clipboard: true };
        store.settings.input = Input { super_key: Stand::CapsLock, ctrl_key: Stand::RightCtrl, ctrl_as_command: true, scroll_reversed: true, draw_pointer: false, capture: true, touch: true };
        store.save_settings();
        // A computer paired from here on starts with the defaults of that moment.
        store.used("desk:7791", "desk", "linux");
        store.update("desk:7791", |c| (c.media.camera, c.second_side, c.max_height) = (true, Side::Above, 1440));
        let again = Store::load(&dir);
        assert_eq!(again.settings.media, store.settings.media);
        assert_eq!(again.settings.input, store.settings.input);
        assert_eq!(again.find("desk:7791").unwrap().media, Media { sound: true, microphone: false, camera: true, clipboard: true });
        assert_eq!(again.find("desk:7791").unwrap().second_side, Side::Above);
        assert_eq!(again.find("desk:7791").unwrap().max_height, 1440);
        assert_eq!(fit_lines(3840, 2160, 1.25, 1440), (2560, 1440, 1.25 * 1440.0 / 2160.0));
        assert_eq!(fit_lines(1920, 1080, 1.0, 1440), (1920, 1080, 1.0));
        assert_eq!(fit_lines(3840, 2160, 2.0, 0), (3840, 2160, 2.0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
