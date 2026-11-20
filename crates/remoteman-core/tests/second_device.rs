//! More than one device at once: what a second device that connects while another is using the
//! computer gets, by the host's `second_device` setting (docs/PROTOCOL.md, "More than one device").

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use remoteman_core::host::{Accept, ClientInfo, Host, HostHandler, HostOptions, Link, SecondDevice};
use remoteman_core::{Chroma, Client, Codec, Display, Event, Frame, Handler, Message, Mode, Options, StreamInfo};

const MODE: Mode = Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 8 };
const OTHER: Mode = Mode { codec: Codec::Av1, chroma: Chroma::C444, depth: 10 };
const WAIT: Duration = Duration::from_secs(5);

struct Desk {
    input: mpsc::Sender<Event>,
    links: mpsc::Sender<Link>,
    keyframes: Arc<AtomicU32>,
    lost: Arc<AtomicU32>,
    connected: Arc<AtomicU32>,
    left: Arc<AtomicU32>,
}

impl HostHandler for Desk {
    fn connected(&mut self, _client: &ClientInfo, link: Link) -> Result<Accept, String> {
        self.connected.fetch_add(1, Ordering::SeqCst);
        let _ = self.links.send(link);
        Ok(Accept { mode: MODE, width: 640, height: 480, refresh: 60, colour: "bt709".into(), sdr_white: 0, peak: 0 })
    }
    fn keyframe(&mut self) {
        self.keyframes.fetch_add(1, Ordering::SeqCst);
    }
    fn lost(&mut self, _first: u32, _last: u32) {
        self.lost.fetch_add(1, Ordering::SeqCst);
    }
    fn input(&mut self, event: Event) {
        let _ = self.input.send(event);
    }
    fn left(&mut self) {
        self.left.fetch_add(1, Ordering::SeqCst);
    }
}

struct Seen {
    streams: mpsc::Sender<(u16, u32, u32)>,
    frames: mpsc::Sender<(bool, u32, Vec<u8>)>,
    closed: mpsc::Sender<String>,
}

impl Handler for Seen {
    fn stream(&mut self, info: &StreamInfo) {
        let _ = self.streams.send((info.epoch, info.width, info.height));
    }
    fn frame(&mut self, frame: &Frame, _age_us: Option<i64>) {
        let _ = self.frames.send((frame.key, frame.number, frame.data.clone()));
    }
    fn message(&mut self, _message: &Message) {}
    fn closed(&mut self, reason: &str) {
        let _ = self.closed.send(reason.to_owned());
    }
}

struct Device {
    client: Client,
    streams: mpsc::Receiver<(u16, u32, u32)>,
    frames: mpsc::Receiver<(bool, u32, Vec<u8>)>,
    closed: mpsc::Receiver<String>,
}

struct Rig {
    dir: std::path::PathBuf,
    host: Host,
    input: mpsc::Receiver<Event>,
    links: mpsc::Receiver<Link>,
    keyframes: Arc<AtomicU32>,
    lost: Arc<AtomicU32>,
    connected: Arc<AtomicU32>,
    left: Arc<AtomicU32>,
}

fn start(name: &str, mode: SecondDevice) -> Rig {
    let dir = std::env::temp_dir().join(format!("remoteman-second-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (input_tx, input) = mpsc::channel();
    let (links_tx, links) = mpsc::channel();
    let (keyframes, lost, connected, left) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
    let features = ["video", "input", "pen", "gamepad", "clipboard", "files", "sound", "mic", "camera", "configure", "cursor"].iter().map(|f| f.to_string()).collect();
    let host = Host::start(
        HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: dir.join("host"), name: "desk".into(), os: "test".into(), features, pair: true, test_drop_one_in: None },
        Desk { input: input_tx, links: links_tx, keyframes: keyframes.clone(), lost: lost.clone(), connected: connected.clone(), left: left.clone() },
    )
    .unwrap();
    host.set_second_device(mode);
    Rig { dir, host, input, links, keyframes, lost, connected, left }
}

fn try_connect(rig: &Rig, who: &str, decoders: Vec<Mode>) -> anyhow::Result<Device> {
    rig.host.set_pairing(true);
    let (streams_tx, streams) = mpsc::channel();
    let (frames_tx, frames) = mpsc::channel();
    let (closed_tx, closed) = mpsc::channel();
    let client = Client::connect(
        Options {
            host: rig.host.address.to_string(),
            state: rig.dir.join(who),
            name: who.into(),
            pair: true,
            display: Display { width: 640, height: 480, scale: 1.0, refresh: 60, placement: Default::default(), hdr: false, headroom: 0.0 },
            decoders,
            test_drop_one_in: None,
            test_delay_lost_ms: None,
            cursor: false,
            unlock: None,
            attach_only: false,
        },
        Seen { streams: streams_tx, frames: frames_tx, closed: closed_tx },
    )?;
    Ok(Device { client, streams, frames, closed })
}

fn connect(rig: &Rig, who: &str) -> Device {
    try_connect(rig, who, vec![MODE]).unwrap()
}

fn has(features: &[String], f: &str) -> bool {
    features.iter().any(|x| x == f)
}

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out: {what}");
}

