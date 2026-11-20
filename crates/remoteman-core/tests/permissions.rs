//! What a device may do: a view-only device's input never reaches the host's handler, its hello
//! and later `features` leave out what it isn't allowed, and the connection log says who came
//! and went.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use remoteman_core::host::{Accept, ClientInfo, ConnectionEvent, Host, HostHandler, HostOptions, Link, Permissions};
use remoteman_core::{Chroma, Client, Codec, Display, Event, Frame, Handler, Message, Mode, Options, StreamInfo};

const MODE: Mode = Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 8 };

struct Desk {
    input: mpsc::Sender<Event>,
    messages: mpsc::Sender<Message>,
    notices: mpsc::Sender<ConnectionEvent>,
    links: mpsc::Sender<Link>,
}

impl HostHandler for Desk {
    fn connected(&mut self, _client: &ClientInfo, link: Link) -> Result<Accept, String> {
        let _ = self.links.send(link);
        Ok(Accept { mode: MODE, width: 640, height: 480, refresh: 60, colour: "bt709".into(), sdr_white: 0, peak: 0 })
    }
    fn keyframe(&mut self) {}
    fn lost(&mut self, _first: u32, _last: u32) {}
    fn input(&mut self, event: Event) {
        let _ = self.input.send(event);
    }
    fn message(&mut self, message: &Message) {
        let _ = self.messages.send(message.clone());
    }
    fn notice(&mut self, event: &ConnectionEvent) {
        let _ = self.notices.send(event.clone());
    }
    fn left(&mut self) {}
}

struct Screen(mpsc::Sender<Message>);

impl Handler for Screen {
    fn stream(&mut self, _info: &StreamInfo) {}
    fn frame(&mut self, _frame: &Frame, _age_us: Option<i64>) {}
    fn message(&mut self, message: &Message) {
        let _ = self.0.send(message.clone());
    }
    fn closed(&mut self, _reason: &str) {}
}

struct Rig {
    dir: std::path::PathBuf,
    host: Host,
    input: mpsc::Receiver<Event>,
    messages: mpsc::Receiver<Message>,
    notices: mpsc::Receiver<ConnectionEvent>,
}

