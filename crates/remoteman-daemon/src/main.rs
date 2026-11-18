//! `remotemand`: the system service of a Linux host.
//!
//!     remotemand [--listen ADDR:PORT] [--state DIR] [--socket PATH] [--pair] [--name NAME]
//!     remotemand ctl [--socket PATH] status | switch N | prefer N
//!     remotemand test-agent [--socket PATH] [--label NAME]      (for tests: see standin.rs)
//!
//! It owns the UDP port, the trust list and every client's connection. The picture comes from
//! `remoteman-agent --daemon`, one in each desktop session, which attach over the Unix socket.
//! `ctl status` lists them; `ctl switch N` moves the connected client to agent N without the
//! client reconnecting (what happens by itself when a login screen gives way to a session);
//! `ctl prefer N` gives the next client to agent N.

mod broker;
mod standin;

use std::io::Read;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use anyhow::{Context, Result};
use remoteman_core::host::{Host, HostOptions};
use remoteman_core::relay::{self, ToDaemon};

fn main() -> Result<()> {
    let level = if std::env::var_os("REMOTEMAN_DEBUG").is_some() { tracing::Level::DEBUG } else { tracing::Level::INFO };
    tracing_subscriber::fmt().with_max_level(level).init();
    // SAFETY: no arguments, no state.
    let root = unsafe { libc::geteuid() } == 0;
    let mut listen: SocketAddr = format!("0.0.0.0:{}", remoteman_core::PORT).parse()?;
    let mut state = if root { PathBuf::from("/var/lib/remoteman") } else { state_dir() };
    let mut socket = relay::socket_path(root);
    let (mut pair, mut drop_one_in, mut name) = (false, None::<u32>, None::<String>);
    let mut ctl = None::<Vec<String>>;
    let (mut standin, mut label) = (false, "stand-in".to_owned());
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if let Some(words) = &mut ctl {
            if a == "--socket" {
                socket = args.next().context("--socket needs a value")?.into();
            } else {
                words.push(a);
            }
            continue;
        }
        let mut value = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "ctl" => ctl = Some(Vec::new()),
            "test-agent" => standin = true,
            "--label" => label = value()?,
            "--listen" => listen = value()?.parse().context("--listen ADDR:PORT")?,
            "--state" => state = value()?.into(),
            "--socket" => socket = value()?.into(),
            "--name" => name = Some(value()?),
            "--pair" => pair = true,
            // For tests: lose one video packet in this many.
            "--test-drop-one-in" => drop_one_in = Some(value()?.parse().context("--test-drop-one-in N")?),
            other => anyhow::bail!("unknown option {other}"),
        }
    }
    if standin {
        return standin::run(&socket, label);
    }
    if let Some(words) = ctl {
        let mut stream = UnixStream::connect(&socket).with_context(|| format!("no daemon at {}", socket.display()))?;
        ToDaemon::Control(words.join(" ")).write(&mut stream)?;
        let mut answer = String::new();
        stream.read_to_string(&mut answer)?;
        print!("{answer}");
        return Ok(());
    }

    let name = name.unwrap_or_else(|| std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "linux".into()));
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    // A socket file left by a daemon that died is in the way; one in use is not ours to take.
    if UnixStream::connect(&socket).is_ok() {
        anyhow::bail!("another remotemand is already serving {}", socket.display());
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).with_context(|| format!("listen on {}", socket.display()))?;
    // Agents run as whoever owns the session (the login screen's user among them). Who an
    // agent is comes from the kernel, never from this file's mode.
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(if root { 0o666 } else { 0o600 }))?;

    let shared = broker::Shared::new();
    let features = ["video", "input", "cursor", "placement"].map(String::from).to_vec();
    let options = HostOptions { listen, state, name, os: "linux".into(), features, pair, test_drop_one_in: drop_one_in };
    let host = Host::start(options, broker::Broker(shared.clone()))?;
    tracing::info!("listening on {}; this host's fingerprint is {}; agents attach at {}", host.address, host.fingerprint, socket.display());
    if pair {
        tracing::info!("pairing: the next unknown device to connect will be trusted");
    }
    std::thread::Builder::new().name("agents".into()).spawn(move || broker::accept(listener, shared))?;

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
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("remoteman")
}
