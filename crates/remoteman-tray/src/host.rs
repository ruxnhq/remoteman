//! The host as this program sees it: a user service (`remoteman-host.service`) that it starts
//! and stops through systemd, and the running host's control socket, which says how things
//! stand and takes the owner's changes (crates/remoteman-agent/src/control.rs).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// The user service that runs the host. `REMOTEMAN_HOST_UNIT` names another, for tests.
pub fn unit() -> String {
    std::env::var("REMOTEMAN_HOST_UNIT").unwrap_or_else(|_| "remoteman-host.service".into())
}

/// The running host's control socket. `REMOTEMAN_HOST_SOCKET` names another, for tests.
fn socket() -> PathBuf {
    if let Some(path) = std::env::var_os("REMOTEMAN_HOST_SOCKET") {
        return path.into();
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join("remoteman-agent-7791.sock")
}

/// Ask the running host one thing. `None`: no host answers.
pub fn ask(line: &str) -> Option<String> {
    let mut stream = UnixStream::connect(socket()).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
    stream.write_all(format!("{line}\n").as_bytes()).ok()?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).ok()?;
    Some(answer.trim().to_owned())
}

fn systemctl(args: &[&str]) -> Option<String> {
    let out = Command::new("systemctl").arg("--user").args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Turn sharing on, now and at every sign-in from here on.
pub fn start() {
    let _ = systemctl(&["enable", "--now", &unit()]);
}

/// Turn sharing off, now and at sign-in, until it is turned on again.
pub fn stop() {
    let _ = systemctl(&["disable", "--now", &unit()]);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Client {
    pub name: String,
    /// When it connected, in seconds since 1970.
    pub since: u64,
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    pub placement: String,
    pub picture: String,
    pub sound: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub fingerprint: String,
    pub name: String,
    pub connected: bool,
}

/// How things stand.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// The host runs.
    Sharing { fingerprint: String, port: u16, pairing: bool, clipboard: bool, files: bool, client: Option<Client>, devices: Vec<Device> },
    /// The host doesn't run because it was stopped (or never started).
    Paused,
    /// The host doesn't run because it failed; why, as far as systemd says.
    Failed(String),
}

pub fn state() -> State {
    if let Some(status) = ask("status").and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok()) {
        let text = |v: &serde_json::Value, key: &str| v[key].as_str().unwrap_or_default().to_owned();
        let number = |v: &serde_json::Value, key: &str| v[key].as_u64().unwrap_or(0);
        let client = status["client"].as_object().map(|_| {
            let c = &status["client"];
            Client {
                name: text(c, "name"),
                since: number(c, "since"),
                width: number(c, "width") as u32,
                height: number(c, "height") as u32,
                refresh: number(c, "refresh") as u32,
                placement: text(c, "placement"),
                picture: text(c, "picture"),
                sound: c["sound"].as_bool().unwrap_or(false),
            }
        });
        let devices = status["devices"]
            .as_array()
            .map(|list| list.iter().map(|d| Device { fingerprint: text(d, "fingerprint"), name: text(d, "name"), connected: d["connected"].as_bool().unwrap_or(false) }).collect())
            .unwrap_or_default();
        let port = text(&status, "address").rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(7791);
        return State::Sharing {
            fingerprint: text(&status, "fingerprint"),
            port,
            pairing: status["pairing"].as_bool().unwrap_or(false),
            clipboard: status["clipboard"].as_bool().unwrap_or(false),
            files: status["files"].as_bool().unwrap_or(false),
            client,
            devices,
        };
    }
    match systemctl(&["is-failed", &unit()]).as_deref() {
        Some("failed") => {
            let why = systemctl(&["show", "-p", "Result", "--value", &unit()]).unwrap_or_default();
            State::Failed(if why.is_empty() || why == "success" { "The host stopped unexpectedly".into() } else { format!("The host stopped: {why}") })
        }
        _ => State::Paused,
    }
}

/// This computer's name, and the address a device on the same network reaches it at.
pub fn reach(port: u16) -> (String, String) {
    let name = std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "localhost".into());
    // The address the system would use to reach another network: nothing is sent.
    let ip = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|s| s.connect("192.0.2.1:9").and_then(|_| s.local_addr())).map(|a| a.ip().to_string()).unwrap_or_default();
    let with_port = |host: &str| if port == 7791 { host.to_owned() } else { format!("{host}:{port}") };
    (with_port(&name), if ip.is_empty() { String::new() } else { with_port(&ip) })
}

/// A key's fingerprint as people compare it: the first sixteen digits, in fours.
pub fn short_key(fingerprint: &str) -> String {
    let digits: Vec<char> = fingerprint.to_uppercase().chars().take(16).collect();
    digits.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

/// "14 min", "2 h 5 min", "just now".
pub fn ago(since: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let minutes = now.saturating_sub(since) / 60;
    match minutes {
        0 => "just now".into(),
        m if m < 60 => format!("{m} min"),
        m => format!("{} h {} min", m / 60, m % 60),
    }
}
