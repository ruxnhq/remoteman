//! remoteman for Windows: the app. A Win32 window with an interface drawn by ourselves
//! (Direct2D and DirectWrite, no toolkit), Direct3D 11, FFmpeg's D3D11VA decoders on the same
//! device, and input sent back.
//!
//!   remoteman.exe                      the list of computers
//!   remoteman.exe HOST:PORT [--pair] [--state DIR] [--size WxH] [--refresh HZ]
//!                 [--decoders av1,hevc] [--seconds N] [--fullscreen] [--sound]
//!                                      straight into a session with that computer
//! and, for testing with nobody looking:
//!   --shot FILE.png      a few seconds into the picture, save what is about to be presented
//!   --stats              one line a second
//!   --input-demo         move the pointer, click, type and scroll
//!   --log FILE           what is printed, in a file as well
//!   --ui STEPS --shots DIR [--dark|--light]
//!                        pictures of the screens named (`walk.rs`; `all` for every one), with
//!                        a made-up list and no network
//!   --profiles           the card's Direct3D 11 decoder profiles, and leave
//!   --vrr-probe SECONDS  the variable refresh control (`probe.rs`), and leave
//! (the engine also takes --watch-vblank, --screen-hdr and --play with its --play-* words)
//!
//! Who does what:
//! - the main thread owns the window and the app's state: what is on the window, what each
//!   press does, input, the cursor (`window.rs`, `app.rs`, and the screens in `pages.rs`,
//!   `session.rs` and `menus.rs`, built from the kit in `ui/`);
//! - the core's threads hand frames to the decode thread (`stream.rs`, `decode.rs`);
//! - the draw thread waits for the display and presents the newest picture with the
//!   interface over it (`draw.rs`).
//!
//! Pictures stay on the GPU from the decoder to the screen.

// No console window of its own; what it says goes to the console it was started from, if any.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("remoteman-viewer-windows only runs on Windows; the Linux viewer is remoteman-viewer");
}

#[cfg(windows)]
#[macro_use]
mod log;
#[cfg(windows)]
mod app;
#[cfg(windows)]
mod camera;
#[cfg(windows)]
mod args;
#[cfg(windows)]
mod creds;
#[cfg(windows)]
mod cursor;
#[cfg(windows)]
mod decode;
mod cuda;
#[cfg(windows)]
mod demo;
#[cfg(windows)]
mod draw;
#[cfg(windows)]
mod features;
#[cfg(windows)]
mod gpu;
#[cfg(windows)]
mod keys;
#[cfg(windows)]
mod media;
#[cfg(windows)]
mod menus;
#[cfg(windows)]
mod monitor;
#[cfg(windows)]
mod mic;
#[cfg(windows)]
mod nearby;
#[cfg(windows)]
mod pages;
#[cfg(windows)]
mod probe;
#[cfg(windows)]
mod session;
#[cfg(windows)]
mod shot;
#[cfg(windows)]
mod sound;
#[cfg(windows)]
mod status;
#[cfg(windows)]
#[allow(dead_code)] // The engine's alone (`engine::touch`).
mod touch;
#[cfg(windows)]
mod store;
#[cfg(windows)]
mod stream;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod walk;
#[cfg(windows)]
mod window;

#[cfg(windows)]
fn main() {
    // SAFETY: attaching to the console of whoever started us, if there is one.
    unsafe {
        let _ = windows::Win32::System::Console::AttachConsole(windows::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            say!("failed: {e:#}");
            2
        }
    };
    // Not a return: nothing still running (a stuck decoder, the core's threads) may keep the
    // process alive.
    std::process::exit(code);
}

