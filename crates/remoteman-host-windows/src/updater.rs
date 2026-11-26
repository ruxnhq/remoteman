//! Updates of the installed host. It looks for a newer release by itself, a minute after it
//! starts and every 12 hours after that, and when asked (`update check`, the Check now of
//! Remoteman Host). What it finds goes to the log and into `status` (`update`), where Remoteman
//! Host shows it with an Install button; `update install` downloads it and runs it. With `set
//! updates install` (the app's switch, off unless the owner turns it on) or `--updates install`
//! it installs by itself, once no device is connected. Never while one is: the app's Install
//! asks the owner first, and then says `update install anyway`, which lets the device go.
//!
//! Where from: remoteman's own downloads. The newest `remoteman-host-setup-X.Y.Z.exe` that
//! https://remoteman.ruxn.com/ links (`update::newest_on_page`, as there is no manifest), from
//! https://files.ruxn.com/, with the `.sha256` published beside it. Both are fetched with
//! Windows' own curl.exe (System32's, with fixed arguments and no shell), over HTTPS only, on
//! redirects too. The installer isn't code-signed (docs/WINDOWS-PACKAGING.md), so nothing else
//! vouches for it: trust rests on TLS to RUXN's two domains and on the file matching the SHA-256
//! published beside it, checked when it has been downloaded and again just before it runs. It
//! is kept in the host's state folder (`STATE\updates`, which only Administrators and SYSTEM
//! can write), and older ones are deleted.
//!
//! How: only a host the service started from an install of remoteman-host-setup's can install
//! one (`installable`). Its installer, run again, keeps the host's identity and trust list but
//! writes the service's command line from its own options, so it is given the ones it takes
//! (`--encoder`, `--no-pair`) as the service has them now (`update::carry`); a command line with
//! any other option, which an update would lose, isn't updated, and says why. The installer
//! stops the service, and with it this host, so it is run by a copy of this program outside the
//! install folder (`STATE\updates\remoteman-host-update.exe update-run ...`, [`run_installer`]),
//! which waits for it, starts the service again if the install left it stopped, starts
//! Remoteman Host again for the user at the console (the installer closes it), and leaves
//! `STATE\updates\last.json` for the next host to say how it went.
//!
//! `--updates off`, or `{"mode":"off"}` in `STATE\updates.json` (which `set updates off`
//! writes), has the host never look by itself: for a manager that installs a copy it pins, as
//! Guesthouse does in its guests at every boot. A host the service didn't start doesn't look by
//! itself unless `--updates` says so.

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use remoteman_core::host::Host;
use sha2::{Digest, Sha256};

use crate::update::{self, Carried, Mode, Release, State, Version};

/// The first look, after the host has started (and the network is likely up).
const FIRST: Duration = Duration::from_secs(60);
const EVERY: Duration = Duration::from_secs(12 * 3600);
/// After a look that failed.
const AGAIN: Duration = Duration::from_secs(3600);
/// How often an update that installs by itself looks whether the device has gone.
const DEVICE_WAIT: Duration = Duration::from_secs(60);
/// How often a host whose installer runs looks for the runner's word (it is only still there
/// to look if the installer failed before stopping it).
const INSTALL_WAIT: Duration = Duration::from_secs(30);
/// The longest an install may take before the runner gives up on it.
const INSTALL_LIMIT: Duration = Duration::from_secs(20 * 60);
/// The copy of this program that runs the installer.
const RUNNER: &str = "remoteman-host-update.exe";

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// `--updates`, when the command line says it: it decides over what the owner set.
pub static SAID: OnceLock<Mode> = OnceLock::new();

static SHARED: OnceLock<Shared> = OnceLock::new();

struct Shared {
    now: Mutex<Now>,
    wake: Condvar,
    /// The host's state folder.
    state: PathBuf,
    /// Started by the service: the host looks by itself unless told not to.
    service: bool,
    /// What an install is given, or why this host can't install one (asked once at the start).
    install: Result<Installed, String>,
}