#[test]
fn by_default_the_second_device_takes_over() {
    let rig = start("takeover", SecondDevice::TakeOver);
    assert_eq!(rig.host.second_device(), SecondDevice::TakeOver);
    let first = connect(&rig, "first");
    let _link = rig.links.recv_timeout(WAIT).unwrap();
    let second = connect(&rig, "second");
    assert!(has(&second.client.host_features, "input"));
    // The first is ended and the handler heard of both as sessions.
    let reason = first.closed.recv_timeout(WAIT).expect("the first session did not end");
    assert!(!reason.is_empty());
    until("the handler hears left, connected", || rig.connected.load(Ordering::SeqCst) == 2 && rig.left.load(Ordering::SeqCst) == 1);
    drop(second);
}

#[test]
fn the_setting_is_kept_with_the_host() {
    let rig = start("kept", SecondDevice::Control);
    let dir = rig.dir.clone();
    drop(rig);
    let (input_tx, _) = mpsc::channel();
    let (links_tx, _) = mpsc::channel();
    let host = Host::start(
        HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: dir.join("host"), name: "desk".into(), os: "test".into(), features: vec!["video".into()], pair: false, test_drop_one_in: None },
        Desk { input: input_tx, links: links_tx, keyframes: Default::default(), lost: Default::default(), connected: Default::default(), left: Default::default() },
    )
    .unwrap();
    assert_eq!(host.second_device(), SecondDevice::Control);
}

