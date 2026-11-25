//! Updates, the part that needs no Windows: where the newest release is read from, how versions
//! compare, the `.sha256` beside each file, which downloads to delete, and which of the host's
//! options an install of the new version can be given again. `updater.rs` does the rest: asking,
//! downloading, running the installer. Nothing here uses more than std, so its tests run on any
//! system, without the Windows crates:
//!
//!     rustc --edition 2024 --test src/update.rs -o update-tests && ./update-tests

use std::cmp::Ordering;
use std::fmt;

/// The page remoteman's downloads are on (built from `site/public/index.html`). There is no
/// list of releases to read (no `latest.json`), so the newest host is the installer this page
/// links: see [`newest_on_page`].
pub const PAGE: &str = "https://remoteman.ruxn.com/";

/// Where the files are, each with `NAME.sha256` beside it.
pub const FILES: &str = "https://files.ruxn.com/p/remoteman/";

/// The start of the host installer's file name, `remoteman-host-setup-X.Y.Z.exe`.
pub const PREFIX: &str = "remoteman-host-setup-";

/// Whether the host looks for updates by itself (`--updates`): never, to say when there is one,
/// or to install it too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    Check,
    Install,
}

impl Mode {
    pub fn parse(word: &str) -> Option<Mode> {
        match word {
            "off" => Some(Mode::Off),
            "check" => Some(Mode::Check),
            "install" => Some(Mode::Install),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Check => "check",
            Mode::Install => "install",
        }
    }
}

/// A version as remoteman numbers its releases: numbers between dots, compared as numbers
/// (0.1.10 is after 0.1.9), with missing ones as 0 (0.2 is 0.2.0).
#[derive(Clone, Debug)]
pub struct Version(Vec<u64>);

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let parts: Vec<&str> = text.split('.').collect();
        if parts.is_empty() || parts.len() > 4 || parts.iter().any(|p| p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit())) {
            return None;
        }
        Some(Version(parts.iter().map(|p| p.parse().unwrap_or(0)).collect()))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.0.len().max(other.0.len());
        let at = |v: &Version, i: usize| v.0.get(i).copied().unwrap_or(0);
        (0..n).map(|i| at(self, i).cmp(&at(other, i))).find(|o| o.is_ne()).unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let words: Vec<String> = self.0.iter().map(u64::to_string).collect();
        f.write_str(&words.join("."))
    }
}

/// A host installer that can be downloaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// `remoteman-host-setup-X.Y.Z.exe`.
    pub file: String,
    pub url: String,
}

impl Release {
    pub fn of(version: Version) -> Release {
        let file = format!("{PREFIX}{version}.exe");
        Release { url: format!("{FILES}{file}"), file, version }
    }

    /// Its SHA-256, in a file of its own beside it.
    pub fn sha256_url(&self) -> String {
        format!("{}.sha256", self.url)
    }
}

/// The newest host installer that remoteman's download page links: every
/// `https://files.ruxn.com/p/remoteman/remoteman-host-setup-X.Y.Z.exe` on it, the highest X.Y.Z.
/// This is the one place that knows where releases are listed; a manifest, once there is one,
/// is read here instead.
pub fn newest_on_page(html: &str) -> Result<Release, String> {
    let link = format!("{FILES}{PREFIX}");
    let newest = html
        .match_indices(&link)
        .filter_map(|(at, _)| {
            let rest = &html[at + link.len()..];
            rest.find(".exe").and_then(|end| Version::parse(&rest[..end]))
        })
        .max();
    newest
        .map(Release::of)
        .ok_or_else(|| format!("{PAGE} links no {PREFIX}X.Y.Z.exe in {FILES}, so the newest version can't be told (has the page changed?)"))
}