#[derive(Clone)]
struct Installed {
    /// `C:\Program Files\remoteman host`.
    dir: PathBuf,
    carried: Carried,
}

struct Now {
    state: State,
    /// What the owner set (`STATE\updates.json`).
    saved: Mode,
    /// The newest release seen, while it is newer than this host.
    newer: Option<Release>,
    asked: Option<Asked>,
    /// When it last looked, in seconds since 1970.
    checked: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    Check,
    /// `anyway`: the owner said yes to letting a connected device go.
    Install { anyway: bool },
}

enum Outcome {
    /// The installer runs; it stops this host.
    Started,
    /// A device is connected: not now.
    Busy,
}

fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|| Version::parse("0").unwrap())
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn system32() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32")
}

fn folder(state: &Path) -> PathBuf {
    state.join("updates")
}

fn mode(service: bool, saved: Mode) -> Mode {
    SAID.get().copied().unwrap_or(if service { saved } else { Mode::Off })
}

fn load_saved(state: &Path) -> Mode {
    std::fs::read_to_string(state.join("updates.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("mode").and_then(|m| m.as_str()).and_then(Mode::parse))
        .unwrap_or(Mode::Check)
}

fn save_saved(state: &Path, mode: Mode) -> std::io::Result<()> {
    std::fs::write(state.join("updates.json"), serde_json::json!({ "mode": mode.as_str() }).to_string())
}

/// Can this host install an update, and with what? Only one the service started, from an
/// install of remoteman-host-setup's, with options its installer can be given again.
fn installable() -> Result<Installed, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|a| a == "--stop-event") || !crate::desk::is_system() {
        return Err("this host wasn't started by the service \"remoteman host\", so it doesn't install updates; install one from https://remoteman.ruxn.com/".into());
    }
    let exe = std::env::current_exe().map_err(|e| format!("where this program is: {e}"))?;
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    // setup.ps1 leaves its record of what it installed beside itself.
    if !dir.join("installed.json").exists() || !dir.join("setup.ps1").exists() {
        return Err(format!("{} wasn't installed by remoteman's installer (there is no installed.json beside it), so it doesn't install updates", exe.display()));
    }
    // The state folder setup.ps1 gives the service's host.
    let program_data = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    let carried = update::carry(&args, &format!(r"{program_data}\remoteman\host"))?;
    Ok(Installed { dir, carried })
}

/// Start looking for updates, for as long as `host` lives. Says how the last update went, if
/// the runner left word, and deletes downloads that are done with.
pub fn start(state: PathBuf, host: Weak<Host>) {
    let dir = folder(&state);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("updates: {} can't be made ({e}); none can be downloaded", dir.display());
    }
    let service = std::env::args().any(|a| a == "--stop-event") && crate::desk::is_system();
    let first = take_last(&dir).unwrap_or(State::NotYet);
    clean(&dir, None);
    let saved = load_saved(&state);
    let shared = Shared { now: Mutex::new(Now { state: first, saved, newer: None, asked: None, checked: None }), wake: Condvar::new(), state, service, install: installable() };
    let m = mode(service, saved);
    match &shared.install {
        Ok(i) => tracing::info!(mode = m.as_str(), carried = ?i.carried.installer, "updates: looked for {}", if m == Mode::Off { "only when asked" } else { "by this host itself" }),
        Err(why) => tracing::info!(mode = m.as_str(), "updates: {why}"),
    }
    if SHARED.set(shared).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new().name("updates".into()).spawn(move || watch(SHARED.get().unwrap(), host));
    if let Err(e) = spawned {
        tracing::warn!("updates: couldn't start looking for them: {e}");
    }
}

fn set_state(s: &Shared, state: State) {
    let mut now = s.now.lock().unwrap();
    if now.state != state {
        match &state {
            State::Failed(_) => tracing::warn!("updates: {}", state.words(&current())),
            _ => tracing::info!("updates: {}", state.words(&current())),
        }
        now.state = state;
    }
}

