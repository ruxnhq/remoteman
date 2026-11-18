//! `remoteman-agent`: the Linux host, standalone for now. Run it inside a GNOME session:
//!
//!     remoteman-agent [--listen ADDR:PORT] [--pair] [--state DIR] [--bitrate MBIT]
//!
//! `--pair` lets one unknown device in and remembers it. One client at a time; a new one takes
//! over.

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
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--listen" => listen = value()?.parse().context("--listen ADDR:PORT")?,
            "--state" => state = value()?.into(),
            "--bitrate" => bitrate = value()?.parse().context("--bitrate in Mbit/s")?,
            "--pair" => pair = true,
            // For tests: lose one video packet in this many.
            "--test-drop-one-in" => drop_one_in = Some(value()?.parse().context("--test-drop-one-in N")?),
            other => anyhow::bail!("unknown option {other}"),
        }
    }
    let name = std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "linux".into());
    let options = HostOptions { listen, state, name, os: "linux".into(), features: vec!["video".into(), "input".into(), "cursor".into(), "placement".into()], pair, test_drop_one_in: drop_one_in };
    let host = Host::start(options, session::Agent::new(bitrate * 1_000_000)?)?;
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