/// Runs the app from start to end; returns what the process ends with: 0 unless a session
/// named on the command line showed no picture (1) or something failed (2).
#[cfg(windows)]
fn run() -> anyhow::Result<i32> {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    let args = args::parse()?;
    if let Some(path) = &args.log {
        log::to_file(path)?;
    }
    if let Some(seconds) = args.seconds {
        // The last resort: whatever hangs, the process ends a little after it was told to.
        std::thread::Builder::new().name("deadline".into()).spawn(move || {
            std::thread::sleep(Duration::from_secs(seconds + 8));
            say!("still running 8 s after --seconds was up: leaving the hard way");
            std::process::exit(3);
        })?;
    }

    let gpu = gpu::Gpu::open()?;
    say!("gpu: {}", gpu.name);
    if args.profiles {
        for line in gpu.profiles()? {
            say!("decoder profile: {line}");
        }
        return Ok(0);
    }
    if let Some(seconds) = args.vrr_probe {
        probe::run(&gpu, seconds)?;
        return Ok(0);
    }
    let mut decoders = Vec::new();
    // Any maker's card that decodes through Direct3D 11 video does; what a card doesn't
    // decode is decoded on the CPU, the last resort (REMOTEMAN_SOFTWARE=1 does so for all of
    // it, to try that way on a card that needs none of it).
    let forced = std::env::var("REMOTEMAN_SOFTWARE").as_deref() == Ok("1");
    for mode in &args.decoders {
        let on_card = if forced { Err(anyhow::anyhow!("REMOTEMAN_SOFTWARE=1")) } else { gpu.decodes(*mode) };
        match on_card {
            Ok(()) => decoders.push(*mode),
            // 4:4:4 on the CPU leaves the codec's 4:2:0 where it was.
            Err(e) if mode.chroma == remoteman_core::Chroma::C444 => match decode::can_software(mode.codec) {
                Ok(()) => {
                    say!("decoder {:?} 4:4:4: {e:#}; on the CPU", mode.codec);
                    decoders.push(*mode);
                }
                Err(soft) => say!("decoder {:?} 4:4:4 left out: {e:#}; {soft:#}", mode.codec),
            },
            Err(e) => match decode::use_software(mode.codec) {
                Ok(()) => {
                    say!("decoder {:?}: not on this card ({e:#}); on the CPU instead", mode.codec);
                    decoders.push(*mode);
                }
                Err(soft) => say!("decoder {:?} left out: {e:#}; {soft:#}", mode.codec),
            },
        }
    }
    // What the card decodes is offered first.
    decoders.sort_by_key(|m| decode::is_software(m.codec));
    // With none, the app still opens: the list, the settings and the reason are worth showing.
    say!("decoders offered: {}", decoders.iter().map(|m| format!("{:?}{}", m.codec, if m.chroma == remoteman_core::Chroma::C444 { "444" } else { "" }).to_lowercase()).collect::<Vec<_>>().join(", "));
    anyhow::ensure!(!decoders.is_empty() || args.host.is_none(), "this card decodes none of what was asked for");

    let scene = Arc::new(stream::Scene::default());
    let dark = args.dark.unwrap_or_else(window::system_dark);
    let shot = args.shot.clone();
    let app = app::App::new(args, gpu.clone(), scene.clone(), decoders, dark)?;
    let hwnd = window::create(app)?;
    let drawing = draw::start(gpu, scene.clone(), hwnd, shot)?;
    window::run();
    scene.quit.store(true, Ordering::Relaxed);
    scene.poke();
    let _ = drawing.join();

    let Some(app) = window::take() else { return Ok(2) };
    if let Some(s) = &app.summary {
        say!(
            "in {:.1} s: decoded {} ({:.1} a second), presented {} ({:.1} a second), replaced unseen {}, lost {}, keyframe requests {}, round trip {:.1} ms, {}",
            s.seconds,
            s.decoded,
            s.decoded as f64 / s.seconds.max(0.001),
            s.shown,
            s.shown as f64 / s.seconds.max(0.001),
            s.replaced,
            s.lost,
            s.keyframe_requests,
            s.round_trip_ms,
            s.decoder,
        );
        if let Some(reason) = &s.ended {
            say!("the session ended: {reason}");
        }
    }
    Ok(app.code.unwrap_or(0))
}