fn watch(s: &'static Shared, host: Weak<Host>) {
    let mut next_check = Instant::now() + FIRST;
    let mut retry: Option<Instant> = None;
    // The version an update that installs by itself said it waits for the device with.
    let mut waiting: Option<Version> = None;
    loop {
        let asked = {
            let mut now = s.now.lock().unwrap();
            loop {
                if let Some(asked) = now.asked.take() {
                    break Some(asked);
                }
                let left = retry.map_or(next_check, |r| r.min(next_check)).saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break None;
                }
                now = s.wake.wait_timeout(now, left).unwrap().0;
            }
        };
        if host.strong_count() == 0 {
            return;
        }
        retry = None;
        let (saved, installing) = {
            let now = s.now.lock().unwrap();
            (now.saved, matches!(now.state, State::Installing(_)))
        };
        // How an update went, as its runner says once the installer has ended: to the new
        // host, which the installer started before that, or to this one, still here because
        // the installer failed before it stopped it.
        if let Some(said) = take_last(&folder(&s.state)) {
            set_state(s, said);
        } else if installing {
            retry = Some(Instant::now() + INSTALL_WAIT);
            continue;
        }
        let mode = mode(s.service, saved);
        if asked.is_some() || (Instant::now() >= next_check && mode != Mode::Off) {
            next_check = Instant::now() + if check(s, asked.is_some()) { EVERY } else { AGAIN };
        } else if Instant::now() >= next_check {
            next_check = Instant::now() + EVERY;
        }
        let Some(release) = s.now.lock().unwrap().newer.clone() else { continue };
        let anyway = asked == Some(Asked::Install { anyway: true });
        if !matches!(asked, Some(Asked::Install { .. })) && mode != Mode::Install {
            continue;
        }
        if let Err(why) = &s.install {
            if asked.is_some() {
                set_state(s, State::Failed(why.clone()));
            }
            continue;
        }
        match install(s, &host, &release, anyway) {
            Ok(Outcome::Started) => retry = Some(Instant::now() + INSTALL_WAIT),
            Ok(Outcome::Busy) => {
                if waiting.as_ref() != Some(&release.version) {
                    tracing::info!("updates: {} is installed once no device is connected", release.version);
                    waiting = Some(release.version.clone());
                }
                retry = Some(Instant::now() + DEVICE_WAIT);
            }
            Err(why) => set_state(s, State::Failed(why)),
        }
    }
}

/// Look at the download page. Whether it could be read. A check the owner asked for says what
/// it found even right after an update; one of the host's own leaves "updated from" standing.
fn check(s: &Shared, asked: bool) -> bool {
    let before = s.now.lock().unwrap().state.clone();
    set_state(s, State::Checking);
    let found = fetch_text(update::PAGE, 2 << 20, 30).and_then(|html| update::newest_on_page(&html));
    s.now.lock().unwrap().checked = Some(unix_now());
    match found {
        Ok(release) if release.version > current() => {
            let version = release.version.clone();
            s.now.lock().unwrap().newer = Some(release);
            set_state(s, State::Available(version));
            true
        }
        Ok(release) => {
            s.now.lock().unwrap().newer = None;
            // "Updated from" stays until there is news, or the owner asks.
            set_state(s, if matches!(before, State::Updated(_)) && !asked { before } else { State::UpToDate(release.version) });
            true
        }
        Err(why) => {
            set_state(s, State::Failed(format!("couldn't look for updates: {why}")));
            false
        }
    }
}

/// Is a device connected (the one being served, or one that joined it)?
fn connected(host: &Host) -> bool {
    host.link().is_some() || host.devices().iter().any(|d| d.connected)
}

