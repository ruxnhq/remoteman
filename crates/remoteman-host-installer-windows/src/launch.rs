//! The starter and the elevated part (src/main.rs says how a run is three processes).

use std::path::{Path, PathBuf};

use crate::archive::Archive;
use crate::engine::{self, Ctx, Event};
use crate::ui::{EXIT_CANCELLED, EXIT_DONE, EXIT_FAILED, EXIT_OPEN_APP, EXIT_RESTART_APP};
use crate::{Args, log, system};

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
        return fail(&format!("This file can't install or remove remoteman host: {e}"));
    }

    // Run from the install folder (the uninstaller), the rest runs from a copy outside it, so
    // that the folder can be deleted, and nothing here keeps it open.
    let mut copy_dir = None;
    let target = if inside {
        let temp = std::env::temp_dir();
        let _ = std::env::set_current_dir(&temp);
        let dir = match system::private_dir(false) {
            Ok(d) => d,
            Err(e) => return fail(&format!("remoteman host setup couldn't make a folder in Temp: {e}")),
        };
        let copy = dir.join("remoteman-host-setup.exe");
        if let Err(e) = std::fs::copy(&me, &copy) {
            return fail(&format!("remoteman host setup couldn't copy itself to {}: {e}", copy.display()));
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
    let verb = if args.dry_run { "open" } else { "runas" };
    system::allow_foreground();
    let cwd = target.parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
    let child = match system::shell_start(&target, &child_args, verb, &cwd) {
        Ok(system::Launched::Process(p)) => p,
        Ok(system::Launched::Cancelled) => {
            log::say("the UAC prompt was answered No: nothing was done");
            if let Some(dir) = &copy_dir {
                let _ = std::fs::remove_dir_all(dir);
            }
            return if args.silent { EXIT_FAILED } else { EXIT_CANCELLED };
        }
        Err(e) => return fail(&format!("remoteman host setup couldn't start: {e}")),
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
    match code {
        EXIT_OPEN_APP => {
            open_app(&install_dir, false, args.dry_run);
            EXIT_DONE
        }
        EXIT_RESTART_APP => {
            open_app(&install_dir, true, args.dry_run);
            EXIT_DONE
        }
        EXIT_DONE | EXIT_CANCELLED => code,
        _ => EXIT_FAILED,
    }
}

/// Remoteman Host, started as the user (this process is not elevated): its window, or (it was
/// running before the install stopped it) in the background as at sign-in.
fn open_app(install_dir: &Path, background: bool, dry_run: bool) {
    let exe = install_dir.join(r"app\remoteman-host-app.exe");
    let args: &[&str] = if background { &["--background"] } else { &[] };
    if dry_run {
        log::say(&format!("would start {} {}", exe.display(), args.join(" ")));
        return;
    }
    if !exe.exists() {
        log::say(&format!("{} isn't there", exe.display()));
        return;
    }
    let started = if system::is_elevated() {
        // Run as an administrator (from an elevated prompt, say), what is started here would be
        // elevated too; Explorer starts it as the signed-in user instead. With no arguments, so
        // the window opens either way.
        let explorer = system::windows_dir().join("explorer.exe");
        system::start(&explorer, &[&exe.to_string_lossy()])
    } else {
        system::start(&exe, args)
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
    if !args.dry_run && !elevated {
        return fail("remoteman host setup needs an administrator, and Windows didn't give it one.");
    }
    if !system::single_instance() {
        return fail("remoteman host setup is already running.");
    }

    if args.silent {
        let ctx = Ctx::new(args, me.to_path_buf());
        let archive = Archive::open(me).ok();
        let facts = engine::facts(&ctx, archive.as_ref());
        if ctx.mode == engine::Mode::Uninstall && facts.installed.is_none() {
            log::say("remoteman host isn't installed: nothing to remove");
            return EXIT_DONE;
        }
        let mut emit = |e: Event| {
            if let Event::Detail(_, line) = &e {
                log::say(&format!("  {line}"));
            }
        };
        let outcome = engine::run(&ctx, &facts, &mut emit);
        return match (outcome.ok, outcome.app_was_running && facts.has_app && ctx.mode == engine::Mode::Install) {
            (true, true) => EXIT_RESTART_APP,
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
        Err(e) => return fail(&format!("remoteman host setup couldn't make its folder: {e}")),
    };
    let started = std::time::Instant::now();
    let exe = dir.join("remoteman-host-setup.exe");
    let unpacked = archive.unpack(&archive.index.ui, &dir, &mut |_| {}).and_then(|files| archive.write_exe(&exe).map(|()| files));
    let code = match unpacked {
        Err(e) => fail(&format!("remoteman host setup couldn't unpack itself into {}: {e}", dir.display())),
        Ok(files) => {
            log::say(&format!("unpacked the pages' runtime into {}: {} files in {:.1} s", dir.display(), files.len(), started.elapsed().as_secs_f64()));
            let mut ui_args = vec!["--ui".to_string(), "--source".into(), me.to_string_lossy().into_owned()];
            ui_args.extend(args.forwarded());
            system::allow_foreground();
            match std::process::Command::new(&exe).args(&ui_args).current_dir(&dir).status() {
                Ok(status) => status.code().unwrap_or(EXIT_FAILED),
                Err(e) => fail(&format!("remoteman host setup couldn't open its window: {e}")),
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
