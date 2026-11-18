//! `remoteman-host`: the Windows host. It streams a monitor of the display driver to one client
//! at a time (a new one takes over) and puts the client's input into the desktop.
//!
//!     remoteman-host [--listen ADDR:PORT] [--state DIR] [--pair] [--name NAME] [--log FILE]
//!                    [--bitrate MBIT] [--mode keep|listed|any] [--seconds N]
//!     remoteman-host --probe | --probe-picture [--log FILE]
//!
//! `--probe` only looks: it opens the display driver, says what it finds (the driver, its frame
//! channel, the monitor's modes, the adapter, the shared textures) and exits. Run it first on
//! a new machine. `--probe-picture` goes on to take one frame, convert it and encode it once
//! with each codec, to show how far the picture path gets with no client.
//!
//! `--mode` says how far the host may go to give a client its own size: `keep` streams the
//! monitor as it is, `listed` switches to the client's size if the monitor offers it, `any`
//! (the default) also has the driver add the size when it doesn't, which re-creates the monitor.
//! The monitor is put back when the client goes.
//!
//! It must run elevated (the display driver's device lets in Administrators and SYSTEM only)
//! and in the user's own desktop session (input, the cursor and mode changes work nowhere
//! else): see `input.rs`.
//!
//! The first time it listens on anything but loopback, Windows Firewall asks the user whether
//! to let it, and until the answer is "Allow" it keeps a rule that blocks it from other
//! computers. Tests on the same machine can listen on 127.0.0.1 and are never asked.
//!
//! Everything here is Windows-only; on other systems this builds to a stub so that the
//! workspace builds whole.

#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod convert;
#[cfg(windows)]
mod cursor;
#[cfg(windows)]
mod display;
#[cfg(windows)]
mod encode;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod pipeline;
#[cfg(windows)]
mod probe;
#[cfg(windows)]
mod session;
#[cfg(windows)]
mod vdd;

#[cfg(not(windows))]
fn main() {
    eprintln!("remoteman-host is the Windows host; on Linux the host is remoteman-agent");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows_main::main()
}

#[cfg(windows)]
mod windows_main {
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result};
    use remoteman_core::host::{Host, HostOptions};
    use windows::Win32::Media::timeBeginPeriod;
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    use windows::core::BOOL;

    use crate::display::{Fit, Screen};
    use crate::{probe, session};

    /// Ctrl-C, the console closing, or the user signing out.
    static STOP: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn on_console(_event: u32) -> BOOL {
        STOP.store(true, Ordering::Relaxed);
        true.into()
    }

    fn state_dir() -> PathBuf {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("remoteman")
    }

    pub fn main() -> Result<()> {
        let mut listen: SocketAddr = format!("0.0.0.0:{}", remoteman_core::PORT).parse()?;
        let (mut pair, mut state, mut bitrate, mut drop_one_in) = (false, state_dir(), 40u32, None::<u32>);
        let (mut name, mut log, mut seconds, mut probing, mut fit) = (None::<String>, None::<PathBuf>, None::<u64>, None::<bool>, Fit::Any);
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            let mut value = || args.next().with_context(|| format!("{a} needs a value"));
            match a.as_str() {
                "--listen" => listen = value()?.parse().context("--listen ADDR:PORT")?,
                "--state" => state = value()?.into(),
                "--bitrate" => bitrate = value()?.parse().context("--bitrate in Mbit/s")?,
                "--pair" => pair = true,
                "--name" => name = Some(value()?),
                "--log" => log = Some(value()?.into()),
                // For tests: stop by itself after this long.
                "--seconds" => seconds = Some(value()?.parse().context("--seconds N")?),
                "--probe" => probing = Some(false),
                "--probe-picture" => probing = Some(true),
                "--mode" => {
                    fit = match value()?.as_str() {
                        "keep" => Fit::Keep,
                        "listed" => Fit::Listed,
                        "any" => Fit::Any,
                        other => anyhow::bail!("--mode {other}: keep, listed, any"),
                    }
                }
                // For tests: lose one video packet in this many.
                "--test-drop-one-in" => drop_one_in = Some(value()?.parse().context("--test-drop-one-in N")?),
                other => anyhow::bail!("unknown option {other}"),
            }
        }

        let level = if std::env::var_os("REMOTEMAN_DEBUG").is_some() { tracing::Level::DEBUG } else { tracing::Level::INFO };
        match &log {
            // To a file, for runs nobody watches (and nothing to colour there).
            Some(path) => {
                let file = std::fs::OpenOptions::new().create(true).append(true).open(path).with_context(|| format!("--log {}", path.display()))?;
                tracing_subscriber::fmt().with_max_level(level).with_ansi(false).with_writer(std::sync::Mutex::new(file)).init();
            }
            None => tracing_subscriber::fmt().with_max_level(level).with_ansi(false).init(),
        }
        // Anything that goes wrong is in the log before the process ends.
        let result = run(listen, state, pair, name, bitrate, drop_one_in, seconds, probing, fit);
        if let Err(e) = &result {
            tracing::error!("{e:#}");
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn run(listen: SocketAddr, state: PathBuf, pair: bool, name: Option<String>, bitrate: u32, drop_one_in: Option<u32>, seconds: Option<u64>, probing: Option<bool>, fit: Fit) -> Result<()> {
        // Sizes and places in real pixels, whatever scale the desktop is drawn at: the cursor's
        // place, the monitor's corner and the pointer's target must all be in the picture's own.
        // SAFETY: plain call, before any window exists.
        if let Err(e) = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } {
            tracing::warn!("couldn't ask Windows for real pixels ({e}); on a scaled desktop the pointer will be off");
        }
        // Sleeps of a few milliseconds (the cursor's) are that long, not a whole 15 ms tick.
        // SAFETY: plain call.
        unsafe { timeBeginPeriod(1) };
        if let Some(picture) = probing {
            return probe::run(picture);
        }

        let screen = Screen::open()?;
        let name = name.or_else(|| std::env::var("COMPUTERNAME").ok()).unwrap_or_else(|| "windows".into());
        // What this host does today. `configure`: a new bitrate in the running picture, another
        // size or codec by starting the picture afresh. No sound, clipboard or files yet.
        let features = ["video", "input", "cursor", "configure"].map(String::from).to_vec();
        let options = HostOptions { listen, state, name, os: "windows".into(), features, pair, test_drop_one_in: drop_one_in };
        let host = Host::start(options, session::Host::new(screen, bitrate * 1_000_000, fit))?;
        tracing::info!("listening on {}; this host's fingerprint is {}", host.address, host.fingerprint);
        if pair {
            tracing::info!("pairing: the next unknown device to connect will be trusted");
        }

        // SAFETY: a handler that only sets a flag.
        if let Err(e) = unsafe { SetConsoleCtrlHandler(Some(on_console), true) } {
            tracing::warn!("no Ctrl-C handler ({e}); stop this process to end it");
        }
        let began = Instant::now();
        while !STOP.load(Ordering::Relaxed) && seconds.is_none_or(|s| began.elapsed() < Duration::from_secs(s)) {
            std::thread::sleep(Duration::from_millis(100));
        }
        tracing::info!("stopping");
        // Stopping the host drops the client; its handler, on a thread of the core's, then lets
        // go of the client's keys and puts the monitor back. Give it the moment that takes.
        host.stop();
        std::thread::sleep(Duration::from_millis(700));
        Ok(())
    }
}