/// Download `release`, check it, and have the runner install it.
fn install(s: &Shared, host: &Weak<Host>, release: &Release, anyway: bool) -> Result<Outcome, String> {
    let installed = s.install.clone()?;
    let v = &release.version;
    let busy = || host.upgrade().is_some_and(|h| connected(&h));
    if busy() && !anyway {
        return Ok(Outcome::Busy);
    }
    let (file, sha256) = download(s, release)?;
    // The download takes a while: a device may have come meanwhile.
    if let Some(h) = host.upgrade()
        && connected(&h)
    {
        if !anyway {
            set_state(s, State::Available(v.clone()));
            return Ok(Outcome::Busy);
        }
        h.close_client(&format!("This computer's remoteman host is being updated to {v}. It is back in a minute or two."));
        tracing::info!("updates: the connected device was let go for the update, as the owner said");
    }
    // What the installer can't be given, kept where the next host reads it.
    if let Some(m) = installed.carried.updates.as_deref().and_then(Mode::parse)
        && let Err(e) = save_saved(&s.state, m)
    {
        return Err(format!("--updates {} couldn't be kept for after the update: {e}", m.as_str()));
    }
    // The runner is a copy of this program outside the install folder, as the installer
    // replaces that folder's files and ends whatever runs from them.
    let runner = folder(&s.state).join(RUNNER);
    let me = std::env::current_exe().map_err(|e| format!("where this program is: {e}"))?;
    std::fs::copy(&me, &runner).map_err(|e| format!("{} couldn't be copied to {}: {e}", me.display(), runner.display()))?;
    let mut command = Command::new(&runner);
    command
        .arg("update-run")
        .arg("--state")
        .arg(&s.state)
        .args(["--from", env!("CARGO_PKG_VERSION"), "--to", &v.to_string(), "--sha256", &sha256])
        .arg("--installer")
        .arg(&file)
        .arg("--app")
        .arg(installed.dir.join(r"app\remoteman-host-app.exe"))
        .arg("--")
        .args(&installed.carried.installer)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Out of any job the service's processes are in, where that is allowed, so that it outlives
    // this host.
    let child = command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB).spawn().or_else(|_| command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP).spawn());
    let child = child.map_err(|e| format!("{} couldn't be started: {e}", runner.display()))?;
    tracing::info!(pid = child.id(), installer = %file.display(), options = ?installed.carried.installer, "updates: the installer is run; it stops this host and starts the new one");
    set_state(s, State::Installing(v.clone()));
    Ok(Outcome::Started)
}

/// The installer, from `STATE\updates` if it is there already, checked against its `.sha256`.
fn download(s: &Shared, release: &Release) -> Result<(PathBuf, String), String> {
    let dir = folder(&s.state);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    set_state(s, State::Downloading(release.version.clone()));
    let sums = fetch_text(&release.sha256_url(), 4096, 30)?;
    let want = update::sha256_of(&sums, &release.file)?;
    let file = dir.join(&release.file);
    if file.exists() && hash(&file).ok().as_deref() == Some(want.as_str()) {
        tracing::info!("updates: {} was downloaded already", release.file);
    } else {
        let part = dir.join(format!("{}.part", release.file));
        let _ = std::fs::remove_file(&part);
        fetch(&release.url, Some(&part), 512 << 20, 30 * 60)?;
        let got = hash(&part)?;
        if got != want {
            let _ = std::fs::remove_file(&part);
            return Err(format!("{} doesn't match the SHA-256 published beside it ({got}, not {want}): deleted, not run", release.file));
        }
        let _ = std::fs::remove_file(&file);
        std::fs::rename(&part, &file).map_err(|e| format!("{}: {e}", file.display()))?;
        tracing::info!(sha256 = want, "updates: {} downloaded and checked", release.file);
    }
    clean(&dir, Some(&release.file));
    Ok((file, want))
}

/// Delete the downloads that are done with (`update::stale`).
fn clean(dir: &Path, keep: Option<&str>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let names: Vec<String> = entries.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
    for name in update::stale(&names, &current(), keep) {
        match std::fs::remove_file(dir.join(&name)) {
            Ok(()) => tracing::info!("updates: {name} deleted"),
            Err(e) => tracing::warn!("updates: {name} couldn't be deleted: {e}"),
        }
    }
}

