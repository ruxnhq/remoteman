//! The running host, as its control pipe says it stands. The pipe (`\\.\pipe\remoteman-host-PORT`,
//! `crates/remoteman-host-windows/src/control.rs`) takes one line and answers it; the host is
//! asked for `status` once a second and told what the owner chose, in its own words: `sharing
//! on|off`, `pair on|off|accept|decline`, `allow clipboard|files on|off`, `drop`, `revoke
//! FINGERPRINT`, `send PATH`. Every rule is the host's; this only reads and asks.

use std::io::{Read, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The port the host listens on unless it was told otherwise.
pub const DEFAULT_PORT: u16 = 7791;

/// Why the host couldn't be asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreached {
    /// Nothing answers: the host isn't running (or not on this port).
    NoHost,
    /// The pipe is there but Windows refused it to this user.
    Refused,
}

/// Ask the host one line and return its answer, trimmed.
pub fn ask(port: u16, line: &str) -> Result<String, Unreached> {
    let name = format!(r"\\.\pipe\remoteman-host-{port}");
    // Between two questions the pipe is away for a moment (the host makes a new one for each),
    // and another program's question may hold it.
    for _ in 0..10 {
        match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
            Ok(mut pipe) => {
                if pipe.write_all(format!("{line}\n").as_bytes()).is_err() {
                    std::thread::sleep(Duration::from_millis(30));
                    continue;
                }
                let mut said = String::new();
                let _ = pipe.read_to_string(&mut said);
                return Ok(said.trim().to_string());
            }
            // ERROR_ACCESS_DENIED
            Err(e) if e.raw_os_error() == Some(5) => return Err(Unreached::Refused),
            // ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY and the like: wait a moment and try again.
            Err(_) => std::thread::sleep(Duration::from_millis(30)),
        }
    }
    Err(Unreached::NoHost)
}

/// Ask for `status`. An answer that isn't understood counts as no host, and is logged.
pub fn read(port: u16) -> Result<Status, Unreached> {
    let said = ask(port, "status")?;
    match Status::parse(&said) {
        Some(status) => Ok(status),
        None => {
            crate::log::say(&format!("status not understood: {}", said.chars().take(200).collect::<String>()));
            Err(Unreached::NoHost)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub fingerprint: String,
    pub connected: bool,
    pub permissions: Permissions,
}

/// What a device may do here (all of it, unless the host says otherwise).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permissions {
    /// False: view only.
    pub input: bool,
    pub clipboard: bool,
    pub files: bool,
    pub sound: bool,
    pub mic: bool,
    pub camera: bool,
}

impl Default for Permissions {
    fn default() -> Self {
        Self { input: true, clipboard: true, files: true, sound: true, mic: true, camera: true }
    }
}

/// One line of the host's connection log: `paired`, `connected`, `left` or `refused`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Connection {
    /// Which one this is, counting from the host's start (0 for a line of the log).
    pub seq: u64,
    /// UTC, `2026-11-19T10:15:00Z`.
    pub time: String,
    pub event: String,
    pub name: String,
    pub address: String,
    pub duration: Option<u64>,
    pub reason: String,
}

impl Connection {
    /// What it says in a list or a notification: "Anna's iPad connected".
    pub fn words(&self) -> String {
        match self.event.as_str() {
            "connected" => format!("{} connected", self.name),
            "left" => match self.duration {
                Some(s) => format!("{} left after {}", self.name, span(s)),
                None => format!("{} left", self.name),
            },
            "refused" => format!("{} was refused", self.name),
            "paired" => format!("{} was paired", self.name),
            other => format!("{} {other}", self.name),
        }
    }

    /// When, as a list shows it: `2026-11-19 10:15 UTC`.
    pub fn when(&self) -> String {
        let t = self.time.replace('T', " ");
        format!("{} UTC", t.trim_end_matches('Z').get(..16).unwrap_or(&t))
    }
}

/// A length of time in words: "40 s", "12 min", "1 h 5 min".
fn span(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds} s")
    } else if seconds < 3600 {
        format!("{} min", seconds / 60)
    } else {
        format!("{} h {} min", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// The device being served.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Client {
    pub name: String,
    /// When it connected, in seconds since 1970.
    pub since: u64,
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    /// `extra`, `main`, `only` or `mirror`.
    pub placement: String,
    /// What its picture is encoded as, e.g. "AV1 10-bit".
    pub picture: String,
    pub sound: bool,
}

/// A new device waiting for the owner's word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub name: String,
    pub fingerprint: String,
    pub code: String,
    /// How long the owner has left to answer.
    pub seconds: u32,
}