#[test]
fn a_watching_device_sees_what_the_first_sees_and_cannot_use_the_computer() {
    let rig = start("watch", SecondDevice::Watch);
    let first = connect(&rig, "first");
    let link = rig.links.recv_timeout(WAIT).unwrap();
    let (epoch, w, h) = first.streams.recv_timeout(WAIT).unwrap();
    // Some picture before the second device comes.
    assert!(link.send_frame(b"first-key", true, 1));

    let second = connect(&rig, "second");
    // It joined: the handler heard of one session only, and the first is still there.
    assert_eq!(rig.connected.load(Ordering::SeqCst), 1);
    let f = &second.client.host_features;
    assert!(has(f, "video") && has(f, "sound") && has(f, "cursor") && has(f, "parity"));
    for gone in ["input", "pen", "gamepad", "clipboard", "files", "mic", "camera", "configure"] {
        assert!(!has(f, gone), "a watching device is offered {gone}");
    }
    assert!(has(&first.client.host_features, "input"));
    // The same stream message.
    assert_eq!(second.streams.recv_timeout(WAIT).unwrap(), (epoch, w, h));
    // It asked for a keyframe to start from.
    until("the handler is asked for a keyframe", || rig.keyframes.load(Ordering::SeqCst) >= 1);

    // The same frames, with the same numbers.
    assert!(link.send_frame(b"key", true, 2));
    assert!(link.send_frame(b"delta", false, 3));
    let mut got = Vec::new();
    while got.len() < 2 {
        let (key, number, data) = second.frames.recv_timeout(WAIT).expect("the joiner got no frame");
        if data == b"key" || data == b"delta" {
            got.push((key, number, data));
        }
    }
    let mut lead = Vec::new();
    while lead.len() < 2 {
        let (key, number, data) = first.frames.recv_timeout(WAIT).unwrap();
        if data == b"key" || data == b"delta" {
            lead.push((key, number, data));
        }
    }
    assert_eq!(got, lead);
    assert!(got[0].0 && !got[1].0);

    // Its input is thrown away; the first's is not.
    second.client.send(Event::PointerTo { x: 0.9, y: 0.9 });
    first.client.send(Event::PointerTo { x: 0.1, y: 0.1 });
    assert_eq!(rig.input.recv_timeout(WAIT).unwrap(), Event::PointerTo { x: 0.1, y: 0.1 });
    assert!(rig.input.recv_timeout(Duration::from_millis(500)).is_err(), "a watching device's input reached the handler");

    // Its loss is a request for a keyframe, no more: the handler's `lost` is the lead's alone.
    let before = rig.keyframes.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(600));
    second.client.send_message(Message::Lost { first: 1, last: 2, epoch: None });
    until("the joiner's loss asks for a keyframe", || rig.keyframes.load(Ordering::SeqCst) > before);
    assert_eq!(rig.lost.load(Ordering::SeqCst), 0);
    // Asked again at once, it isn't passed on a second time.
    let after = rig.keyframes.load(Ordering::SeqCst);
    second.client.send_message(Message::Keyframe);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(rig.keyframes.load(Ordering::SeqCst), after);

    // The picture starts afresh (a resize): the joiner is told too.
    assert!(link.restart(Accept { mode: MODE, width: 800, height: 600, refresh: 60, colour: "bt709".into(), sdr_white: 0, peak: 0 }));
    let (e2, w2, h2) = second.streams.recv_timeout(WAIT).unwrap();
    assert_eq!((w2, h2), (800, 600));
    assert_ne!(e2, epoch);

    // The connection log has it, and the devices list.
    let log = rig.host.connections(10);
    assert!(log.iter().any(|e| e.event == "connected" && e.name == "second" && e.joined), "{log:?}");
    let devices = rig.host.devices();
    assert!(devices.iter().all(|d| d.connected) && devices.iter().filter(|d| d.joined).count() == 1, "{devices:?}");

    // The first leaves: the session ends for the joiner, with the reason.
    drop(first);
    let reason = second.closed.recv_timeout(WAIT).expect("the joiner was not closed");
    assert!(reason.contains("The device that was using this computer left"), "{reason}");
    until("the handler hears left", || rig.left.load(Ordering::SeqCst) == 1);
}

#[test]
fn a_device_that_cannot_show_the_picture_is_refused() {
    let rig = start("busy", SecondDevice::Watch);
    let first = connect(&rig, "first");
    let _link = rig.links.recv_timeout(WAIT).unwrap();
    let _ = first.streams.recv_timeout(WAIT).unwrap();
    match try_connect(&rig, "second", vec![OTHER]) {
        Err(e) => assert!(format!("{e:#}").contains("busy"), "{e:#}"),
        Ok(second) => {
            let reason = second.closed.recv_timeout(WAIT).expect("not refused");
            assert!(reason.contains("busy"), "{reason}");
        }
    }
    // The first is undisturbed.
    assert!(first.closed.recv_timeout(Duration::from_millis(500)).is_err());
    assert_eq!(rig.connected.load(Ordering::SeqCst), 1);
}

#[test]
fn a_device_that_may_control_has_its_input_used() {
    let rig = start("control", SecondDevice::Control);
    let first = connect(&rig, "first");
    let _link = rig.links.recv_timeout(WAIT).unwrap();
    let _ = first.streams.recv_timeout(WAIT).unwrap();
    let second = connect(&rig, "second");
    let f = &second.client.host_features;
    assert!(has(f, "input") && has(f, "pen") && has(f, "gamepad"));
    assert!(!has(f, "clipboard") && !has(f, "files") && !has(f, "mic") && !has(f, "camera") && !has(f, "configure"));
    second.client.send(Event::PointerTo { x: 0.3, y: 0.3 });
    assert_eq!(rig.input.recv_timeout(WAIT).unwrap(), Event::PointerTo { x: 0.3, y: 0.3 });
    first.client.send(Event::PointerTo { x: 0.6, y: 0.6 });
    assert_eq!(rig.input.recv_timeout(WAIT).unwrap(), Event::PointerTo { x: 0.6, y: 0.6 });
    assert_eq!(rig.connected.load(Ordering::SeqCst), 1);
}
