//! A connection given its keys rather than taking them from the state folder (`Given`): the
//! host's fingerprint wins over what is kept for the address and over pairing, a host with any
//! other certificate is turned away with nothing kept, and the device's identity given is the
//! one the host sees, with the kept one neither read nor made.

use std::time::Duration;

use remoteman_core::host::{Accept, ClientInfo, Host, HostHandler, HostOptions, Link};
use remoteman_core::{Chroma, Client, Codec, Display, Event, Frame, Given, Handler, Mode, Options, StreamInfo, check_identity, knows_host};

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
    fn pairing(&mut self, _code: &str, _seconds: u32) {
        panic!("a device given the host's key was asked to pair");
    }
    fn closed(&mut self, _reason: &str) {}
}

#[test]
fn the_keys_given_win_over_the_state_folders() {
    let dir = std::env::temp_dir().join(format!("remoteman-given-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let host = Host::start(HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: dir.join("host"), name: "desk".into(), os: "test".into(), features: vec!["video".into()], pair: true, test_drop_one_in: None }, Desk).unwrap();
    let address = host.address.to_string();
    // The identity another program holds for this device, as the files it would pass.
    remoteman_transport::Identity::load_or_create(&dir.join("held")).unwrap();
    let (cert, key) = (std::fs::read(dir.join("held/cert.der")).unwrap(), std::fs::read(dir.join("held/key.der")).unwrap());
    let device = check_identity(&cert, &key).unwrap();
    let state = dir.join("viewer");
    // What the viewer keeps for the address is another host's key.
    let kept = state.join("client/hosts").join(address.replace([':', '/'], "_"));
    std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
    std::fs::write(&kept, "00".repeat(32)).unwrap();
    let options = |pair: bool| Options {
        host: address.clone(),
        state: state.clone(),
        name: "the viewer".into(),
        pair,
        display: Display { width: 640, height: 480, scale: 1.0, refresh: 60, placement: Default::default(), hdr: false, headroom: 0.0 },
        decoders: vec![MODE],
        test_drop_one_in: None,
        test_delay_lost_ms: None,
        cursor: false,
        unlock: None,
        attach_only: false,
    };
    let given = |expect: &str| Given { identity: Some((cert.clone(), key.clone())), expect: Some(expect.to_owned()) };

    // The host's own fingerprint (in capitals too): in, past the key kept for the address, which
    // is left as it was, and as the identity given.
    let client = Client::connect_given(options(false), given(&host.fingerprint.to_ascii_uppercase()), Screen).expect("the host given wasn't accepted");
    assert_eq!(client.host_fingerprint, host.fingerprint);
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), "00".repeat(32));
    assert_eq!(host.devices().iter().map(|d| d.fingerprint.as_str()).collect::<Vec<_>>(), [device.as_str()]);
    assert!(!state.join("client/cert.der").exists() && !state.join("client/key.der").exists(), "an identity was made in the state folder");
    client.close();

    // Any other fingerprint: turned away, even when pairing is asked for and nothing is kept for
    // the address, and nothing is kept afterwards either.
    std::fs::remove_file(&kept).unwrap();
    let wrong = Client::connect_given(options(true), given(&"ab".repeat(32)), Screen).err().expect("a host with another key was accepted").to_string();
    assert!(wrong.contains("isn't the host that was expected"), "{wrong}");
    assert!(!knows_host(&state, &address), "the host that answered was kept");

    // Again with the right one, the host no longer pairing: the identity given is the trusted one.
    std::thread::sleep(Duration::from_millis(100));
    assert!(!host.pairing());
    let again = Client::connect_given(options(false), given(&host.fingerprint), Screen).expect("the identity given wasn't let in again");
    assert!(!knows_host(&state, &address));
    again.close();

    host.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
