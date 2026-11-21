//! remoteman host's installer and uninstaller for Windows, one program:
//!
//!     remoteman-host-setup [--uninstall] [--silent] [--dry-run] [--fail-at STEP]
//!                          [--theme light|dark] [--log FILE]
//!                          [--trust FILE] [--encoder NAME] [--no-pair]
//!
//! The downloadable `remoteman-host-setup-VERSION.exe` is this program with an archive appended
//! (src/archive.rs): the Windows App Runtime its pages need, and the files it installs. The
//! uninstaller, `uninstall.exe` in the install folder, is this program with the runtime only.
//!
//! For an install nobody watches (a deployment tool, a scheduled task as SYSTEM):
//! `--trust FILE` installs FILE as the host's trust list (one line per viewer: the SHA-256 of
//! its certificate in hex, a space, its name) before the service starts; `--encoder NAME` starts
//! the service's host with `--encoder NAME` (auto, nvenc, mf, d3d12); `--no-pair` starts it with
//! `--no-pair`, so that nobody new can be let in (no `--ask`, and `ctl pair on` refused). Each
//! install sets the service's options from its own command line. All three are checked before
//! anything is done: a trust list that can't be read or has no fingerprint, or an encoder that
//! isn't one of those, ends the run with 1 at once. Installing over an install replaces the
//! files and keeps the host's state, its identity above all, so its fingerprint stays the same.
//!
//! One run is three processes of this one exe, so that the pages draw on a PC that never
//! installed the Windows App Runtime and nothing elevated loads code from a folder the user can
//! write to:
//!
//! 1. **the starter** (src/launch.rs), what was opened: asks Windows to run the same file
//!    elevated (the one UAC prompt), waits, and returns its exit code. It stays unelevated, so
//!    it is what opens Remoteman Host afterwards, as the user. Run from the install folder (the
//!    uninstaller), it first copies itself out of it. Already elevated (an elevated prompt, a
//!    deployment tool, SYSTEM), or in a dry run, it asks nothing of Windows: it is the elevated
//!    part itself (or, from the install folder, starts its copy as it is), so that it also runs
//!    where nobody could answer a prompt (`launch::handover`).
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
//! entries). `--silent` has no pages and no message boxes, and ends with 0 (done) or 1 (failed).
//! The log is `remoteman-host-setup.log` in the Temp folder of whoever runs it unless `--log`
//! says otherwise; the install's own lines also go to `setup.log` in the install folder, beside
//! setup.ps1's.
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
mod steam;
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
    /// The host's trust list, installed before the service starts (named in full).
    pub trust: Option<std::path::PathBuf>,
    /// The service's host's `--encoder`.
    pub encoder: Option<String>,
    /// The service's host started with `--no-pair`.
    pub no_pair: bool,
    /// What was wrong with the command line (a value missing), acted on by `check`.
    pub errors: Vec<String>,
}

/// The encoders remoteman-host.exe's `--encoder` takes (crates/remoteman-host-windows/src/
/// main.rs; setup.ps1 has them too): checked here, before anything is stopped or replaced.
#[cfg(windows)]
pub const ENCODERS: &[&str] = &["auto", "nvenc", "mf", "d3d12"];

#[cfg(windows)]
impl Args {
    pub fn parse(mut args: impl Iterator<Item = String>) -> Args {
        let mut a = Args::default();
        while let Some(arg) = args.next() {
            let mut value = |a: &mut Args| {
                let v = args.next();
                if v.is_none() {
                    a.errors.push(format!("{arg} needs a value"));
                }
                v
            };
            match arg.as_str() {
                "--uninstall" => a.uninstall = true,
                "--silent" => a.silent = true,
                "--dry-run" => a.dry_run = true,
                "--fail-at" => a.fail_at = value(&mut a),
                "--theme" => a.theme = value(&mut a),
                "--log" => a.log = value(&mut a).map(Into::into),
                "--stage2" => a.stage2 = true,
                "--ui" => a.ui = true,
                "--source" => a.source = value(&mut a).map(Into::into),
                "--temp-copy" => a.temp_copy = true,
                "--pack" => a.pack = true,
                "--runtime" => a.runtime = value(&mut a).map(Into::into),
                "--payload" => a.payload = value(&mut a).map(Into::into),
                "--version" => a.version = value(&mut a),
                "--out" => a.out = value(&mut a).map(Into::into),
                "--trust" => a.trust = value(&mut a).map(Into::into),
                "--encoder" => a.encoder = value(&mut a),
                "--no-pair" => a.no_pair = true,
                _ => {}
            }
        }
        // The processes after this one run in another folder: a trust list named from here is
        // named in full.
        if let Some(trust) = &a.trust
            && let Ok(full) = std::path::absolute(trust)
        {
            a.trust = Some(full);
        }
        a
    }

