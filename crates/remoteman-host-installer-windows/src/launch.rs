//! The starter and the elevated part (src/lib.rs says how a run is three processes). The
//! viewer's installer is never elevated: its starter is its "elevated part" itself, as an
//! elevated host installer's is.

use std::path::{Path, PathBuf};

use crate::archive::Archive;
use crate::engine::{self, Ctx, Event};
use crate::ui::{EXIT_CANCELLED, EXIT_DONE, EXIT_FAILED, EXIT_OPEN_APP, EXIT_START_APP};
use crate::{Args, Product, log, product, system};

/// How the starter hands over to the elevated part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handover {
    /// In this process: it is elevated already, or it is a dry run.
    Here,
    /// To a copy of this exe outside the install folder, started as this process is (no
    /// shell, no prompt), and waited for.
    Copy,
    /// Through the shell with this verb: `runas`, the UAC prompt.
    Shell(&'static str),
}

/// Asks Windows for elevation only when this process hasn't got it and needs it (`enough`: it
/// is elevated, or it is the viewer's installer, which needs no elevation). Run from the install
/// folder (`inside`, the uninstaller), the rest always runs from a copy outside it.
pub fn handover(enough: bool, dry_run: bool, inside: bool) -> Handover {
    match (enough || dry_run, inside) {
        (false, _) => Handover::Shell("runas"),
        (true, false) => Handover::Here,
        (true, true) => Handover::Copy,
    }
}