/// A file's SHA-256 in lower-case hex.
fn hash(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut sha = Sha256::new();
    let mut piece = vec![0u8; 1 << 20];
    loop {
        match file.read(&mut piece) {
            Ok(0) => break,
            Ok(n) => sha.update(&piece[..n]),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(sha.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Fetch `url` (HTTPS only, on redirects too) with Windows' curl.exe: into `out`, or returned.
/// At most `max` bytes and `seconds` long.
fn fetch(url: &str, out: Option<&Path>, max: u64, seconds: u64) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        return Err(format!("{url}: only HTTPS is used"));
    }
    let curl = system32().join("curl.exe");
    let mut command = Command::new(&curl);
    command
        .args(["--silent", "--show-error", "--fail", "--location", "--max-redirs", "5", "--proto", "=https", "--proto-redir", "=https", "--tlsv1.2"])
        .args(["--max-filesize", &max.to_string(), "--max-time", &seconds.to_string()])
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    if let Some(out) = out {
        command.arg("--output").arg(out);
    }
    let said = command.arg(url).output().map_err(|e| format!("{} couldn't be run: {e}", curl.display()))?;
    if !said.status.success() {
        let why = String::from_utf8_lossy(&said.stderr).trim().to_string();
        return Err(format!("{url}: {} (curl ended with {})", if why.is_empty() { "no answer" } else { &why }, said.status.code().unwrap_or(-1)));
    }
    if said.stdout.len() as u64 > max {
        return Err(format!("{url}: more than {max} bytes"));
    }
    Ok(said.stdout)
}

fn fetch_text(url: &str, max: u64, seconds: u64) -> Result<String, String> {
    String::from_utf8(fetch(url, None, max, seconds)?).map_err(|_| format!("{url}: not text"))
}

/// What the runner left, as a state to say: the update installed (this host is the new one), or
/// why not. Taken: the file goes.
fn take_last(dir: &Path) -> Option<State> {
    let path = dir.join("last.json");
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    if v.get("ok").and_then(|x| x.as_bool()) == Some(true) {
        Version::parse(&s("from")).map(State::Updated)
    } else {
        Some(State::Failed(format!("the update to {} didn't install: {}", s("to"), s("why"))))
    }
}

/// `ctl update [check | install [anyway]]`.
pub fn answer(host: &Host, what: Option<&str>, more: Option<&str>) -> String {
    let Some(s) = SHARED.get() else { return "this host doesn't look for updates\n".into() };
    match (what, more) {
        (None | Some("status"), _) => format!("{}\n", s.now.lock().unwrap().state.words(&current())),
        (Some("check"), None) => {
            ask(s, Asked::Check);
            "ok\n".into()
        }
        (Some("install"), anyway @ (None | Some("anyway"))) => {
            if let Err(why) = &s.install {
                return format!("{why}\n");
            }
            if matches!(s.now.lock().unwrap().state, State::Installing(_)) {
                return "an update is being installed\n".into();
            }
            if anyway.is_none() && connected(host) {
                return "a device is connected: `update install anyway` lets it go and installs\n".into();
            }
            ask(s, Asked::Install { anyway: anyway.is_some() });
            "ok\n".into()
        }
        _ => "update [check | install [anyway]]\n".into(),
    }
}

fn ask(s: &Shared, asked: Asked) {
    let mut now = s.now.lock().unwrap();
    // Installing is more than looking, and the owner's yes to letting a device go stands.
    now.asked = Some(match (now.asked, asked) {
        (Some(Asked::Install { anyway: a }), Asked::Install { anyway: b }) => Asked::Install { anyway: a || b },
        (Some(Asked::Install { anyway }), Asked::Check) => Asked::Install { anyway },
        (_, asked) => asked,
    });
    s.wake.notify_one();
}

/// `ctl set updates off|check|install`: whether this host looks by itself, and installs too.
pub fn set(word: &str) -> String {
    let Some(mode) = Mode::parse(word) else { return "set updates off|check|install\n".into() };
    let Some(s) = SHARED.get() else { return "this host doesn't look for updates\n".into() };
    if let Some(said) = SAID.get() {
        return format!("this host was started with --updates {}, which decides this\n", said.as_str());
    }
    s.now.lock().unwrap().saved = mode;
    s.wake.notify_one();
    tracing::info!(mode = mode.as_str(), "updates: set by the owner");
    match save_saved(&s.state, mode) {
        Ok(()) => "ok\n".into(),
        Err(e) => format!("done, but it won't be remembered: {e}\n"),
    }
}

/// For `status`: how updates stand.
pub fn status() -> serde_json::Value {
    let Some(s) = SHARED.get() else { return serde_json::Value::Null };
    let now = s.now.lock().unwrap();
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "mode": mode(s.service, now.saved).as_str(),
        "said": SAID.get().is_some(),
        "state": now.state.key(),
        "words": now.state.words(&current()),
        "newer": now.newer.as_ref().map(|r| r.version.to_string()),
        "checked": now.checked,
        "can_install": s.install.is_ok(),
        "why_not": s.install.as_ref().err(),
    })
}

