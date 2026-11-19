//! `remoteman-agent`: the Linux host's part inside a GNOME session.
//!
//!     remoteman-agent [--listen ADDR:PORT] [--pair] [--state DIR] [--bitrate MBIT]
//!     remoteman-agent --daemon [--socket PATH] [--bitrate MBIT]
//!
//! While a device has the sound on, its speaker is the desktop's output, and the output chosen
//! before comes back afterwards; `--keep-output` leaves the output alone.
//!
//! `--allow files,clipboard`: paired devices may put files into this user's Downloads folder,
//! and share the clipboard both ways. Both are off unless said: any paired device could
//! otherwise write here, or read whatever is copied.
//!
//! By itself it listens and serves one client at a time (a new one takes over); `--pair` lets
//! one unknown device in and remembers it. With `--daemon` it listens for nothing: it attaches
//! to `remotemand`, which owns the port and the trust list and hands it clients.

mod control;
mod pipeline;
mod session;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use remoteman_core::host::{Host, HostOptions};

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("remoteman")
}

fn main() -> Result<()> {
    let level = if std::env::var_os("REMOTEMAN_DEBUG").is_some() { tracing::Level::DEBUG } else { tracing::Level::INFO };
    tracing_subscriber::fmt().with_max_level(level).init();
    let mut listen: SocketAddr = format!("0.0.0.0:{}", remoteman_core::PORT).parse()?;
    let (mut pair, mut state, mut bitrate, mut drop_one_in) = (false, state_dir(), 40u32, None::<u32>);
    let (mut daemon, mut socket) = (false, None::<PathBuf>);
    let (mut advertise, mut advertise_as) = (None::<bool>, None::<String>);
    let mut refs = None::<(u32, u32)>;
    let allowed = session::Allowed::default();
    let (mut allow_files, mut allow_clipboard) = (false, false);
    let mut control_at = None::<PathBuf>;
    let mut take_output = true;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--listen" => listen = value()?.parse().context("--listen ADDR:PORT")?,
            "--state" => state = value()?.into(),
            "--bitrate" => bitrate = value()?.parse().context("--bitrate in Mbit/s")?,
            "--pair" => pair = true,
            "--daemon" => daemon = true,
            // The device's speaker appears on the desktop but isn't made the output: what
            // plays goes on playing here unless a program is pointed at it. (And for tests,
            // which must leave the real desktop's sound alone.)
            "--keep-output" => take_output = false,
            "--allow" => {
                for what in value()?.split(',') {
                    match what.trim() {
                        "files" => allow_files = true,
                        "clipboard" => allow_clipboard = true,
                        other => anyhow::bail!("--allow {other}: files, clipboard"),
                    }
                }
            }
            // For tests: how many recent and long-term frames the encoder keeps, as "4+2".
            "--test-refs" => {
                let v = value()?;
                let (s, l) = v.split_once('+').context("--test-refs RECENT+LONGTERM")?;
                refs = Some((s.parse()?, l.parse()?));
            }
            "--socket" => socket = Some(value()?.into()),
            // Where the host's owner asks how things stand and changes them (see control.rs).
            "--control" => control_at = Some(value()?.into()),
            // Tell the local network this host is here (or don't), whatever it listens on.
            "--advertise" => advertise = Some(true),
            "--no-advertise" => advertise = Some(false),
            "--advertise-as" => advertise_as = Some(value()?),
            // For tests: lose one video packet in this many.
            "--test-drop-one-in" => drop_one_in = Some(value()?.parse().context("--test-drop-one-in N")?),
            other => anyhow::bail!("unknown option {other}"),
        }
    }
    // What the owner switched on earlier (from the host's window) holds until switched off;
    // `--allow` adds to it for this run.
    let saved = control::Saved::load(&state);
    allowed.files.store(allow_files || saved.files, std::sync::atomic::Ordering::Relaxed);
    allowed.clipboard.store(allow_clipboard || saved.clipboard, std::sync::atomic::Ordering::Relaxed);
    let features = control::features(&allowed);
    if daemon {
        // The system service's socket if it is there, else one run by hand as this user.
        let socket = socket.unwrap_or_else(|| {
            let system = remoteman_core::relay::socket_path(true);
            if system.exists() { system } else { remoteman_core::relay::socket_path(false) }
        });
        // The daemon may start after us, or be restarted: keep trying.
        let mut said = false;
        loop {
            match std::os::unix::net::UnixStream::connect(&socket) {
                Ok(stream) => {
                    tracing::info!("attached to the daemon at {}", socket.display());
                    said = false;
                    if let Err(e) = remoteman_core::relay::run_agent(stream, features.clone(), session::Agent::new(bitrate * 1_000_000, refs, allowed.clone(), take_output)?) {
                        tracing::warn!("{e:#}");
                    }
                    tracing::info!("the daemon went away");
                }
                Err(e) if !said => {
                    tracing::info!("no daemon at {} yet ({e}); waiting for it", socket.display());
                    said = true;
                }
                Err(_) => {}
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
    let name = std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "linux".into());
    let said_name = advertise_as.unwrap_or_else(|| name.clone());
    let options = HostOptions { listen, state, name, os: "linux".into(), features, pair, test_drop_one_in: drop_one_in };
    let state_dir = options.state.clone();
    let host = std::sync::Arc::new(Host::start(options, session::Agent::new(bitrate * 1_000_000, refs, allowed.clone(), take_output)?)?);
    tracing::info!("listening on {}; this host's fingerprint is {}", host.address, host.fingerprint);
    let control_at = control_at.unwrap_or_else(|| control::default_path(host.address.port()));
    control::serve(&control_at, std::sync::Arc::downgrade(&host), allowed, state_dir);
    if pair {
        tracing::info!("pairing: the next unknown device to connect will be trusted");
    }
    // Viewers on this network find a host that can be reached from it; one that listens on
    // this computer alone (a test's) keeps to itself unless told otherwise.
    let _advert = if advertise.unwrap_or(!listen.ip().is_loopback()) {
        match remoteman_desktop::advertise::Advert::start(&said_name, host.address.port(), remoteman_desktop::advertise::records(&host.fingerprint, "linux")) {
            Ok(advert) => {
                tracing::info!(name = said_name, "viewers on this network can find this host");
                Some(advert)
            }
            Err(e) => {
                tracing::warn!("this host isn't advertised on the network (viewers need its address): {e:#}");
                None
            }
        }
    } else {
        None
    };

    // Until Ctrl-C or a request to stop.
    let wait = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    wait.block_on(async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        anyhow::Ok(())
    })?;
    let _ = std::fs::remove_file(&control_at);
    // A control request may hold the host for a moment.
    let mut host = host;
    let host = loop {
        match std::sync::Arc::try_unwrap(host) {
            Ok(host) => break host,
            Err(held) => {
                host = held;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    };
    host.stop();
    Ok(())
}
