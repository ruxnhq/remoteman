//! A new device that asks to pair while nobody is there to answer (a Windows service at the
//! sign-in screen, with the owner's app not running) is refused cleanly, soon, and keeps nothing.

use std::time::{Duration, Instant};

use remoteman_core::host::{Accept, ClientInfo, Host, HostHandler, HostOptions, Link};
use remoteman_core::{Chroma, Client, Codec, Display, Event, Frame, Handler, Mode, Options, StreamInfo, knows_host};

const MODE: Mode = Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 8 };

struct Desk;

impl HostHandler for Desk {
    fn connected(&mut self, _client: &ClientInfo, _link: Link) -> Result<Accept, String> {
        Ok(Accept { mode: MODE, width: 640, height: 480, refresh: 60, colour: "bt709".into(), sdr_white: 0, peak: 0 })
    }
    fn keyframe(&mut self) {}
    fn input(&mut self, _event: Event) {}
    fn left(&mut self) {}
}

struct Screen;

impl Handler for Screen {
    fn stream(&mut self, _info: &StreamInfo) {}
    fn frame(&mut self, _frame: &Frame, _age_us: Option<i64>) {}
    fn closed(&mut self, _reason: &str) {}
}

#[test]
fn a_device_is_refused_soon_when_nobody_is_there_to_accept_it() {
    let dir = std::env::temp_dir().join(format!("remoteman-approver-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let host = Host::start(HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: dir.join("host"), name: "desk".into(), os: "test".into(), features: vec!["video".into()], pair: false, test_drop_one_in: None }, Desk).unwrap();
    host.set_confirm(true);
    host.set_pairing(true);
    host.set_approver_wait(Some(Duration::from_millis(1200)));
    let options = Options {
        host: host.address.to_string(),
        state: dir.join("phone"),
        name: "the phone".into(),
        pair: true,
        display: Display { width: 640, height: 480, scale: 1.0, refresh: 60, placement: Default::default(), hdr: false, headroom: 0.0 },
        decoders: vec![MODE],
        test_drop_one_in: None,
        test_delay_lost_ms: None,
        cursor: false,
        unlock: None,
        attach_only: false,
    };
    let began = Instant::now();
    let refused = Client::connect(options, Screen).err().expect("nobody answered, yet the device got in");
    assert!(began.elapsed() < Duration::from_secs(10), "waited the whole minute");
    assert!(format!("{refused:#}").contains("Nobody is signed in on this PC to accept the device"), "{refused:#}");
    assert!(!knows_host(&dir.join("phone"), &host.address.to_string()));
    assert!(host.devices().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
