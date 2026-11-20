//! The steps of installing and of uninstalling, run on a thread of their own (the pages hear
//! of each through events) or, silently, on the main one. The system's own steps are
//! setup.ps1's (windows/host-installer/setup.ps1): this only stops what is running, puts the
//! files in place, runs setup.ps1, and keeps Settings > Apps and the start at sign-in in order.
//!
//! Every change to the system goes through `Ctx::dry_run`: in a dry run it is written to the
//! log as what would be done, and the files go into a folder in %TEMP% instead of Program Files.
//! What is only read (is the service running, is there an earlier install) is read for real.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::archive::{self, Archive};
use crate::log;
use crate::system::{self, ServiceState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Install,
    Uninstall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepId {
    // Installing.
    Stop,
    Files,
    Register,
    Setup,
    App,
    // Uninstalling.
    CloseApp,
    Unsetup,
    Delete,
    Entries,
}

impl StepId {
    /// Its name for `--fail-at`.
    pub fn key(self) -> &'static str {
        match self {
            StepId::Stop | StepId::CloseApp => "stop",
            StepId::Files | StepId::Delete => "files",
            StepId::Register => "register",
            StepId::Setup | StepId::Unsetup => "setup",
            StepId::App => "app",
            StepId::Entries => "entries",
        }
    }
}

/// How this run goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Ctx {
    pub mode: Mode,
    pub dry_run: bool,
    /// A step that fails on purpose (dry runs only).
    pub fail_at: Option<String>,
    /// The setup file the files to install are read from.
    pub source: PathBuf,
    /// `C:\Program Files\remoteman host`.
    pub real_dir: PathBuf,
    /// Where the files go: `real_dir`, or in a dry run a folder in %TEMP%.
    pub dir: PathBuf,
}

impl Ctx {
    pub fn new(args: &crate::Args, source: PathBuf) -> Ctx {
        let real_dir = system::install_dir();
        let dir = if args.dry_run { dry_run_dir() } else { real_dir.clone() };
        Ctx {
            mode: if args.uninstall { Mode::Uninstall } else { Mode::Install },
            dry_run: args.dry_run,
            fail_at: if args.dry_run { args.fail_at.clone() } else { None },
            source,
            real_dir,
            dir,
        }
    }

    fn real(&self, path: &Path) -> String {
        path.display().to_string()
    }
}

/// Where a dry run puts the files.
pub fn dry_run_dir() -> PathBuf {
    std::env::temp_dir().join("remoteman-host-setup-dry-run").join("remoteman host")
}

/// What is there before anything is done.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    /// The version this setup installs.
    pub version: String,
    /// The files to install have Remoteman Host.
    pub has_app: bool,
    pub installed: Option<Installed>,
    /// Remoteman Host is installed now.
    pub app_installed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Installed {
    pub version: String,
    /// Installed by the Inno Setup installer.
    pub inno: bool,
}

