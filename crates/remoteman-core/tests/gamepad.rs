//! A game controller's state goes from the client to the host's handler as it was sent, and only to a host that
//! lists `gamepad` in its features.

use std::sync::mpsc;
use std::time::Duration;

use remoteman_core::host::{Accept, ClientInfo, Host, HostHandler, HostOptions, Link};
use remoteman_core::{Chroma, Client, Codec, Display, Event, Frame, Handler, Mode, Options, StreamInfo};

const MODE: Mode = Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 8 };

struct Desk {
    input: mpsc::Sender<Event>,
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
    fn left(&mut self) {}
}

struct Screen;

impl Handler for Screen {
    fn stream(&mut self, _info: &StreamInfo) {}
    fn frame(&mut self, _frame: &Frame, _age_us: Option<i64>) {}
    fn closed(&mut self, _reason: &str) {}
}

fn connect(name: &str, features: &[&str]) -> (std::path::PathBuf, Host, Client, mpsc::Receiver<Event>, mpsc::Receiver<Link>) {
    let dir = std::env::temp_dir().join(format!("remoteman-pad-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (input_tx, input) = mpsc::channel();
    let (links, link) = mpsc::channel();
    let host = Host::start(
        HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: dir.join("host"), name: "desk".into(), os: "test".into(), features: features.iter().map(|f| f.to_string()).collect(), pair: true, test_drop_one_in: None },
        Desk { input: input_tx, links },
    )
    .unwrap();
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
        Screen,
    )
    .unwrap();
    (dir, host, client, input, link)
}

fn pad(buttons: u16, lt: f32) -> Event {
    Event::Gamepad { pad: 0, flags: 1, buttons, left_trigger: lt, right_trigger: 0.0, left_x: -0.5, left_y: 1.0, right_x: 0.25, right_y: -1.0 }
}

#[test]
fn a_gamepad_state_reaches_a_host_that_lists_gamepad_as_it_was_sent() {
    let (dir, host, client, input, _link) = connect("yes", &["video", "input", "gamepad"]);
    assert!(client.host_has_gamepad());
    let unplugged = Event::Gamepad { pad: 0, flags: 0, buttons: 0, left_trigger: 0.0, right_trigger: 0.0, left_x: 0.0, left_y: 0.0, right_x: 0.0, right_y: 0.0 };
    let sent = [pad(0, 0.0), pad(0x1000, 0.5), pad(0x9001, 1.0), unplugged];
    for e in &sent {
        client.send(e.clone());
    }
    for e in &sent {
        assert_eq!(&input.recv_timeout(Duration::from_secs(5)).expect("the gamepad state never arrived"), e);
    }
    client.close();
    host.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_client_sends_no_gamepad_to_a_host_that_doesnt_list_it() {
    let (dir, host, client, input, _link) = connect("no", &["video", "input"]);
    assert!(!client.host_has_gamepad());
    client.send(pad(0x1000, 0.5));
    client.send(Event::PointerTo { x: 0.5, y: 0.5 });
    assert_eq!(input.recv_timeout(Duration::from_secs(5)).unwrap(), Event::PointerTo { x: 0.5, y: 0.5 });
    assert!(input.recv_timeout(Duration::from_millis(300)).is_err());
    client.close();
    host.stop();
    let _ = std::fs::remove_dir_all(&dir);
}