    /// The install's own options, checked before anything is done: the command line complete,
    /// the trust list readable with at least one fingerprint and nothing else in it, the
    /// encoder one the host takes. What is wrong, in a line.
    pub fn check(&self) -> Result<(), String> {
        if let Some(e) = self.errors.first() {
            return Err(format!("remoteman host setup: {e}"));
        }
        if self.uninstall && (self.trust.is_some() || self.encoder.is_some() || self.no_pair) {
            return Err("remoteman host setup: --trust, --encoder and --no-pair are for installing".into());
        }
        if let Some(encoder) = &self.encoder
            && !ENCODERS.contains(&encoder.as_str())
        {
            return Err(format!("remoteman host setup: --encoder {encoder}: it is one of {}", ENCODERS.join(", ")));
        }
        if let Some(trust) = &self.trust {
            let said = |e: String| format!("remoteman host setup: the trust list {}: {e}", trust.display());
            let bytes = std::fs::read(trust).map_err(|e| said(e.to_string()))?;
            let text = String::from_utf8(bytes).map_err(|_| said("it isn't UTF-8 text".into()))?;
            check_trust(&text).map_err(said)?;
        }
        Ok(())
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
        if let Some(trust) = &self.trust {
            out.extend(["--trust".into(), trust.to_string_lossy().into_owned()]);
        }
        if let Some(encoder) = &self.encoder {
            out.extend(["--encoder".into(), encoder.clone()]);
        }
        if self.no_pair {
            out.push("--no-pair".into());
        }
        out.extend(["--log".into(), log::path().to_string_lossy().into_owned()]);
        out
    }
}

/// A trust list as the host reads it (crates/remoteman-transport/src/identity.rs, `Trust`):
/// each line not empty is 64 hex digits (a viewer's certificate's SHA-256), then a space and its
/// name, which may be left out. How many fingerprints, or what is wrong; none is wrong too, as
/// a host with no one to trust and no pairing could never be reached. setup.ps1 checks it again
/// and writes the fingerprints in lower case.
#[cfg(windows)]
pub fn check_trust(text: &str) -> Result<usize, String> {
    let mut n = 0;
    for (i, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fp = line.split_whitespace().next().unwrap_or_default();
        if fp.len() != 64 || !fp.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("line {} isn't a fingerprint (64 hex digits) and a name: {line}", i + 1));
        }
        n += 1;
    }
    if n == 0 {
        return Err("it has no fingerprints".into());
    }
    Ok(n)
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
    } else if let Err(e) = args.check() {
        log::say(&e);
        if !args.silent {
            system::message(&e);
        }
        ui::EXIT_FAILED
    } else if args.ui {
        ui::run(&args)
    } else if args.stage2 {
        launch::stage2(&args)
    } else {
        launch::starter(&args)
    };
    std::process::exit(code);
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;

    fn parse(line: &str) -> Args {
        Args::parse(line.split_whitespace().map(str::to_string))
    }

    const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn unattended_options_are_read_and_passed_on() {
        let a = parse("--silent --trust trusted.txt --encoder d3d12 --no-pair --log C:\\x.log");
        assert!(a.silent && a.no_pair);
        assert_eq!(a.encoder.as_deref(), Some("d3d12"));
        // Named in full, for the processes that run in other folders.
        let trust = a.trust.clone().unwrap();
        assert!(trust.is_absolute(), "{}", trust.display());
        assert!(trust.ends_with("trusted.txt"));
        assert!(a.errors.is_empty());
        let passed = a.forwarded();
        let at = |w: &str| passed.iter().position(|p| p == w);
        assert_eq!(passed[at("--encoder").unwrap() + 1], "d3d12");
        assert_eq!(passed[at("--trust").unwrap() + 1], trust.to_string_lossy());
        assert!(at("--no-pair").is_some() && at("--silent").is_some());
        // And read back the same by the next process.
        let again = Args::parse(passed.into_iter());
        assert_eq!((again.trust, again.encoder, again.no_pair, again.silent), (Some(trust), Some("d3d12".into()), true, true));
    }

    #[test]
    fn a_missing_value_or_a_wrong_option_stops_the_run() {
        assert!(parse("--silent --trust").check().unwrap_err().contains("--trust needs a value"));
        assert!(parse("--encoder").check().unwrap_err().contains("--encoder needs a value"));
        assert!(parse("--encoder x264").check().unwrap_err().contains("auto, nvenc, mf, d3d12"));
        assert!(parse("--uninstall --no-pair").check().is_err());
        assert!(parse("--silent --trust C:\\nowhere\\at-all.txt").check().unwrap_err().contains("at-all.txt"));
        for name in ENCODERS {
            assert!(parse(&format!("--encoder {name} --no-pair")).check().is_ok(), "{name}");
        }
        assert!(parse("--silent").check().is_ok());
    }

    #[test]
    fn trust_lists_are_checked() {
        assert_eq!(check_trust(&format!("{FP} remoteman on PC\n")), Ok(1));
        assert_eq!(check_trust(&format!("\u{feff}{FP} a\r\n\r\n{} b\n{FP}\n", FP.to_uppercase())), Ok(3));
        assert!(check_trust("").unwrap_err().contains("no fingerprints"));
        assert!(check_trust("\n  \n").is_err());
        assert!(check_trust(&format!("{FP} a\nnot a fingerprint\n")).unwrap_err().contains("line 2"));
        assert!(check_trust(&format!("{} short\n", &FP[..63])).is_err());
        assert!(check_trust(&format!("{}g name\n", &FP[..63])).is_err());

        let file = std::env::temp_dir().join(format!("rm-trust-test-{}.txt", std::process::id()));
        std::fs::write(&file, format!("{FP} the viewer\n")).unwrap();
        assert!(parse(&format!("--silent --trust {}", file.display())).check().is_ok());
        std::fs::write(&file, "nothing\n").unwrap();
        assert!(parse(&format!("--silent --trust {}", file.display())).check().is_err());
        std::fs::remove_file(&file).unwrap();
    }
}
