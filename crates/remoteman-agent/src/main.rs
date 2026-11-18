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
    let mut refs = None::<(u32, u32)>;
    let mut allowed = session::Allowed::default();
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
                        "files" => allowed.files = true,
                        "clipboard" => allowed.clipboard = true,
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
            // For tests: lose one video packet in this many.
            "--test-drop-one-in" => drop_one_in = Some(value()?.parse().context("--test-drop-one-in N")?),
            other => anyhow::bail!("unknown option {other}"),
        }
    }
    let mut features: Vec<String> = ["video", "input", "cursor", "placement", "configure", "sound", "mic"].map(String::from).to_vec();
    if allowed.files {
        features.push("files".into());
    }
    if allowed.clipboard {
        features.push("clipboard".into());
    }
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
                    if let Err(e) = remoteman_core::relay::run_agent(stream, features.clone(), session::Agent::new(bitrate * 1_000_000, refs, allowed, take_output)?) {
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
    let options = HostOptions { listen, state, name, os: "linux".into(), features, pair, test_drop_one_in: drop_one_in };
    let host = Host::start(options, session::Agent::new(bitrate * 1_000_000, refs, allowed, take_output)?)?;
    tracing::info!("listening on {}; this host's fingerprint is {}", host.address, host.fingerprint);
    if pair {
        tracing::info!("pairing: the next unknown device to connect will be trusted");
    }

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
    host.stop();
    Ok(())
}
