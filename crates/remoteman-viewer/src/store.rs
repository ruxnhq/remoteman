//! What the viewer remembers, in the state folder: the paired computers (`viewer-computers`)
//! and the preferences (`viewer-settings`). Both are plain `key=value` lines.
//!
//! The keys themselves are the core's: `client/hosts/<address>` holds the fingerprint of the
//! computer at that address, and a computer without that file isn't paired as far as the core
//! is concerned. So forgetting a computer deletes its file, and a new address moves it.

use std::path::{Path, PathBuf};

use gtk::glib;
use remoteman_core::{Chroma, Mode, Placement};

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
        };
        let mut store = Self { dir: dir.to_owned(), computers: Vec::new(), settings };
        match std::fs::read_to_string(dir.join("viewer-computers")) {
            Ok(text) => {
                for line in text.lines() {
                    if line == "[computer]" {
                        store.computers.push(Computer { name: String::new(), address: String::new(), os: String::new(), paired: 0, used: 0, placement: Placement::Extend, quality: Quality::Automatic });
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
                        _ => {}
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
                    store.computers.push(Computer { name: host_part(last).to_owned(), address: last.to_owned(), os: String::new(), paired, used: paired, placement: store.settings.placement, quality: store.settings.quality });
                }
            }
        }
        store
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
                self.computers.push(Computer { name: name.to_owned(), address: address.to_owned(), os: os.to_owned(), paired: now, used: now, placement: Placement::Extend, quality: self.settings.quality });
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
                "[computer]\nname={}\naddress={}\nos={}\npaired={}\nused={}\nplacement={}\nquality={}\n",
                clean(&c.name),
                clean(&c.address),
                clean(&c.os),
                c.paired,
                c.used,
                placement_key(c.placement),
                c.quality.key()
            );
        }
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(self.dir.join("viewer-computers"), out);
    }

    pub fn save_settings(&self) {
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
