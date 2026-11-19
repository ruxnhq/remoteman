//! A desktop where nothing moves sends a frame and then nothing. Whatever of that last frame the
//! network loses must still reach the client: no later frame comes for it to notice the loss by.

use std::sync::mpsc;
use std::time::Duration;

use remoteman_core::host::{Accept, ClientInfo, Host, HostHandler, HostOptions, Link};
use remoteman_core::{Chroma, Client, Codec, Display, Event, Frame, Handler, Mode, Options, StreamInfo};

const MODE: Mode = Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 8 };

fn accept() -> Accept {
    Accept { mode: MODE, width: 640, height: 480, refresh: 60, colour: "bt709".into(), sdr_white: 0, peak: 0 }
}

/// Hands its link to the test, and counts what the client asks for.
struct Desk {
    links: mpsc::Sender<Link>,
    asked: mpsc::Sender<&'static str>,
}

impl HostHandler for Desk {
    fn connected(&mut self, _client: &ClientInfo, link: Link) -> Result<Accept, String> {
        let _ = self.links.send(link);
        Ok(accept())
    }
    fn keyframe(&mut self) {
        let _ = self.asked.send("keyframe");
    }
    fn lost(&mut self, _first: u32, _last: u32) {
        let _ = self.asked.send("lost");
    }
    fn input(&mut self, _event: Event) {}
    fn left(&mut self) {}
}

#[derive(Debug, PartialEq)]
enum Seen {
    Stream(u16),
    Frame(u32, Vec<u8>),
    Stream2(u16),
    Frame2(u32, bool),
}

struct Screen(mpsc::Sender<Seen>);

impl Handler for Screen {
    fn stream(&mut self, info: &StreamInfo) {
        let _ = self.0.send(Seen::Stream(info.epoch));
    }
    fn frame(&mut self, frame: &Frame, _age_us: Option<i64>) {
        let _ = self.0.send(Seen::Frame(frame.number, frame.data.clone()));
    }
    fn stream2(&mut self, info: &StreamInfo) {
        let _ = self.0.send(Seen::Stream2(info.epoch));
    }
    fn frame2(&mut self, frame: &Frame, _age_us: Option<i64>) {
        let _ = self.0.send(Seen::Frame2(frame.number, frame.key));
    }
    fn closed(&mut self, _reason: &str) {}
}

struct Pair {
    dir: std::path::PathBuf,
    host: Host,
    client: Client,
    link: Link,
    seen: mpsc::Receiver<Seen>,
    asked: mpsc::Receiver<&'static str>,
}

fn pair(name: &str, drop_one_in: Option<u32>) -> Pair {
    let dir = std::env::temp_dir().join(format!("remoteman-still-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (links, link) = mpsc::channel();
    let (asked_tx, asked) = mpsc::channel();
    let host = Host::start(HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: dir.join("host"), name: "desk".into(), os: "test".into(), features: vec!["video".into()], pair: true, test_drop_one_in: drop_one_in }, Desk { links, asked: asked_tx }).unwrap();
    let (tx, seen) = mpsc::channel();
    let client = Client::connect(
        Options {
            host: host.address.to_string(),
            state: dir.join("device"),
            name: "device".into(),
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
    let link = link.recv_timeout(Duration::from_secs(5)).unwrap();
    Pair { dir, host, client, link, seen, asked }
}

impl Pair {
    fn end(self) {
        self.client.close();
        self.host.stop();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The third frame is the last before the desktop goes still, and the network loses it whole.
/// No frame follows, so the client can't know there was one: the host has to see to it.
#[test]
fn the_last_frame_before_a_still_desktop_arrives_though_the_network_lost_it() {
    // Every third datagram is thrown away; each frame here is one datagram.
    let p = pair("last", Some(3));
    assert!(p.link.send_frame(b"zero", true, 1));
    assert!(p.link.send_frame(b"one", false, 2));
    assert!(p.link.send_frame(b"two", false, 3));
    let mut frames = Vec::new();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while frames.len() < 3 {
        match p.seen.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
            Ok(Seen::Frame(n, data)) => frames.push((n, data)),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert_eq!(frames, [(0, b"zero".to_vec()), (1, b"one".to_vec()), (2, b"two".to_vec())], "the last frame never reached the client");
    // It was mended without the encoder being asked for anything.
    assert!(p.asked.try_recv().is_err());
    // And a frame that did arrive is not handed over twice when it is sent again.
    std::thread::sleep(Duration::from_millis(400));
    assert!(p.seen.try_recv().is_err(), "something was handed over twice");
    p.end();
}

/// The second display's first frame is a keyframe sent right behind the `stream` message that
/// says what it is, and the two travel apart. On a still desktop no other frame follows, so
/// it has to be shown whichever of the two arrives first.
#[test]
fn a_still_second_display_shows_its_first_frame() {
    for round in 0..40 {
        let p = pair(&format!("second-{round}"), None);
        assert!(p.link.send_frame(b"first", true, 1));
        let second = p.link.second();
        assert!(second.restart(accept()));
        assert!(second.send_frame(b"second", true, 2));
        let mut got = false;
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while let Ok(seen) = p.seen.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
            if seen == Seen::Frame2(0, true) {
                got = true;
                break;
            }
        }
        assert!(got, "round {round}: the second display's only frame was never handed over");
        p.end();
    }
}

/// The same with a last frame of many datagrams of which the network loses some, at the
/// very start of a connection, while the datagram limit is still growing: the copy must fit
/// the pieces the client already holds.
#[test]
fn a_large_last_frame_is_completed_by_its_copy() {
    let p = pair("large", Some(3));
    let large: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
    assert!(p.link.send_frame(b"zero", true, 1));
    assert!(p.link.send_frame(b"one", false, 2));
    assert!(p.link.send_frame(&large, false, 3));
    let mut last = None;
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while let Ok(seen) = p.seen.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
        if let Seen::Frame(2, data) = seen {
            last = Some(data);
            break;
        }
    }
    assert!(last.is_some_and(|data| data == large), "the last frame never arrived whole, or not as it was sent");
    p.end();
}