/// What the user opened: hands over to the elevated part, waits, and does what is left to do
/// as the user (open Remoteman Host). Its exit code: 0 done, 1 failed, 2 cancelled.
pub fn starter(args: &Args) -> i32 {
    let me = std::env::current_exe().expect("this exe");
    let install_dir = system::install_dir();
    // In a dry run, the dry run's install folder counts as the install folder, so that its
    // uninstaller copies itself out as the real one does.
    let inside = me.parent().is_some_and(|d| system::same_path(d, &install_dir) || (args.dry_run && system::same_path(d, &engine::dry_run_dir())));
    log::say(&format!(
        "started: {} {}{}",
        me.display(),
        args.forwarded().join(" "),
        if inside { " (from the install folder)" } else { "" }
    ));
    let fail = |text: &str| -> i32 {
        log::say(text);
        if !args.silent {
            system::message(text);
        }
        EXIT_FAILED
    };
    if let Err(e) = Archive::open(&me) {
        return fail(&format!("This file can't install or remove {}: {e}", product().name()));
    }

    let elevated = system::is_elevated();
    let how = handover(elevated || !product().per_machine(), args.dry_run, inside);
    log::say(&format!("elevated: {elevated}; the elevated part runs {how:?}"));

    let code = if how == Handover::Here {
        // Elevated already (or a dry run): this process is the elevated part. No prompt, no
        // other process, so it runs where nobody could answer one (SYSTEM with nobody signed in).
        stage2(args)
    } else {
        // Run from the install folder (the uninstaller), the rest runs from a copy outside it,
        // so that the folder can be deleted, and nothing here keeps it open.
        let mut copy_dir = None;
        let target = if inside {
            let temp = std::env::temp_dir();
            let _ = std::env::set_current_dir(&temp);
            let dir = match system::private_dir(elevated && !args.dry_run) {
                Ok(d) => d,
                Err(e) => return fail(&format!("{} couldn't make a folder in Temp: {e}", product().setup_name())),
            };
            let copy = dir.join(format!("{}.exe", product().setup_name()));
            if let Err(e) = std::fs::copy(&me, &copy) {
                return fail(&format!("{} couldn't copy itself to {}: {e}", product().setup_name(), copy.display()));
            }
            copy_dir = Some(dir);
            copy
        } else {
            me.clone()
        };

        let mut child_args = vec!["--stage2".to_string()];
        child_args.extend(args.forwarded());
        if inside {
            child_args.push("--temp-copy".into());
        }
        system::allow_foreground();
        let cwd = target.parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
        let child = match how {
            Handover::Shell(verb) => match system::shell_start(&target, &child_args, verb, &cwd) {
                Ok(system::Launched::Process(p)) => p,
                Ok(system::Launched::Cancelled) => {
                    log::say("the UAC prompt was answered No: nothing was done");
                    if let Some(dir) = &copy_dir {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    return if args.silent { EXIT_FAILED } else { EXIT_CANCELLED };
                }
                Err(e) => return fail(&format!("{} couldn't start: {e}", product().setup_name())),
            },
            // As this process is (elevated, or a dry run), with no shell between.
            _ => match system::spawn(&target, &child_args, &cwd) {
                Ok(p) => p,
                Err(e) => return fail(&format!("{} couldn't start: {e}", product().setup_name())),
            },
        };

        // The uninstaller, opened from Settings, doesn't wait: it would keep the folder open. A
        // silent one does, for its exit code (the elevated part deletes it at restart instead).
        if inside && !args.silent {
            log::say("handed over; the starter ends");
            return EXIT_DONE;
        }
        let code = child.wait() as i32;
        drop(child);
        log::say(&format!("the elevated part ended: {code}"));
        if let Some(dir) = &copy_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
        code
    };
    match code {
        EXIT_OPEN_APP => {
            open_app(&install_dir, false, args.dry_run);
            EXIT_DONE
        }
        EXIT_START_APP => {
            open_app(&install_dir, true, args.dry_run);
            EXIT_DONE
        }
        EXIT_DONE | EXIT_CANCELLED => code,
        _ => EXIT_FAILED,
    }
}

/// Remoteman Host, started as the user (this process is not elevated), so that its icon is there
/// now and not only from the next sign-in: its window (Open Remoteman Host), or in the
/// background as at sign-in (Close). For the viewer's installer: the viewer (Open remoteman).
fn open_app(install_dir: &Path, background: bool, dry_run: bool) {
    let exe = engine::app_exe(install_dir);
    let background = background && product() == Product::Host;
    let args: &[&str] = if background { &["--background"] } else { &[] };
    if dry_run {
        log::say(&format!("would start {} {}", exe.display(), args.join(" ")));
        return;
    }
    if !exe.exists() {
        log::say(&format!("{} isn't there", exe.display()));
        return;
    }
    let started = if !system::is_elevated() {
        system::start(&exe, args)
    } else if !background {
        // Run as an administrator (from an elevated prompt, say), what is started here would be
        // elevated too; Explorer starts it as the signed-in user instead (with no arguments, so
        // its window opens).
        let explorer = system::windows_dir().join("explorer.exe");
        system::start(&explorer, &[&exe.to_string_lossy()])
    } else {
        // An elevated silent install (a deployment tool, SYSTEM perhaps): nobody's app to start.
        log::say("not started from an elevated starter: it starts at the next sign-in");
        return;
    };
    log::say(&format!("started {} {}: {started:?}", exe.display(), args.join(" ")));
}

/// The elevated part: the pages from a private folder with the runtime beside them, or with
/// `--silent` the steps here. Its exit code is the pages' (src/ui.rs).
pub fn stage2(args: &Args) -> i32 {
    let me = std::env::current_exe().expect("this exe");
    let elevated = system::is_elevated();
    log::say(&format!("elevated part: {} (elevated: {elevated})", me.display()));
    let code = stage2_inner(args, &me, elevated);
    if args.temp_copy {
        // The copy the starter made out of the install folder goes too.
        if let Some(dir) = me.parent() {
            system::delete_after_exit(dir);
        }
    }
    log::say(&format!("elevated part ends: {code}"));
    code
}

fn stage2_inner(args: &Args, me: &Path, elevated: bool) -> i32 {
    let fail = |text: &str| -> i32 {
        log::say(text);
        if !args.silent {
            system::message(text);
        }
        EXIT_FAILED
    };
    if !args.dry_run && !elevated && product().per_machine() {
        return fail(&format!("{} setup needs an administrator, and Windows didn't give it one.", product().name()));
    }
    if !system::single_instance() {
        return fail(&format!("{} setup is already running.", product().name()));
    }

    if args.silent {
        let ctx = Ctx::new(args, me.to_path_buf());
        let archive = Archive::open(me).ok();
        let facts = engine::facts(&ctx, archive.as_ref());
        if ctx.mode == engine::Mode::Uninstall && facts.installed.is_none() {
            log::say(&format!("{} isn't installed: nothing to remove", product().name()));
            return EXIT_DONE;
        }
        let mut emit = |e: Event| {
            if let Event::Detail(_, line) = &e {
                log::say(&format!("  {line}"));
            }
        };
        let outcome = engine::run(&ctx, &facts, &mut emit);
        return match (outcome.ok, facts.has_app && ctx.mode == engine::Mode::Install) {
            (true, true) => EXIT_START_APP,
            (true, false) => EXIT_DONE,
            (false, _) => EXIT_FAILED,
        };
    }

    // The pages, with the Windows App Runtime beside them, in a folder nobody unelevated can
    // write to, so that no DLL in it is anyone else's.
    let archive = match Archive::open(me) {
        Ok(a) => a,
        Err(e) => return fail(&e.to_string()),
    };
    let dir = match system::private_dir(elevated) {
        Ok(d) => d,
        Err(e) => return fail(&format!("{} couldn't make its folder: {e}", product().setup_name())),
    };
    let started = std::time::Instant::now();
    let exe = dir.join(format!("{}.exe", product().setup_name()));
    let unpacked = archive.unpack(&archive.index.ui, &dir, &mut |_| {}).and_then(|files| archive.write_exe(&exe).map(|()| files));
    let code = match unpacked {
        Err(e) => fail(&format!("{} couldn't unpack itself into {}: {e}", product().setup_name(), dir.display())),
        Ok(files) => {
            log::say(&format!("unpacked the pages' runtime into {}: {} files in {:.1} s", dir.display(), files.len(), started.elapsed().as_secs_f64()));
            let mut ui_args = vec!["--ui".to_string(), "--source".into(), me.to_string_lossy().into_owned()];
            ui_args.extend(args.forwarded());
            system::allow_foreground();
            match std::process::Command::new(&exe).args(&ui_args).current_dir(&dir).status() {
                Ok(status) => status.code().unwrap_or(EXIT_FAILED),
                Err(e) => fail(&format!("{} couldn't open its window: {e}", product().setup_name())),
            }
        }
    };
    remove_soon(&dir);
    code
}

/// Deletes a folder whose files a process that just ended may still hold for a moment.
fn remove_soon(dir: &PathBuf) {
    for _ in 0..25 {
        if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    log::say(&format!("{} couldn't be deleted now; it goes later", dir.display()));
    system::delete_after_exit(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Already elevated (an elevated prompt, a deployment tool, SYSTEM from a scheduled task with
    /// nobody signed in): no `runas`, which as SYSTEM never comes back, and no other process
    /// unless the folder must be left. Only an unelevated real run asks.
    #[test]
    fn elevation_is_asked_for_only_when_it_is_missing() {
        assert_eq!(handover(true, false, false), Handover::Here);
        assert_eq!(handover(true, false, true), Handover::Copy);
        assert_eq!(handover(false, false, false), Handover::Shell("runas"));
        assert_eq!(handover(false, false, true), Handover::Shell("runas"));
        // A dry run walks the elevated path without elevation.
        assert_eq!(handover(false, true, false), Handover::Here);
        assert_eq!(handover(false, true, true), Handover::Copy);
        assert_eq!(handover(true, true, false), Handover::Here);
    }
}