/// `remoteman-host update-run --state DIR --from V --to V --sha256 HEX --installer FILE
/// --app FILE -- INSTALLER OPTIONS`: the runner, a copy of the host's program in `STATE\updates`
/// started by the host. It checks the installer once more, runs it with `--silent`, the options
/// and a log of its own (`STATE\updates\setup.log`), and waits; then writes `last.json`, starts
/// the service if the install left it stopped, and starts Remoteman Host for the user at the
/// console. Its own lines go to `STATE\updates\update.log`.
pub fn run_installer(args: Vec<String>) -> anyhow::Result<()> {
    let (mut state, mut from, mut to, mut sha256, mut installer, mut app) = (None, String::new(), String::new(), String::new(), None, None);
    let mut options = Vec::new();
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        let mut value = || args.next().with_context(|| format!("update-run: {a} needs a value"));
        match a.as_str() {
            "--state" => state = Some(PathBuf::from(value()?)),
            "--from" => from = value()?,
            "--to" => to = value()?,
            "--sha256" => sha256 = value()?,
            "--installer" => installer = Some(PathBuf::from(value()?)),
            "--app" => app = Some(PathBuf::from(value()?)),
            "--" => {
                options = args.by_ref().collect();
            }
            other => anyhow::bail!("update-run: unknown option {other}"),
        }
    }
    let state = state.context("update-run: --state DIR")?;
    let installer = installer.context("update-run: --installer FILE")?;
    let dir = folder(&state);
    if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("update.log")) {
        tracing_subscriber::fmt().with_ansi(false).with_writer(std::sync::Mutex::new(file)).init();
    }
    tracing::info!(from, to, installer = %installer.display(), ?options, "the update runs");
    let result = run_setup(&dir, &installer, &sha256, &options);
    match &result {
        Ok(()) => tracing::info!("the update to {to} installed"),
        Err(why) => tracing::warn!("the update to {to} didn't install: {why}"),
    }
    let last = serde_json::json!({ "from": from, "to": to, "ok": result.is_ok(), "why": result.as_ref().err() });
    if let Err(e) = std::fs::write(dir.join("last.json"), last.to_string()) {
        tracing::warn!("last.json couldn't be written: {e}");
    }
    // A failed install can leave the service stopped (the installer stops it first and puts
    // nothing back): it is started again, with whatever is installed now, after last.json is
    // there for its host to read.
    start_service();
    // The installer closes Remoteman Host; it would start again only at the next sign-in.
    if let Some(app) = app.filter(|a| a.exists()) {
        match start_for_console_user(&app) {
            Ok(()) => tracing::info!("Remoteman Host started again for the user at the console"),
            Err(why) => tracing::info!("Remoteman Host wasn't started again: {why}"),
        }
    }
    Ok(())
}

