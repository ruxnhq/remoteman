//! `remotemand`: the system service of a Linux host.
//!
//!     remotemand [--listen ADDR:PORT] [--state DIR] [--socket PATH] [--pair] [--name NAME]
//!                [--user NAME] [--login screen|key|off]
//!     remotemand ctl [--socket PATH] status | switch N | prefer N
//!     remotemand ctl [--socket PATH] devices | revoke FINGERPRINT | drop | pair on|off
//!     remotemand test-agent [--socket PATH] [--label NAME]      (for tests: see standin.rs)
//!
//! It owns the UDP port, the trust list and every client's connection. The picture comes from
//! `remoteman-agent --daemon`, one in each desktop session, which attach over the Unix socket.
//! `ctl status` lists them; `ctl switch N` moves the connected client to agent N without the
//! client reconnecting (what happens by itself when a login screen gives way to a session);
//! `ctl prefer N` gives the next client to agent N.
//!
//! `--user NAME`: the paired devices are NAME's (one user for all of them, for now). A device
//! goes to NAME's session if there is one. If there is none, `--login` says what happens:
//! `screen` (the default when run as root) asks GDM for a headless login screen and moves the
//! device to the session once someone has logged in on it; `key` asks GDM for NAME's session
//! straight away, with no password, trusting the device's key alone; `off` turns it away.

mod broker;
mod gdm;
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
    let mut advertise = None::<bool>;
    let (mut user, mut login) = (None::<String>, if root { "screen" } else { "off" }.to_owned());
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
            "--user" => user = Some(value()?),
            "--login" => login = value()?,
            // Tell the local network this host is here (or don't), whatever it listens on.
            "--advertise" => advertise = Some(true),
            "--no-advertise" => advertise = Some(false),
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

    let owner = match user {
        Some(name) => Some((name.clone(), uid_of(&name).with_context(|| format!("there is no user {name}"))?)),
        None => None,
    };
    let login = match login.as_str() {
        "off" => None,
        how @ ("screen" | "key") => {
            anyhow::ensure!(owner.is_some(), "--login {how} needs --user: whose session a device logs in to");
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).thread_name("gdm").enable_all().build()?;
            let gdm = runtime.block_on(gdm::Gdm::connect())?;
            Some(broker::Login { runtime, gdm, by_key: how == "key" })
        }
        other => anyhow::bail!("--login {other}: screen, key or off"),
    };
    let shared = broker::Shared::new(owner, login, Some(&state));
    // What the agents can do. (Each agent says what it has when it attaches; until the daemon
    // tells clients per session, it lists what an agent allowed everything would have.)
    let features = ["video", "input", "cursor", "placement", "mirror", "configure", "sound", "mic", "camera", "files", "clipboard"].map(String::from).to_vec();
    let said_name = name.clone();
    let options = HostOptions { listen, state, name, os: "linux".into(), features, pair, test_drop_one_in: drop_one_in };
    let host = std::sync::Arc::new(Host::start(options, broker::Broker(shared.clone()))?);
    shared.listening(&host);
    tracing::info!("listening on {}; this host's fingerprint is {}; agents attach at {}", host.address, host.fingerprint, socket.display());
    if pair {
        tracing::info!("pairing: the next unknown device to connect will be trusted");
    }
    std::thread::Builder::new().name("agents".into()).spawn(move || broker::accept(listener, shared))?;
    // Viewers on this network find a host that can be reached from it; one that listens on
    // this computer alone (a test's) keeps to itself.
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
    // Stop listening and tell the client. A `ctl` request may hold the host for a moment.
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
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

/// A user's number, from the system's user database.
fn uid_of(name: &str) -> Option<u32> {
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: a valid C string; the result is read at once, before any other such call.
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    if entry.is_null() { None } else { Some(unsafe { (*entry).pw_uid }) }
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("remoteman")
}