pub fn facts(ctx: &Ctx, archive: Option<&Archive>) -> Facts {
    let ours = system::open_key(system::UNINSTALL_KEY, system::VIEW64).map(|k| k.get_str("DisplayVersion").unwrap_or_default());
    let inno = [system::VIEW64, system::VIEW32].into_iter().find_map(|view| system::open_key(system::INNO_KEY, view)).map(|k| k.get_str("DisplayVersion").unwrap_or_default());
    let installed = match (ours, inno) {
        (Some(v), _) => Some(Installed { version: v, inno: false }),
        (None, Some(v)) => Some(Installed { version: v, inno: true }),
        (None, None) if ctx.real_dir.join("setup.ps1").exists() => Some(Installed { version: String::new(), inno: false }),
        _ => None,
    };
    Facts {
        version: archive.map(|a| a.index.version.clone()).unwrap_or_default(),
        has_app: archive.is_some_and(|a| a.has_app()),
        installed,
        app_installed: ctx.real_dir.join(r"app\remoteman-host-app.exe").exists(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepInfo {
    pub id: StepId,
    pub title: String,
    pub caption: String,
}

pub fn plan(ctx: &Ctx, facts: &Facts) -> Vec<StepInfo> {
    let step = |id, title: &str, caption: &str| StepInfo { id, title: title.into(), caption: caption.into() };
    let dir = ctx.real(&ctx.real_dir);
    match ctx.mode {
        Mode::Install => {
            let mut steps = vec![
                step(StepId::Stop, "The running host", if facts.app_installed { "Stopped first, with Remoteman Host, so that their files can be replaced" } else { "Stopped first, so that its files can be replaced" }),
                step(StepId::Files, "Files", &format!("Into {dir}")),
                step(
                    StepId::Register,
                    "Settings > Apps",
                    if facts.installed.as_ref().is_some_and(|i| i.inno) { "An entry to remove it again, in place of the earlier installer's" } else { "An entry to remove it again" },
                ),
                step(StepId::Setup, "Driver, firewall and service", "setup.ps1 signs remoteman's display driver with a certificate made on this PC, lets in UDP port 7791, and starts the service"),
            ];
            if facts.has_app {
                steps.push(step(StepId::App, "Remoteman Host at sign-in", "Started for everyone who signs in to this PC, with a shortcut in Start"));
            } else if facts.app_installed {
                steps.push(step(StepId::App, "Remoteman Host", "The earlier one, and its start at sign-in, taken away"));
            }
            steps
        }
        Mode::Uninstall => vec![
            step(StepId::CloseApp, "Remoteman Host", "Closed, if it is open"),
            step(StepId::Unsetup, "Service, driver and firewall", "setup.ps1 removes the service, the display driver and its certificate, the firewall rule, and this PC's key and paired devices"),
            step(StepId::Delete, "Files", &format!("{dir}, deleted")),
            step(StepId::Entries, "Settings > Apps", "The entry, the start at sign-in and the shortcut in Start, removed"),
        ],
    }
}

/// What the pages hear.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Started(StepId),
    /// A line about the step while it runs (setup.ps1's latest, the file being written).
    Detail(StepId, String),
    /// Done: a word for it ("Installed") and a line about it.
    Done(StepId, String, Option<String>),
    Failed(StepId, String),
    Finished(Outcome),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub ok: bool,
    pub failed: Option<(StepId, String)>,
    /// The log a failure points at.
    pub log: PathBuf,
    /// Remoteman Host was running before and was stopped.
    pub app_was_running: bool,
    /// Files that go when Windows restarts.
    pub at_restart: usize,
}

struct Run<'a> {
    ctx: &'a Ctx,
    facts: &'a Facts,
    emit: &'a mut dyn FnMut(Event),
    app_was_running: bool,
    at_restart: usize,
    /// This run writes to setup.log in the install folder too.
    setup_log: bool,
}

type StepResult = Result<(String, Option<String>), String>;

/// Runs every step of `plan`, stopping at the first that fails.
pub fn run(ctx: &Ctx, facts: &Facts, emit: &mut dyn FnMut(Event)) -> Outcome {
    let steps = plan(ctx, facts);
    log::say(&format!(
        "{} remoteman host {}{} ({})",
        if ctx.mode == Mode::Install { "installing" } else { "uninstalling" },
        facts.version,
        if ctx.dry_run { ", DRY RUN: nothing is changed" } else { "" },
        ctx.dir.display()
    ));
    if !ctx.dry_run {
        system::enable_debug_privilege();
    }
    let mut run = Run { ctx, facts, emit, app_was_running: false, at_restart: 0, setup_log: false };
    if ctx.mode == Mode::Uninstall && ctx.dir.exists() {
        log::also(Some(ctx.dir.join("setup.log")));
        run.setup_log = true;
    }
    let mut failed = None;
    for step in &steps {
        (run.emit)(Event::Started(step.id));
        log::say(&format!("step: {} ({})", step.title, step.id.key()));
        let started = Instant::now();
        let result = if ctx.fail_at.as_deref() == Some(step.id.key()) && step.id != StepId::Setup && step.id != StepId::Unsetup {
            std::thread::sleep(Duration::from_millis(600));
            Err(format!("this step was made to fail (--fail-at {})", step.id.key()))
        } else {
            run.step(step.id)
        };
        // A dry run's steps are quick; each is given a moment, so that the pages can be seen
        // walking through them.
        if ctx.dry_run && started.elapsed() < Duration::from_millis(900) {
            std::thread::sleep(Duration::from_millis(900) - started.elapsed());
        }
        match result {
            Ok((word, detail)) => {
                log::say(&format!("  {word}{} ({:.1} s)", detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default(), started.elapsed().as_secs_f64()));
                (run.emit)(Event::Done(step.id, word, detail));
            }
            Err(reason) => {
                log::say(&format!("  FAILED: {reason}"));
                (run.emit)(Event::Failed(step.id, reason.clone()));
                failed = Some((step.id, reason));
                break;
            }
        }
    }
    let setup_log = ctx.dir.join("setup.log");
    let outcome = Outcome {
        ok: failed.is_none(),
        failed,
        log: if run.setup_log && setup_log.exists() { setup_log } else { log::path() },
        app_was_running: run.app_was_running,
        at_restart: run.at_restart,
    };
    log::say(if outcome.ok { "finished" } else { "stopped at the failure" });
    log::also(None);
    (run.emit)(Event::Finished(outcome.clone()));
    outcome
}

impl Run<'_> {
    fn step(&mut self, id: StepId) -> StepResult {
        match id {
            StepId::Stop => self.stop(),
            StepId::Files => self.files(),
            StepId::Register => self.register(),
            StepId::Setup => self.setup("install", id),
            StepId::App => self.app(),
            StepId::CloseApp => self.close_app(),
            StepId::Unsetup => self.setup("uninstall", id),
            StepId::Delete => self.delete(),
            StepId::Entries => self.entries(),
        }
    }

    fn would(&self, what: &str) {
        log::say(&format!("  would {what}"));
    }

    fn detail(&mut self, id: StepId, line: impl Into<String>) {
        (self.emit)(Event::Detail(id, line.into()));
    }

    /// Ends the processes running `exe`; how many there were.
    fn end(&mut self, exe: &Path) -> Result<usize, String> {
        let pids = system::processes_of(exe);
        for &pid in &pids {
            if self.ctx.dry_run {
                self.would(&format!("end {} (pid {pid})", exe.display()));
            } else {
                system::end_process(pid).map_err(|e| format!("{} (pid {pid}) couldn't be stopped: {e}", exe.display()))?;
                log::say(&format!("  ended {} (pid {pid})", exe.display()));
            }
        }
        Ok(pids.len())
    }

    // ---- Installing ----

    /// The host stopped before its files are replaced: the service, an older install's logon
    /// task, then any remoteman-host.exe left from the install folder, by its pid (as the Inno
    /// installer's PrepareToInstall did), and Remoteman Host, which holds its runtime's files.
    fn stop(&mut self) -> StepResult {
        let dir = &self.ctx.real_dir;
        let mut stopped = Vec::new();
        match system::service_state(system::SERVICE) {
            ServiceState::Missing | ServiceState::Stopped => {}
            _ => {
                if self.ctx.dry_run {
                    self.would(&format!("stop the service \"{}\" (it is running)", system::SERVICE));
                } else if let Err(e) = system::stop_service(system::SERVICE, Duration::from_secs(30)) {
                    // Its process is ended below in any case.
                    log::say(&format!("  the service: {e}"));
                }
                stopped.push("the service");
            }
        }
        let schtasks = system::windows_dir().join(r"System32\schtasks.exe");
        if matches!(system::run_hidden(&schtasks, &["/Query", "/TN", system::SERVICE], Duration::from_secs(20)), Ok((0, _))) {
            if self.ctx.dry_run {
                self.would(&format!("end the logon task \"{}\" of an older install", system::SERVICE));
            } else {
                let ended = system::run_hidden(&schtasks, &["/End", "/TN", system::SERVICE], Duration::from_secs(20));
                log::say(&format!("  the logon task ended: {ended:?}"));
            }
        }
        if self.end(&dir.join("remoteman-host.exe"))? > 0 {
            stopped.push("the host");
        }
        if self.end(&dir.join(r"app\remoteman-host-app.exe"))? > 0 {
            self.app_was_running = true;
            stopped.push("Remoteman Host");
        }
        Ok(match stopped.as_slice() {
            [] => ("Not running".into(), None),
            list => ("Stopped".into(), Some(format!("Stopped: {}", list.join(", ")))),
        })
    }

    fn files(&mut self) -> StepResult {
        let archive = Archive::open(&self.ctx.source).map_err(|e| e.to_string())?;
        let dir = self.ctx.dir.clone();
        if self.ctx.dry_run {
            self.would(&format!("copy the files into {} (they are unpacked into {} instead)", self.ctx.real_dir.display(), dir.display()));
            // A dry run starts from an empty folder of its own.
            let _ = std::fs::remove_dir_all(&dir);
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        log::also(Some(dir.join("setup.log")));
        self.setup_log = true;
        log::say(&format!("remoteman host setup {}: the files into {}", archive.index.version, dir.display()));

        // Remoteman Host's folder holds only this version's files: what an earlier one had and
        // this one doesn't is deleted (the rest of the install folder is left, installed.json
        // above all, which tells setup.ps1 what the earlier install made).
        let app = dir.join("app");
        if app.exists() {
            let keep: std::collections::HashSet<String> = archive.index.payload.iter().map(|e| e.path.to_lowercase()).collect();
            let mut stale = 0;
            for file in walk(&app) {
                let rel = file.strip_prefix(&dir).unwrap().to_string_lossy().to_lowercase();
                if !keep.contains(&rel) {
                    std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
                    stale += 1;
                }
            }
            if stale > 0 {
                log::say(&format!("  {stale} files of the earlier Remoteman Host deleted"));
            }
        }

        let mut last = Instant::now() - Duration::from_secs(1);
        let emit = &mut self.emit;
        let unpacked = archive
            .unpack(&archive.index.payload, &dir, &mut |path| {
                if last.elapsed() > Duration::from_millis(100) {
                    last = Instant::now();
                    emit(Event::Detail(StepId::Files, path.to_string()));
                }
            })
            .map_err(|e| e.to_string())?;
        let uninstaller = dir.join("uninstall.exe");
        archive.write_uninstaller(&uninstaller).map_err(|e| format!("{}: {e}", uninstaller.display()))?;
        // Read back: every file there, its size and SHA-256 those of the index.
        self.detail(StepId::Files, "Checking each file");
        let mut bytes = 0u64;
        for file in &unpacked {
            let (size, sha) = archive::hash_file(&file.path).map_err(|e| format!("{}: {e}", file.path.display()))?;
            if size != file.size || sha != file.sha256 {
                return Err(format!("{} isn't what was written", file.path.display()));
            }
            bytes += size;
        }
        let check = Archive::open(&uninstaller).map_err(|e| format!("the uninstaller: {e}"))?;
        if check.index.ui.len() != archive.index.ui.len() {
            return Err("the uninstaller is incomplete".into());
        }
        let line = format!("{} files, {:.1} MB, each checked; and uninstall.exe", unpacked.len(), bytes as f64 / 1_048_576.0);
        log::say(&format!("  {line}"));
        Ok(("Copied".into(), Some(line)))
    }

    fn register(&mut self) -> StepResult {
        let real = &self.ctx.real_dir;
        let size_kb = walk(&self.ctx.dir).iter().filter_map(|f| f.metadata().ok()).map(|m| m.len()).sum::<u64>() / 1024;
        let uninstaller = real.join("uninstall.exe");
        let strings = [
            ("DisplayName", "remoteman host".to_string()),
            ("DisplayVersion", self.facts.version.clone()),
            ("Publisher", "remoteman".to_string()),
            ("DisplayIcon", format!("{},0", uninstaller.display())),
            ("InstallLocation", real.display().to_string()),
            ("UninstallString", format!("\"{}\" --uninstall", uninstaller.display())),
            ("QuietUninstallString", format!("\"{}\" --uninstall --silent", uninstaller.display())),
            ("InstallDate", log::stamp()[..10].replace('-', "")),
        ];
        let dwords = [("EstimatedSize", size_kb as u32), ("NoModify", 1), ("NoRepair", 1)];
        if self.ctx.dry_run {
            for (name, value) in &strings {
                self.would(&format!(r"set HKLM\{}: {name} = {value}", system::UNINSTALL_KEY));
            }
            for (name, value) in &dwords {
                self.would(&format!(r"set HKLM\{}: {name} = {value} (DWORD)", system::UNINSTALL_KEY));
            }
        } else {
            let key = system::create_key(system::UNINSTALL_KEY).map_err(|e| e.to_string())?;
            for (name, value) in &strings {
                key.set_str(name, value).map_err(|e| e.to_string())?;
            }
            for (name, value) in &dwords {
                key.set_dword(name, *value).map_err(|e| e.to_string())?;
            }
        }
        // The Inno Setup installer's entry and its uninstaller go: this one is the uninstaller
        // now. Only those: its own uninstaller is never run (it would delete the host's state).
        let mut took_over = false;
        for view in [system::VIEW64, system::VIEW32] {
            if system::open_key(system::INNO_KEY, view).is_some() {
                took_over = true;
                if self.ctx.dry_run {
                    self.would(&format!(r"delete HKLM\{} (the earlier installer's entry)", system::INNO_KEY));
                } else {
                    system::delete_key(system::INNO_KEY, view).map_err(|e| e.to_string())?;
                    log::say(&format!(r"  HKLM\{} deleted", system::INNO_KEY));
                }
            }
        }
        for name in ["unins000.exe", "unins000.dat", "unins000.msg"] {
            let file = real.join(name);
            if file.exists() {
                took_over = true;
                if self.ctx.dry_run {
                    self.would(&format!("delete {}", file.display()));
                } else if std::fs::remove_file(&file).is_err() {
                    system::delete_at_restart(&file).map_err(|e| format!("{}: {e}", file.display()))?;
                }
            }
        }
        Ok(("Added".into(), took_over.then(|| "In place of the earlier installer's entry, whose uninstaller is gone".to_string())))
    }

    /// setup.ps1 install or uninstall, as the Inno installer ran it: hidden PowerShell,
    /// `-ExecutionPolicy Bypass`, exit code 0, or 1 with the reason as the last line of
    /// setup.log. setup.log's new lines are shown as they come.
    fn setup(&mut self, verb: &str, id: StepId) -> StepResult {
        let script = self.ctx.dir.join("setup.ps1");
        let setup_log = self.ctx.dir.join("setup.log");
        let ps = system::powershell();
        let args = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", &script.to_string_lossy(), verb].map(|s| s.to_string());
        if self.ctx.dry_run {
            self.would(&format!("run {} {} (hidden, in {})", ps.display(), args.join(" "), self.ctx.real_dir.display()));
            // What setup.ps1 would write, so that the page shows lines as it does.
            if self.ctx.dir.exists() {
                for line in ["(dry run) setup.ps1 would run here", "(dry run) the driver, the firewall rule and the service"] {
                    std::thread::sleep(Duration::from_millis(700));
                    append(&setup_log, line);
                    self.detail(id, line);
                }
                if self.ctx.fail_at.as_deref() == Some(id.key()) {
                    append(&setup_log, &format!("FAILED: a failure made with --fail-at {} (dry run)", id.key()));
                    return Err(last_reason(&setup_log).unwrap_or_default());
                }
            } else {
                std::thread::sleep(Duration::from_millis(1000));
                if self.ctx.fail_at.as_deref() == Some(id.key()) {
                    return Err(format!("a failure made with --fail-at {} (dry run)", id.key()));
                }
            }
            return Ok((if verb == "install" { "Installed" } else { "Removed" }.into(), None));
        }
        if !script.exists() {
            if verb == "uninstall" {
                log::say("  setup.ps1 isn't there: nothing it made is known");
                return Ok(("Skipped".into(), Some("setup.ps1 isn't there, so nothing it made is known".into())));
            }
            return Err(format!("{} is missing", script.display()));
        }
        let mut from = std::fs::metadata(&setup_log).map(|m| m.len()).unwrap_or(0);
        let mut child = std::process::Command::new(&ps);
        {
            use std::os::windows::process::CommandExt;
            child
                .args(&args)
                .current_dir(&self.ctx.dir)
                .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
        }
        let mut child = child.spawn().map_err(|e| format!("PowerShell couldn't be started: {e}"))?;
        let mut output = String::new();
        let (out, err) = (child.stdout.take(), child.stderr.take());
        let readers = [out.map(|o| Box::new(o) as Box<dyn Read + Send>), err.map(|e| Box::new(e) as Box<dyn Read + Send>)].map(|r| {
            std::thread::spawn(move || {
                let mut text = String::new();
                if let Some(mut r) = r {
                    let _ = r.read_to_string(&mut text);
                }
                text
            })
        });
        let started = Instant::now();
        let code = loop {
            for line in new_lines(&setup_log, &mut from) {
                self.detail(id, strip_stamp(&line));
            }
            if let Ok(Some(status)) = child.try_wait() {
                break status.code().unwrap_or(1);
            }
            if started.elapsed() > Duration::from_secs(15 * 60) {
                let _ = child.kill();
                return Err("setup.ps1 didn't finish within 15 minutes".into());
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        for r in readers {
            output.push_str(&r.join().unwrap_or_default());
        }
        let output = output.trim();
        if !output.is_empty() {
            log::say(&format!("  PowerShell said: {output}"));
        }
        // The last line, read before this program writes again.
        let reason = last_reason(&setup_log);
        if code == 0 {
            return Ok((if verb == "install" { "Installed" } else { "Removed" }.into(), None));
        }
        let reason = match reason {
            Some(r) if !r.is_empty() => r,
            _ if !output.is_empty() => output.lines().next().unwrap_or_default().to_string(),
            _ => format!("setup.ps1 ended with {code}"),
        };
        Err(reason)
    }

    fn app(&mut self) -> StepResult {
        let exe = self.ctx.real_dir.join(r"app\remoteman-host-app.exe");
        let link = system::common_programs().map(|p| p.join(system::SHORTCUT));
        if self.facts.has_app {
            let command = format!("\"{}\" --background", exe.display());
            if self.ctx.dry_run {
                self.would(&format!(r"set HKLM\{}: {} = {command}", system::RUN_KEY, system::RUN_VALUE));
                if let Some(link) = &link {
                    self.would(&format!("make the shortcut {} to {}", link.display(), exe.display()));
                }
            } else {
                system::create_key(system::RUN_KEY).and_then(|k| k.set_str(system::RUN_VALUE, &command)).map_err(|e| e.to_string())?;
                if let Some(link) = &link {
                    system::make_shortcut(link, &exe, "Remoteman Host: let your devices use this PC").map_err(|e| format!("{}: {e}", link.display()))?;
                }
            }
            Ok(("Added".into(), None))
        } else {
            self.remove_app_entries()?;
            Ok(("Removed".into(), None))
        }
    }

    fn remove_app_entries(&mut self) -> Result<(), String> {
        let link = system::common_programs().map(|p| p.join(system::SHORTCUT));
        let run = system::open_key(system::RUN_KEY, system::VIEW64).and_then(|k| k.get_str(system::RUN_VALUE));
        if self.ctx.dry_run {
            if run.is_some() {
                self.would(&format!(r"delete HKLM\{}: {}", system::RUN_KEY, system::RUN_VALUE));
            }
            if let Some(link) = link.filter(|l| l.exists()) {
                self.would(&format!("delete {}", link.display()));
            }
            return Ok(());
        }
        if run.is_some()
            && let Some(key) = system::open_key_writable(system::RUN_KEY)
        {
            key.delete_value(system::RUN_VALUE).map_err(|e| e.to_string())?;
        }
        if let Some(link) = link.filter(|l| l.exists()) {
            std::fs::remove_file(&link).map_err(|e| format!("{}: {e}", link.display()))?;
        }
        Ok(())
    }

    // ---- Uninstalling ----

    fn close_app(&mut self) -> StepResult {
        let n = self.end(&self.ctx.real_dir.join(r"app\remoteman-host-app.exe"))?;
        Ok(if n > 0 { ("Closed".into(), None) } else { ("Not open".into(), None) })
    }

    fn delete(&mut self) -> StepResult {
        let dir = self.ctx.dir.clone();
        // setup.log goes with the folder: kept in %TEMP% first.
        let setup_log = dir.join("setup.log");
        log::also(None);
        self.setup_log = false;
        if setup_log.exists() {
            let kept = std::env::temp_dir().join("remoteman-host-setup-removed.log");
            let _ = std::fs::copy(&setup_log, &kept);
            log::say(&format!("  setup.log kept as {}", kept.display()));
        }
        if self.ctx.dry_run {
            self.would(&format!("delete {} (here: {}, which is deleted)", self.ctx.real_dir.display(), dir.display()));
        }
        // Files in use go to a folder on the same drive (Windows' Temp; the user's in a dry run)
        // and when Windows restarts.
        let trash = system::private_dir(!self.ctx.dry_run).ok();
        let removed = system::remove_dir(&dir, trash.as_deref()).map_err(|e| e.to_string());
        if let Some(trash) = &trash {
            if std::fs::remove_dir(trash).is_err() {
                let _ = system::delete_at_restart(trash);
            }
        }
        let removed = removed?;
        self.at_restart = removed.at_restart.len();
        for p in &removed.at_restart {
            log::say(&format!("  deleted when Windows restarts: {}", p.display()));
        }
        Ok((
            "Deleted".into(),
            Some(match removed.at_restart.len() {
                0 => format!("{} files", removed.files),
                n => format!("{} files; {n} more when Windows restarts", removed.files),
            }),
        ))
    }

    fn entries(&mut self) -> StepResult {
        self.remove_app_entries()?;
        for (path, view) in [(system::UNINSTALL_KEY, system::VIEW64), (system::INNO_KEY, system::VIEW64), (system::INNO_KEY, system::VIEW32)] {
            if system::open_key(path, view).is_some() {
                if self.ctx.dry_run {
                    self.would(&format!(r"delete HKLM\{path}"));
                } else {
                    system::delete_key(path, view).map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(("Removed".into(), None))
    }
}

/// Every file under `dir`.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(entry.path()),
                Ok(_) => out.push(entry.path()),
                Err(_) => {}
            }
        }
    }
    out
}

fn append(file: &Path, line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
        let _ = write!(f, "{} {line}\r\n", log::stamp());
    }
}

/// The lines added to `file` since `from` (which moves on).
fn new_lines(file: &Path, from: &mut u64) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(file) else { return Vec::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len <= *from {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if f.seek(SeekFrom::Start(*from)).is_err() || f.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    // Whole lines only; a part line waits for the rest.
    let Some(end) = bytes.iter().rposition(|&b| b == b'\n') else { return Vec::new() };
    *from += end as u64 + 1;
    String::from_utf8_lossy(&bytes[..end]).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

/// `2026-11-19T18:35:02 the rest` without its time.
fn strip_stamp(line: &str) -> String {
    let rest = match line.split_once(' ') {
        Some((stamp, rest)) if stamp.len() == 19 && stamp.as_bytes().get(10) == Some(&b'T') => rest,
        _ => line,
    };
    rest.strip_prefix("setup.exe: ").unwrap_or(rest).to_string()
}

/// setup.ps1's reason for failing: the last line of setup.log, without its time and "FAILED:".
fn last_reason(file: &Path) -> Option<String> {
    let text = std::fs::read(file).ok()?;
    let text = String::from_utf8_lossy(&text);
    let last = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let line = strip_stamp(last.trim());
    Some(line.strip_prefix("FAILED: ").unwrap_or(&line).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_come_off() {
        assert_eq!(strip_stamp("2026-11-19T18:35:02 FAILED: no driver"), "FAILED: no driver");
        assert_eq!(strip_stamp("2026-11-19T18:35:02 setup.exe: step: Files"), "step: Files");
        assert_eq!(strip_stamp("no stamp here"), "no stamp here");
    }

    #[test]
    fn reasons_are_the_last_line() {
        let dir = std::env::temp_dir().join(format!("rm-setup-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("setup.log");
        std::fs::write(&log, "2026-11-19T18:35:01 install from C:\\x\r\n2026-11-19T18:35:02 FAILED: pnputil did not take the driver\r\n\r\n").unwrap();
        assert_eq!(last_reason(&log).unwrap(), "pnputil did not take the driver");
        let mut from = 0;
        assert_eq!(new_lines(&log, &mut from).len(), 2);
        assert!(new_lines(&log, &mut from).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