fn run_setup(dir: &Path, installer: &Path, sha256: &str, options: &[String]) -> Result<(), String> {
    let got = hash(installer)?;
    if got != sha256 {
        return Err(format!("{} changed since it was checked ({got}, not {sha256}): not run", installer.display()));
    }
    let log = dir.join("setup.log");
    let _ = std::fs::remove_file(&log);
    let mut child = Command::new(installer)
        .arg("--silent")
        .args(options)
        .arg("--log")
        .arg(&log)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("{} couldn't be started: {e}", installer.display()))?;
    let began = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if began.elapsed() < INSTALL_LIMIT => std::thread::sleep(Duration::from_secs(1)),
            Ok(None) => {
                let _ = child.kill();
                return Err(format!("the installer hadn't finished after {} minutes, and was ended (its log: {})", INSTALL_LIMIT.as_secs() / 60, log.display()));
            }
            Err(e) => return Err(format!("the installer couldn't be waited for: {e}")),
        }
    };
    if status.success() {
        return Ok(());
    }
    // The installer's reason is its last "FAILED:" line.
    let reason = std::fs::read_to_string(&log).ok().and_then(|t| t.lines().rev().find_map(|l| l.split_once("FAILED: ").map(|(_, why)| why.to_string())));
    Err(format!("the installer ended with {}{} (its log: {})", status.code().unwrap_or(-1), reason.map(|r| format!(": {r}")).unwrap_or_default(), log.display()))
}

/// Start the service, if it isn't running (`sc.exe start` says so if it is).
fn start_service() {
    let sc = system32().join("sc.exe");
    match Command::new(&sc).args(["start", crate::service::NAME]).stdin(Stdio::null()).creation_flags(CREATE_NO_WINDOW).output() {
        // 1056: ERROR_SERVICE_ALREADY_RUNNING.
        Ok(o) if o.status.success() || o.status.code() == Some(1056) => tracing::info!("the service runs"),
        Ok(o) => tracing::warn!("sc start: {} {}", o.status, String::from_utf8_lossy(&o.stdout).trim()),
        Err(e) => tracing::warn!("{} couldn't be run: {e}", sc.display()),
    }
}

/// Start `app --background` as the user signed in at the console, in their own environment
/// (this process is SYSTEM's).
fn start_for_console_user(app: &Path) -> Result<(), String> {
    use std::ffi::c_void;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows::Win32::System::Threading::{CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, PROCESS_INFORMATION, STARTUPINFOW};
    use windows::core::{HSTRING, PCWSTR, PWSTR};

    let user = crate::desk::console_user().ok_or("nobody is signed in at the console")?;
    let mut line: Vec<u16> = format!("\"{}\" --background", app.display()).encode_utf16().chain([0]).collect();
    let mut desktop: Vec<u16> = "winsta0\\default".encode_utf16().chain([0]).collect();
    let folder = app.parent().map(|p| HSTRING::from(p.as_os_str()));
    // SAFETY: the token is the caller's for the call; the strings and the environment block
    // outlive the calls; the block and the handles that come back are freed here.
    unsafe {
        let mut environment: *mut c_void = std::ptr::null_mut();
        CreateEnvironmentBlock(&mut environment, Some(user.0), false).map_err(|e| format!("the user's environment: {e}"))?;
        let start = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, lpDesktop: PWSTR(desktop.as_mut_ptr()), ..Default::default() };
        let mut info = PROCESS_INFORMATION::default();
        let made = CreateProcessAsUserW(
            Some(user.0),
            PCWSTR::null(),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT,
            Some(environment),
            folder.as_ref().map_or(PCWSTR::null(), |f| PCWSTR(f.as_ptr())),
            &start,
            &mut info,
        );
        let _ = DestroyEnvironmentBlock(environment);
        made.map_err(|e| format!("starting {}: {e}", app.display()))?;
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
    }
    Ok(())
}
