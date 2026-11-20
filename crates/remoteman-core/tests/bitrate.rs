//! The core lowers a host's bitrate when the client reports loss, and tells the client.

use std::sync::mpsc;
use std::time::Duration;

use remoteman_core::host::{Accept, ClientInfo, Host, HostHandler, HostOptions, Link};
use remoteman_core::{Chroma, Client, Codec, Display, Event, Frame, Handler, Message, Mode, Options, StreamInfo};

const MODE: Mode = Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 8 };

struct Desk {
    links: mpsc::Sender<Link>,
    rates: mpsc::Sender<u32>,
    follows: bool,
}

impl HostHandler for Desk {
    fn connected(&mut self, _client: &ClientInfo, link: Link) -> Result<Accept, String> {
        let _ = self.links.send(link);
        Ok(Accept { mode: MODE, width: 640, height: 480, refresh: 60, colour: "bt709".into(), sdr_white: 0, peak: 0 })
    }
    fn default_bitrate(&mut self) -> u32 {
        if self.follows { 10_000_000 } else { 0 }
    }
    fn bitrate(&mut self, bits_per_second: u32) {
        let _ = self.rates.send(bits_per_second);
    }
    fn keyframe(&mut self) {}
    fn input(&mut self, _event: Event) {}
    fn left(&mut self) {}
}

struct Screen(mpsc::Sender<Option<u32>>);

impl Handler for Screen {
    fn stream(&mut self, _info: &StreamInfo) {}
    fn frame(&mut self, _frame: &Frame, _age_us: Option<i64>) {}
    fn message(&mut self, message: &Message) {
        if let Message::Status { bitrate } = message {
            let _ = self.0.send(*bitrate);
        }
    }
    fn closed(&mut self, _reason: &str) {}
}

/// A client that throws away every third datagram it gets, so that frames of several datagrams
/// are given up (parity can mend one lost datagram of a group, not two).
fn run(follows: bool) -> (Vec<u32>, Vec<Option<u32>>) {
    let dir = std::env::temp_dir().join(format!("remoteman-bitrate-{follows}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (links, link) = mpsc::channel();
    let (rates_tx, rates) = mpsc::channel();
    let host = Host::start(HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: dir.join("host"), name: "desk".into(), os: "test".into(), features: vec!["video".into()], pair: true, test_drop_one_in: None }, Desk { links, rates: rates_tx, follows }).unwrap();
    let (tx, status) = mpsc::channel();
    let client = Client::connect(
        Options {
            host: host.address.to_string(),
            state: dir.join("device"),
            name: "device".into(),
            pair: true,
            display: Display { width: 640, height: 480, scale: 1.0, refresh: 60, placement: Default::default(), hdr: false, headroom: 0.0 },
            decoders: vec![MODE],
            test_drop_one_in: Some(3),
            test_delay_lost_ms: None,
            cursor: false,
            unlock: None,
            attach_only: false,
        },
        Screen(tx),
    )
    .unwrap();
    let link = link.recv_timeout(Duration::from_secs(5)).unwrap();
    let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    for n in 0..40 {
        link.send_frame(&data, n == 0, n);
        std::thread::sleep(Duration::from_millis(25));
    }
    std::thread::sleep(Duration::from_millis(300));
    let got = (rates.try_iter().collect(), status.try_iter().collect());
    client.close();
    host.stop();
    let _ = std::fs::remove_dir_all(&dir);
    got
}

#[test]
fn loss_lowers_the_bitrate_and_the_client_is_told() {
    let (rates, status) = run(true);
    assert!(!rates.is_empty(), "the host was never told to lower the rate");
    assert_eq!(rates[0], 8_000_000);
    assert!(rates.iter().all(|r| *r < 10_000_000 && *r >= 2_000_000));
    assert_eq!(status.first(), Some(&Some(8_000_000)));
}

#[test]
fn a_host_that_doesnt_follow_is_left_alone() {
    let (rates, status) = run(false);
    assert!(rates.is_empty());
    assert!(status.is_empty());
}
