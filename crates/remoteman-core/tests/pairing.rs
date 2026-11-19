//! Pairing that waits for the host's owner: a new device is told to wait and can show the same
//! code as the host; it is trusted only after a yes, and turned away (and keeps nothing) after
//! a no.

use std::sync::mpsc;
use std::time::Duration;

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

/// Says the code it was shown.
struct Screen(mpsc::Sender<String>);

impl Handler for Screen {
    fn stream(&mut self, _info: &StreamInfo) {}
    fn frame(&mut self, _frame: &Frame, _age_us: Option<i64>) {}
    fn pairing(&mut self, code: &str, seconds: u32) {
        assert!(seconds > 0);
        let _ = self.0.send(code.to_owned());
    }
    fn closed(&mut self, _reason: &str) {}
}

#[test]
fn a_new_device_waits_for_the_owners_word() {
    let dir = std::env::temp_dir().join(format!("remoteman-pairing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let host = Host::start(HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: dir.join("host"), name: "desk".into(), os: "test".into(), features: vec!["video".into()], pair: false, test_drop_one_in: None }, Desk).unwrap();
    let address = host.address.to_string();
    let options = |state: &str, name: &str| Options {
        host: address.clone(),
        state: dir.join(state),
        name: name.into(),
        pair: true,
        display: Display { width: 640, height: 480, scale: 1.0, refresh: 60, placement: Default::default(), hdr: false, headroom: 0.0 },
        decoders: vec![MODE],
        test_drop_one_in: None,
        test_delay_lost_ms: None,
        cursor: false,
        unlock: None,
        attach_only: false,
    };
    // The owner's answer comes from another thread while the device waits in `connect`.
    let answer = |host: &Host, shown: &mpsc::Receiver<String>, accept: bool| {
        let code = shown.recv_timeout(Duration::from_secs(5)).expect("the device wasn't told to wait");
        let pending = host.pending().expect("nothing is waiting on the host");
        assert_eq!(pending.code, code, "the two sides show different codes");
        assert_eq!(code.len(), 7, "{code}");
        assert!(host.devices().is_empty() || host.devices().iter().all(|d| d.fingerprint != pending.fingerprint), "trusted before the answer");
        assert!(host.decide(accept));
        pending
    };

    // Declined: turned away with the reason, not trusted, and the device keeps no key.
    host.set_confirm(true);
    host.set_pairing(true);
    let (tx, shown) = mpsc::channel();
    let refused = std::thread::scope(|s| {
        let trying = s.spawn(|| Client::connect(options("phone", "the phone"), Screen(tx)));
        let pending = answer(&host, &shown, false);
        assert_eq!(pending.name, "the phone");
        trying.join().unwrap()
    });
    assert!(refused.err().is_some_and(|e| e.to_string().contains("declined")));
    assert!(host.devices().is_empty());
    assert!(host.pending().is_none());
    assert!(!knows_host(&dir.join("phone"), &address));
    assert!(!host.pairing(), "pairing stayed open after a device used it");

    // Accepted: in, and trusted under the name it gave.
    host.set_pairing(true);
    let (tx, shown) = mpsc::channel();
    let client = std::thread::scope(|s| {
        let trying = s.spawn(|| Client::connect(options("pad", "the pad"), Screen(tx)));
        answer(&host, &shown, true);
        trying.join().unwrap()
    })
    .expect("an accepted device got no session");
    assert_eq!(host.devices().iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["the pad"]);
    assert!(knows_host(&dir.join("pad"), &address));
    client.close();

    // Known from then on: no question the next time, pairing open or not.
    let (tx, shown) = mpsc::channel();
    let again = Client::connect(Options { pair: false, ..options("pad", "the pad") }, Screen(tx)).expect("a paired device was not let in");
    assert!(shown.try_recv().is_err(), "a paired device was asked to wait");
    again.close();

    // Without the owner's word being asked for, a new device is in at once, as before.
    host.set_confirm(false);
    host.set_pairing(true);
    let (tx, shown) = mpsc::channel();
    let quick = Client::connect(options("tablet", "the tablet"), Screen(tx)).unwrap();
    assert!(shown.try_recv().is_err());
    assert_eq!(host.devices().len(), 2);
    quick.close();

    host.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
