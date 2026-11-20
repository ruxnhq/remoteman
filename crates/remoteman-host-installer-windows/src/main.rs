//! remoteman host's installer and uninstaller for Windows, one program:
//!
//!     remoteman-host-setup [--uninstall] [--silent] [--dry-run] [--fail-at STEP]
//!                          [--theme light|dark] [--log FILE]
//!
//! The downloadable `remoteman-host-setup-VERSION.exe` is this program with an archive appended
//! (src/archive.rs): the Windows App Runtime its pages need, and the files it installs. The
//! uninstaller, `uninstall.exe` in the install folder, is this program with the runtime only.
//!
//! One run is three processes of this one exe, so that the pages draw on a PC that never
//! installed the Windows App Runtime and nothing elevated loads code from a folder the user can
//! write to:
//!
//! 1. **the starter** (src/launch.rs), what was opened: asks Windows to run the same file
//!    elevated (the one UAC prompt; with `--dry-run`, not elevated), waits, and returns its exit
//!    code. It stays unelevated, so it is what opens Remoteman Host afterwards, as the user.
//!    Run from the install folder (the uninstaller), it first copies itself out of it.
//! 2. **the elevated part** (`--stage2`, src/launch.rs): unpacks the runtime and a copy of the
//!    bare exe into a new folder that only Administrators and SYSTEM can write to, runs the
//!    pages from there and waits; deletes the folder after. With `--silent` it runs the steps
//!    itself, with no pages and no runtime.
//! 3. **the pages** (`--ui --source FILE`, src/ui.rs): WinUI 3 through windows-reactor, the
//!    runtime beside it (the manifest `windows-reactor-setup` embeds finds it there), the steps
//!    (src/engine.rs) on a thread of their own, the files read from FILE.
//!
//! `--dry-run` shows every page and walks every step without elevation and without changing
//! the system: what would be done is logged, and the files are unpacked into
//! `%TEMP%\remoteman-host-setup-dry-run` instead of Program Files. `--fail-at STEP` makes a
//! step fail there (stop, files, register, setup, app; uninstalling: stop, setup, files,
//! entries). `--silent` has no pages and ends with 0 (done) or 1 (failed). The log is
//! `%TEMP%\remoteman-host-setup.log` unless `--log` says otherwise; the install's own lines also
//! go to `setup.log` in the install folder, beside setup.ps1's.
//!
//! Also, for the build (scripts/win-host-setup.ps1):
//!
//!     remoteman-host-setup --pack --runtime DIR --payload DIR --version V --out FILE

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod archive;
#[cfg(windows)]
mod engine;
#[cfg(windows)]
mod launch;
#[cfg(windows)]
mod log;
#[cfg(windows)]
mod system;
#[cfg(windows)]
mod ui;

#[cfg(not(windows))]
fn main() {
    eprintln!("remoteman-host-setup installs remoteman host on Windows; it runs only there");
    std::process::exit(1);
}

/// What the command line asked for.
#[cfg(windows)]
#[derive(Clone, Debug, Default)]
pub struct Args {
    pub uninstall: bool,
    pub silent: bool,
    pub dry_run: bool,
    pub fail_at: Option<String>,
    pub theme: Option<String>,
    pub log: Option<std::path::PathBuf>,
    /// The elevated part (2 above).
    pub stage2: bool,
    /// The pages (3 above).
    pub ui: bool,
    /// The file the pages read the files to install from.
    pub source: Option<std::path::PathBuf>,
    /// The elevated part runs from a copy the starter made (out of the install folder), which
    /// it deletes when it is done.
    pub temp_copy: bool,
    pub pack: bool,
    pub runtime: Option<std::path::PathBuf>,
    pub payload: Option<std::path::PathBuf>,
    pub version: Option<String>,
    pub out: Option<std::path::PathBuf>,
}

#[cfg(windows)]
impl Args {
    pub fn parse(mut args: impl Iterator<Item = String>) -> Args {
        let mut a = Args::default();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--uninstall" => a.uninstall = true,
                "--silent" => a.silent = true,
                "--dry-run" => a.dry_run = true,
                "--fail-at" => a.fail_at = args.next(),
                "--theme" => a.theme = args.next(),
                "--log" => a.log = args.next().map(Into::into),
                "--stage2" => a.stage2 = true,
                "--ui" => a.ui = true,
                "--source" => a.source = args.next().map(Into::into),
                "--temp-copy" => a.temp_copy = true,
                "--pack" => a.pack = true,
                "--runtime" => a.runtime = args.next().map(Into::into),
                "--payload" => a.payload = args.next().map(Into::into),
                "--version" => a.version = args.next(),
                "--out" => a.out = args.next().map(Into::into),
                _ => {}
            }
        }
        a
    }

    /// The options that pass from one process to the next.
    pub fn forwarded(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.uninstall {
            out.push("--uninstall".into());
        }
        if self.silent {
            out.push("--silent".into());
        }
        if self.dry_run {
            out.push("--dry-run".into());
        }
        if let Some(step) = &self.fail_at {
            out.extend(["--fail-at".into(), step.clone()]);
        }
        if let Some(theme) = &self.theme {
            out.extend(["--theme".into(), theme.clone()]);
        }
        out.extend(["--log".into(), log::path().to_string_lossy().into_owned()]);
        out
    }
}

#[cfg(windows)]
fn main() {
    let args = Args::parse(std::env::args().skip(1));
    // Nothing loaded by name later comes from the folder the exe is in (Downloads, say), only
    // from System32. Not in the pages, whose folder is their own and holds the runtime.
    if !args.ui {
        system::safe_dll_search();
    }
    log::init(args.log.clone());
    // With no console, a panic would otherwise leave nothing behind.
    std::panic::set_hook(Box::new(|info| log::say(&format!("panic: {info}"))));

    let code = if args.pack {
        archive::pack_command(&args)
    } else if args.ui {
        ui::run(&args)
    } else if args.stage2 {
        launch::stage2(&args)
    } else {
        launch::starter(&args)
    };
    std::process::exit(code);
}