fn start(name: &str) -> Rig {
    let dir = std::env::temp_dir().join(format!("remoteman-perm-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (input_tx, input) = mpsc::channel();
    let (messages_tx, messages) = mpsc::channel();
    let (notices_tx, notices) = mpsc::channel();
    let (links, _) = mpsc::channel();
    let features = ["video", "input", "pen", "clipboard", "files", "sound", "mic", "camera"].iter().map(|f| f.to_string()).collect();
    let host = Host::start(
        HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: dir.join("host"), name: "desk".into(), os: "test".into(), features, pair: true, test_drop_one_in: None },
        Desk { input: input_tx, messages: messages_tx, notices: notices_tx, links },
    )
    .unwrap();
    Rig { dir, host, input, messages, notices }
}

fn connect(rig: &Rig) -> (Client, mpsc::Receiver<Message>) {
    let (tx, from_host) = mpsc::channel();
    let client = Client::connect(
        Options {
            host: rig.host.address.to_string(),
            state: rig.dir.join("device"),
            name: "Anna's iPad".into(),
            pair: true,
            display: Display { width: 640, height: 480, scale: 1.0, refresh: 60, placement: Default::default(), hdr: false, headroom: 0.0 },
            decoders: vec![MODE],
            test_drop_one_in: None,
            test_delay_lost_ms: None,
            cursor: false,
            unlock: None,
            attach_only: false,
        },
        Screen(tx),
    )
    .unwrap();
    (client, from_host)
}

fn has(features: &[String], f: &str) -> bool {
    features.iter().any(|x| x == f)
}

#[test]
fn a_view_only_device_is_shown_the_picture_and_nothing_it_sends_is_used() {
    let rig = start("view");
    let (client, from_host) = connect(&rig);
    assert!(has(&client.host_features, "input") && has(&client.host_features, "sound"));
    let fp = rig.host.devices()[0].fingerprint.clone();
    assert_eq!(rig.host.permissions(&fp), Permissions::default());

    // Allowed: its input arrives.
    client.send(Event::PointerTo { x: 0.1, y: 0.1 });
    assert_eq!(rig.input.recv_timeout(Duration::from_secs(5)).unwrap(), Event::PointerTo { x: 0.1, y: 0.1 });

    // Switched to view-only mid-session: told at once, with the host's features less `input`.
    rig.host.set_permissions(&fp, Permissions { input: false, sound: false, ..Permissions::default() }).unwrap();
    let features = loop {
        match from_host.recv_timeout(Duration::from_secs(5)).expect("no features message") {
            Message::Features { features } => break features,
            _ => {}
        }
    };
    assert!(!has(&features, "input") && !has(&features, "pen") && !has(&features, "sound"));
    assert!(has(&features, "clipboard") && has(&features, "video"));

    // An old client sends it all anyway: none of it gets to the handler.
    client.send(Event::PointerTo { x: 0.9, y: 0.9 });
    client.send(Event::Pen { phase: remoteman_core::PenPhase::Down, buttons: 0, x: 0.5, y: 0.5, pressure: 0.5, tilt_x: 0.0, tilt_y: 0.0, twist: 0.0 });
    client.send_message(Message::Sound { on: true });
    client.send_message(Message::Clipboard { on: true });
    assert!(rig.input.recv_timeout(Duration::from_millis(500)).is_err(), "input reached the handler of a view-only device");
    // The clipboard message (allowed) arrives and the sound request (not allowed) does not.
    let mut seen = Vec::new();
    while let Ok(m) = rig.messages.recv_timeout(Duration::from_millis(500)) {
        seen.push(m);
    }
    assert!(seen.contains(&Message::Clipboard { on: true }), "{seen:?}");
    assert!(!seen.contains(&Message::Sound { on: true }), "{seen:?}");

    // Persisted: the next hello lacks `input`, and so does a new client.
    drop(client);
    let (again, _) = connect(&rig);
    assert!(!has(&again.host_features, "input") && !has(&again.host_features, "sound") && has(&again.host_features, "files"));
    let after = Permissions { input: false, sound: false, ..Permissions::default() };
    assert_eq!(rig.host.permissions(&fp), after);
    rig.host.set_permissions(&fp, Permissions::default()).unwrap();
    again.send(Event::PointerTo { x: 0.3, y: 0.3 });
    assert_eq!(rig.input.recv_timeout(Duration::from_secs(5)).unwrap(), Event::PointerTo { x: 0.3, y: 0.3 });
    again.close();
    rig.host.stop();
    let _ = std::fs::remove_dir_all(&rig.dir);
}

#[test]
fn the_log_has_who_connected_and_left_and_the_handler_is_told() {
    let rig = start("log");
    let (client, _from_host) = connect(&rig);
    client.close();
    let deadline = Instant::now() + Duration::from_secs(5);
    let log = loop {
        let log = rig.host.connections(10);
        if log.iter().any(|e| e.event == "left") || Instant::now() > deadline {
            break log;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let events: Vec<&str> = log.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(events, ["paired", "connected", "left"], "{log:?}");
    assert!(log.iter().all(|e| e.name == "Anna's iPad" && e.fingerprint.len() == 64 && e.address.starts_with("127.0.0.1:") && e.time.ends_with('Z')));
    assert!(log[2].duration.is_some());
    // The same lines are in the file.
    let text = std::fs::read_to_string(rig.dir.join("host/connections.log")).unwrap();
    assert_eq!(text.lines().count(), 3);
    // The handler was told of the first two, not of leaving.
    let told: Vec<String> = std::iter::from_fn(|| rig.notices.recv_timeout(Duration::from_millis(300)).ok()).map(|e| e.event).collect();
    assert_eq!(told, ["paired", "connected"]);
    rig.host.stop();
    let _ = std::fs::remove_dir_all(&rig.dir);
}