/// One answer to `status`. Read leniently: a field the host doesn't send is empty or off, so
/// a host that says more (or less) is still understood.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub address: String,
    pub fingerprint: String,
    pub name: String,
    pub paused: bool,
    pub pairing: bool,
    pub clipboard: bool,
    pub files: bool,
    pub lock_on_leave: bool,
    pub speakers_follow: bool,
    pub microphone: bool,
    /// Tell the owner when a device connects (on unless the host says otherwise).
    pub notify: bool,
    /// A code from an authenticator app is asked for on top of a device's key: `Some`, with how
    /// many devices were told "don't ask again" and not yet expired.
    pub second_factor: Option<usize>,
    pub devices: Vec<Device>,
    /// What the host reported since it started, oldest first, for notifications.
    pub notices: Vec<Connection>,
    /// The last lines of its connection log, oldest first.
    pub connections: Vec<Connection>,
    pub client: Option<Client>,
    pub pending: Option<Pending>,
    /// What the device said of files sent to it: how many it answered for, and the last.
    pub answered: u64,
    pub sent_ok: bool,
    pub said: String,
}

impl Status {
    pub fn parse(text: &str) -> Option<Status> {
        let o: serde_json::Value = serde_json::from_str(text).ok()?;
        if !o.is_object() {
            return None;
        }
        let s = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
        let b = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
        let n = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let yes = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(true);
        let devices = o
            .get("devices")
            .and_then(|d| d.as_array())
            .map(|list| {
                list.iter()
                    .map(|d| {
                        let p = d.get("permissions").cloned().unwrap_or_default();
                        let permissions = Permissions { input: yes(&p, "input"), clipboard: yes(&p, "clipboard"), files: yes(&p, "files"), sound: yes(&p, "sound"), mic: yes(&p, "mic"), camera: yes(&p, "camera") };
                        Device { name: s(d, "name"), fingerprint: s(d, "fingerprint"), connected: b(d, "connected"), permissions }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let events = |key: &str| -> Vec<Connection> {
            o.get(key)
                .and_then(|d| d.as_array())
                .map(|list| list.iter().map(|e| Connection { seq: n(e, "seq"), time: s(e, "time"), event: s(e, "event"), name: s(e, "name"), address: s(e, "address"), duration: e.get("duration").and_then(|x| x.as_u64()), reason: s(e, "reason") }).collect())
                .unwrap_or_default()
        };
        let client = o.get("client").filter(|c| c.is_object()).map(|c| Client {
            name: s(c, "name"),
            since: n(c, "since"),
            width: n(c, "width") as u32,
            height: n(c, "height") as u32,
            refresh: n(c, "refresh") as u32,
            placement: s(c, "placement"),
            picture: s(c, "picture"),
            sound: b(c, "sound"),
        });
        let pending = o.get("pending").filter(|p| p.is_object()).map(|p| Pending { name: s(p, "name"), fingerprint: s(p, "fingerprint"), code: s(p, "code"), seconds: n(p, "seconds") as u32 });
        let sent = o.get("sent").cloned().unwrap_or_default();
        Some(Status {
            address: s(&o, "address"),
            fingerprint: s(&o, "fingerprint"),
            name: s(&o, "name"),
            paused: b(&o, "paused"),
            pairing: b(&o, "pairing"),
            clipboard: b(&o, "clipboard"),
            files: b(&o, "files"),
            lock_on_leave: b(&o, "lock_on_leave"),
            speakers_follow: b(&o, "speakers_follow"),
            microphone: b(&o, "microphone"),
            notify: yes(&o, "notify"),
            second_factor: o.get("second_factor").filter(|v| v.is_object()).map(|v| n(v, "remembered") as usize),
            notices: events("notices"),
            connections: events("connections"),
            devices,
            client,
            pending,
            answered: n(&sent, "answered"),
            sent_ok: b(&sent, "ok"),
            said: s(&sent, "said"),
        })
    }

    /// What the state is called, and a line or two more: the tray menu's first lines, the
    /// icon's tooltip and the window's status card. As Linux's Remoteman Host says it.
    pub fn words(&self, pc: &str) -> (String, Vec<String>) {
        if self.paused {
            return (format!("Not sharing {pc}"), vec!["No device can connect until you turn it on. Paired devices stay paired.".into()]);
        }
        if let Some(c) = &self.client {
            let place = match c.placement.as_str() {
                "main" => "Main display",
                "only" => "Only display",
                "mirror" => "This PC's display",
                _ => "Extra display",
            };
            return (
                "A device is connected".into(),
                vec![
                    format!("{} · connected {}", c.name, ago(c.since)),
                    format!("{} × {} at {} Hz · {place}", c.width, c.height, c.refresh),
                    format!("{}{}", c.picture, if c.sound { " · sound on" } else { "" }),
                ],
            );
        }
        if self.pairing {
            return ("Pairing is open".into(), vec!["The next new device to connect is asked about here, then pairing closes.".into()]);
        }
        ("No device is connected".into(), vec![format!("Paired devices can connect while {pc} is on.")])
    }
}

/// Where a device's display is, as a notification says it.
pub fn placement_phrase(placement: &str) -> &'static str {
    match placement {
        "mirror" => "this PC's display",
        "only" => "the only display",
        "main" => "the main display",
        _ => "an extra display",
    }
}

/// The fingerprint as people compare it: its first 16 characters in fours.
pub fn short_key(fingerprint: &str) -> String {
    let s: Vec<char> = fingerprint.to_uppercase().chars().take(16).collect();
    s.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ")
}

/// A six-digit code in two threes.
pub fn code(code: &str) -> String {
    let digits: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 6 { format!("{} {}", &digits[..3], &digits[3..]) } else { code.to_string() }
}

/// How long ago something happened, in words: "just now", "2 min", "1 h 5 min".
pub fn ago(since: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    ago_at(since, now)
}

fn ago_at(since: u64, now: u64) -> String {
    let span = now.saturating_sub(since);
    if span < 60 {
        "just now".into()
    } else if span < 3600 {
        format!("{} min", span / 60)
    } else {
        format!("{} h {} min", span / 3600, (span % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As `crates/remoteman-host-windows/src/control.rs` writes it.
    const SAID: &str = r#"{"address":"0.0.0.0:7791","fingerprint":"c41e77a09b2d5f36aa","pairing":false,"pending":{"name":"Studio Mac","fingerprint":"ab","code":"482913","seconds":51},"devices":[{"fingerprint":"ab","name":"iPad Pro","connected":true,"permissions":{"input":false,"clipboard":true,"files":true,"sound":true,"mic":false,"camera":true}}],"notify":true,"notices":[{"seq":7,"time":"2026-11-19T10:15:00Z","event":"connected","name":"iPad Pro","fingerprint":"ab","address":"10.0.0.5:5000"}],"connections":[{"time":"2026-11-19T10:15:00Z","event":"connected","name":"iPad Pro","fingerprint":"ab","address":"10.0.0.5:5000"},{"time":"2026-11-19T10:27:00Z","event":"left","name":"iPad Pro","fingerprint":"ab","address":"10.0.0.5:5000","duration":720}],"daemon":false,"paused":false,"name":"DEN-PC","clipboard":true,"files":false,"microphone":true,"client":{"name":"iPad Pro","since":100,"width":2752,"height":2064,"refresh":120,"placement":"extra","picture":"HEVC 10-bit","sound":true},"sent":{"answered":2,"ok":true,"said":"Downloads/a.txt"}}"#;

    #[test]
    fn reads_the_hosts_status() {
        let s = Status::parse(SAID).unwrap();
        assert_eq!(s.name, "DEN-PC");
        assert!(s.clipboard && !s.files && s.microphone && !s.paused);
        assert_eq!(s.devices, vec![Device { name: "iPad Pro".into(), fingerprint: "ab".into(), connected: true, permissions: Permissions { input: false, mic: false, ..Permissions::default() } }]);
        assert!(s.notify);
        assert_eq!(s.notices.len(), 1);
        assert_eq!(s.notices[0].seq, 7);
        assert_eq!(s.notices[0].words(), "iPad Pro connected");
        assert_eq!(s.connections[1].words(), "iPad Pro left after 12 min");
        assert_eq!(s.connections[0].when(), "2026-11-19 10:15 UTC");
        assert_eq!(s.pending.as_ref().unwrap().code, "482913");
        assert_eq!(s.client.as_ref().unwrap().width, 2752);
        assert_eq!((s.answered, s.sent_ok, s.said.as_str()), (2, true, "Downloads/a.txt"));
    }

    #[test]
    fn missing_fields_are_empty_and_nulls_are_none() {
        let s = Status::parse(r#"{"client":null,"pending":null,"something":"new"}"#).unwrap();
        assert!(s.client.is_none() && s.pending.is_none() && s.devices.is_empty());
        assert!(Status::parse("not json").is_none());
        assert!(Status::parse("[1]").is_none());
    }

    #[test]
    fn words_for_each_state() {
        let mut s = Status::parse(SAID).unwrap();
        let (title, lines) = s.words("DEN-PC");
        assert_eq!(title, "A device is connected");
        assert!(lines[0].starts_with("iPad Pro · connected "));
        assert_eq!(lines[1], "2752 × 2064 at 120 Hz · Extra display");
        assert_eq!(lines[2], "HEVC 10-bit · sound on");
        s.client = None;
        assert_eq!(s.words("DEN-PC").0, "No device is connected");
        s.pairing = true;
        assert_eq!(s.words("DEN-PC").0, "Pairing is open");
        s.paused = true;
        assert_eq!(s.words("DEN-PC").0, "Not sharing DEN-PC");
    }

    #[test]
    fn keys_codes_and_times() {
        assert_eq!(short_key("c41e77a09b2d5f36aa"), "C41E 77A0 9B2D 5F36");
        assert_eq!(short_key("abc"), "ABC");
        assert_eq!(code("482913"), "482 913");
        assert_eq!(code("12"), "12");
        assert_eq!(ago_at(100, 130), "just now");
        assert_eq!(ago_at(100, 100 + 14 * 60 + 5), "14 min");
        assert_eq!(ago_at(0, 3600 + 5 * 60), "1 h 5 min");
    }
}