/// The SHA-256 a `.sha256` file gives for `file`, in lower-case hex. The file is one line,
/// `<64 hex digits>  <file name>` (as `sha256sum` writes it; the name may be left out).
pub fn sha256_of(text: &str, file: &str) -> Result<String, String> {
    let line = text.trim_start_matches('\u{feff}').lines().map(str::trim).find(|l| !l.is_empty()).ok_or("the .sha256 file is empty")?;
    let mut words = line.split_whitespace();
    let hash = words.next().unwrap_or_default();
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("the .sha256 file doesn't start with a SHA-256 (64 hex digits): {}", line.chars().take(80).collect::<String>()));
    }
    // `sha256sum -b` marks the name with a star.
    if let Some(name) = words.next().map(|n| n.trim_start_matches('*'))
        && name != file
    {
        return Err(format!("the .sha256 file is for {name}, not {file}"));
    }
    Ok(hash.to_ascii_lowercase())
}

/// What the updates folder holds that can go: unfinished downloads, installers of this version
/// or older ones, and, when an installer has just been downloaded (`keep`), every other.
pub fn stale(names: &[String], current: &Version, keep: Option<&str>) -> Vec<String> {
    names
        .iter()
        .filter(|name| {
            let Some(rest) = name.strip_prefix(PREFIX) else { return false };
            if rest.ends_with(".part") {
                return true;
            }
            let Some(version) = rest.strip_suffix(".exe").and_then(Version::parse) else { return false };
            version <= *current || keep.is_some_and(|k| k != name.as_str())
        })
        .cloned()
        .collect()
}

/// What an install of the new version is given, so that its service starts the host as now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Carried {
    /// The installer's own options: `--encoder NAME`, `--no-pair`.
    pub installer: Vec<String>,
    /// `--updates`, which the installer can't be given: kept with the host's settings instead.
    pub updates: Option<String>,
}

/// The host's options as the service started it (its own command line, `state` its state
/// folder), turned into what the installer takes. The installer writes the service's command
/// line anew from its own options (windows/host-installer/setup.ps1): it always writes `--state`
/// and `--log` (as `state` and `state\host.log`), `--ask` when Remoteman Host isn't installed and
/// there is no `--no-pair`, and the service adds `--stop-event`; it takes `--encoder` and
/// `--no-pair`, and nothing else. Any other option, with its values, would be lost by an update,
/// so they are the error.
pub fn carry(host_args: &[String], state: &str) -> Result<Carried, String> {
    let same = |a: &str, b: &str| a.trim_end_matches('\\').eq_ignore_ascii_case(b.trim_end_matches('\\'));
    let log = format!("{}\\host.log", state.trim_end_matches('\\'));
    let mut carried = Carried::default();
    let mut lost: Vec<String> = Vec::new();
    let mut args = host_args.iter().peekable();
    while let Some(arg) = args.next() {
        let mut value = || args.next_if(|v| !v.starts_with("--")).cloned();
        match arg.as_str() {
            "--state" => match value() {
                Some(v) if same(&v, state) => {}
                v => lost.push(format!("--state {}", v.unwrap_or_default())),
            },
            "--log" => match value() {
                Some(v) if same(&v, &log) => {}
                v => lost.push(format!("--log {}", v.unwrap_or_default())),
            },
            "--ask" => {}
            "--stop-event" => {
                value();
            }
            "--no-pair" => carried.installer.push("--no-pair".into()),
            "--encoder" => match value() {
                Some(v) => carried.installer.extend(["--encoder".to_string(), v]),
                None => lost.push("--encoder".into()),
            },
            "--updates" => carried.updates = value(),
            other => {
                let mut said = vec![other.to_string()];
                while let Some(v) = args.next_if(|v| !v.starts_with("--")) {
                    said.push(v.clone());
                }
                lost.push(said.join(" "));
            }
        }
    }
    if lost.is_empty() {
        Ok(carried)
    } else {
        let them = if lost.len() == 1 { "it" } else { "them" };
        Err(format!(
            "the service starts the host with {}, which remoteman's installer can't be given (it takes --encoder and --no-pair only), so an update would lose {them}: install the update by hand, then put {them} back on the service's command line",
            lost.join(", ")
        ))
    }
}

/// Where the looking for updates stands, as the log and the owner's app say it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Not looked yet since the host started.
    NotYet,
    Checking,
    UpToDate,
    Available(Version),
    Downloading(Version),
    /// The installer runs; the host is stopped and started again by it.
    Installing(Version),
    /// This host was installed by an update from that version (said once, after the update).
    Updated(Version),
    Failed(String),
}

impl State {
    /// One word for the app to go by.
    pub fn key(&self) -> &'static str {
        match self {
            State::NotYet => "not-yet",
            State::Checking => "checking",
            State::UpToDate => "up-to-date",
            State::Available(_) => "available",
            State::Downloading(_) => "downloading",
            State::Installing(_) => "installing",
            State::Updated(_) => "updated",
            State::Failed(_) => "failed",
        }
    }

    pub fn words(&self) -> String {
        match self {
            State::NotYet => "not looked for yet".into(),
            State::Checking => "looking for updates".into(),
            State::UpToDate => "up to date".into(),
            State::Available(v) => format!("{v} available"),
            State::Downloading(v) => format!("downloading {v}"),
            State::Installing(v) => format!("installing {v}"),
            State::Updated(from) => format!("updated from {from}"),
            State::Failed(why) => format!("failed: {why}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(v("0.1.10") > v("0.1.9"));
        assert!(v("0.2.0") > v("0.1.99"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v("0.2"), v("0.2.0"));
        assert_eq!(v("0.1.7").to_string(), "0.1.7");
        for bad in ["", "0..1", "0.1.x", "v0.1.7", "0.1.7-beta", " 0.1.7", "1.2.3.4.5", "0.1.1234567890"] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
        // This host's own version is one (when built by Cargo).
        if let Some(own) = option_env!("CARGO_PKG_VERSION") {
            assert!(Version::parse(own).is_some(), "{own}");
        }
    }

    #[test]
    fn the_newest_installer_is_read_from_the_page() {
        let page = r#"<a href="https://files.ruxn.com/p/remoteman/remoteman-host-setup-0.1.9.exe">Host</a>
            <a href="https://files.ruxn.com/p/remoteman/remoteman-viewer-setup-0.1.12.exe">Viewer</a>
            <a href="https://files.ruxn.com/p/remoteman/remoteman-host-setup-0.1.10.exe.sha256">host</a>
            <a href="https://files.ruxn.com/p/remoteman/remoteman-host-0.1.11-x86_64.AppImage">Linux</a>
            <a href="https://example.com/p/remoteman/remoteman-host-setup-9.9.9.exe">elsewhere</a>"#;
        let r = newest_on_page(page).unwrap();
        assert_eq!(r.version, v("0.1.10"));
        assert_eq!(r.file, "remoteman-host-setup-0.1.10.exe");
        assert_eq!(r.url, "https://files.ruxn.com/p/remoteman/remoteman-host-setup-0.1.10.exe");
        assert_eq!(r.sha256_url(), "https://files.ruxn.com/p/remoteman/remoteman-host-setup-0.1.10.exe.sha256");
        let none = newest_on_page("<html>moved</html>").unwrap_err();
        assert!(none.contains("https://remoteman.ruxn.com/") && none.contains("has the page changed"), "{none}");
        assert!(newest_on_page("https://files.ruxn.com/p/remoteman/remoteman-host-setup-latest.exe").is_err());
    }

    /// The page as the repository has it now: whatever it says, it must still be read.
    #[test]
    fn the_sites_own_page_is_read() {
        let page = include_str!("../../../site/public/index.html");
        let r = newest_on_page(page).unwrap();
        assert!(page.contains(&format!("href=\"{}\"", r.url)), "{}", r.url);
        assert!(page.contains(&r.sha256_url()));
    }

    #[test]
    fn sha256_files_are_read() {
        let hash = "9F86D081884C7D659A2FEAA0C55AD015A3BF4F1B2B0B822CD15D6C15B0F00A08";
        let file = "remoteman-host-setup-0.1.8.exe";
        assert_eq!(sha256_of(&format!("{hash}  {file}\n"), file).unwrap(), hash.to_ascii_lowercase());
        assert_eq!(sha256_of(&format!("\u{feff}{hash} *{file}\r\n"), file).unwrap(), hash.to_ascii_lowercase());
        assert_eq!(sha256_of(&format!("\n{hash}\n"), file).unwrap(), hash.to_ascii_lowercase());
        assert!(sha256_of(&format!("{hash}  remoteman-host-setup-0.1.7.exe\n"), file).unwrap_err().contains("0.1.7"));
        assert!(sha256_of(&format!("{}  {file}\n", &hash[..63]), file).is_err());
        assert!(sha256_of("<html>not found</html>", file).is_err());
        assert!(sha256_of("", file).is_err());
    }

    #[test]
    fn old_downloads_go() {
        let names = strings(&["remoteman-host-setup-0.1.6.exe", "remoteman-host-setup-0.1.7.exe", "remoteman-host-setup-0.1.8.exe", "remoteman-host-setup-0.1.9.exe", "remoteman-host-setup-0.1.9.exe.part", "remoteman-host-update.exe", "update.log"]);
        // At the start of 0.1.7: what is done with, and what didn't finish.
        assert_eq!(stale(&names, &v("0.1.7"), None), strings(&["remoteman-host-setup-0.1.6.exe", "remoteman-host-setup-0.1.7.exe", "remoteman-host-setup-0.1.9.exe.part"]));
        // 0.1.9 just downloaded: every other installer.
        assert_eq!(stale(&names, &v("0.1.7"), Some("remoteman-host-setup-0.1.9.exe")), strings(&["remoteman-host-setup-0.1.6.exe", "remoteman-host-setup-0.1.7.exe", "remoteman-host-setup-0.1.8.exe", "remoteman-host-setup-0.1.9.exe.part"]));
    }

    const STATE: &str = r"C:\ProgramData\remoteman\host";

    fn carried(line: &str) -> Result<Carried, String> {
        carry(&strings(&line.split(' ').collect::<Vec<_>>()), STATE)
    }

    #[test]
    fn the_installers_options_are_carried() {
        // As setup.ps1 writes the service's command line, and the service adds its event.
        let plain = carried(r"--state C:\ProgramData\remoteman\host --log C:\ProgramData\remoteman\host\host.log --stop-event Global\remoteman-host-stop").unwrap();
        assert_eq!(plain, Carried::default());
        let ask = carried(r"--state C:\ProgramData\remoteman\host --log C:\ProgramData\remoteman\host\host.log --ask").unwrap();
        assert!(ask.installer.is_empty());
        let both = carried(r"--state c:\programdata\remoteman\host\ --log C:\ProgramData\remoteman\host\host.log --encoder d3d12 --no-pair --updates install --stop-event E").unwrap();
        assert_eq!(both.installer, strings(&["--encoder", "d3d12", "--no-pair"]));
        assert_eq!(both.updates.as_deref(), Some("install"));
    }

    #[test]
    fn options_the_installer_cant_take_stop_the_update() {
        let why = carried(r"--state C:\ProgramData\remoteman\host --encoder nvenc --refine-qp 20 --d3d12-av1 --stop-event E").unwrap_err();
        assert!(why.contains("--refine-qp 20, --d3d12-av1"), "{why}");
        assert!(why.contains("lose them"), "{why}");
        let why = carried(r"--state D:\elsewhere --log C:\ProgramData\remoteman\host\host.log").unwrap_err();
        assert!(why.contains(r"--state D:\elsewhere") && why.contains("lose it"), "{why}");
        assert!(carried(r"--log C:\temp\host.log").is_err());
        assert!(carried("--allow files,clipboard").unwrap_err().contains("--allow files,clipboard"));
    }

    #[test]
    fn states_in_words() {
        assert_eq!(State::Available(v("0.1.8")).words(), "0.1.8 available");
        assert_eq!(State::Downloading(v("0.1.8")).words(), "downloading 0.1.8");
        assert_eq!(State::Failed("no network".into()).words(), "failed: no network");
        assert_eq!(State::UpToDate.key(), "up-to-date");
        for word in ["off", "check", "install"] {
            assert_eq!(Mode::parse(word).unwrap().as_str(), word);
        }
        assert!(Mode::parse("on").is_none());
    }
}
