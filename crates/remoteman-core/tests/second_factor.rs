//! The second factor (docs/PROTOCOL.md, "Second factor"): with it on, a paired device's hello says
//! so and nothing else comes until a right code from the authenticator app; wrong codes are
//! counted and answered; "don't ask again" is remembered; a client that never answers is closed.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use remoteman_core::host::{Accept, ClientInfo, Host, HostHandler, HostOptions, Link};
use remoteman_core::second_factor::{totp, unbase32};
use remoteman_core::{Chroma, Client, Codec, Display, Frame, Handler, Mode, Options, StreamInfo};

const MODE: Mode = Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 8 };

struct Desk(Arc<AtomicU32>);

impl HostHandler for Desk {
    fn connected(&mut self, _client: &ClientInfo, _link: Link) -> Result<Accept, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Accept { mode: MODE, width: 640, height: 480, refresh: 60, colour: "bt709".into(), sdr_white: 0, peak: 0 })
    }
    fn keyframe(&mut self) {}
    fn lost(&mut self, _first: u32, _last: u32) {}
    fn input(&mut self, _event: remoteman_core::Event) {}
    fn left(&mut self) {}
}

#[derive(Clone, Default)]
struct Seen {
    streams: Arc<AtomicU32>,
    closed: Arc<Mutex<Option<String>>>,
}

struct Screen(Seen);

impl Handler for Screen {
    fn stream(&mut self, _info: &StreamInfo) {
        self.0.streams.fetch_add(1, Ordering::SeqCst);
    }
    fn frame(&mut self, _frame: &Frame, _age_us: Option<i64>) {}
    fn closed(&mut self, reason: &str) {
        *self.0.closed.lock().unwrap() = Some(reason.to_owned());
    }
}

struct Rig {
    dir: std::path::PathBuf,
    host: Host,
    /// How often the host's handler was asked to make a display.
    connected: Arc<AtomicU32>,
}

fn start(name: &str) -> Rig {
    let dir = std::env::temp_dir().join(format!("remoteman-2fa-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let connected = Arc::new(AtomicU32::new(0));
    let features = ["video", "input"].iter().map(|f| f.to_string()).collect();
    let host = Host::start(HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: dir.join("host"), name: "desk".into(), os: "test".into(), features, pair: true, test_drop_one_in: None }, Desk(connected.clone())).unwrap();
    Rig { dir, host, connected }
}

fn options(rig: &Rig) -> Options {
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
    }
}

/// Pair a device (the second factor still off), then turn the second factor on; the secret.
fn paired_with_factor(rig: &Rig) -> Vec<u8> {
    let seen = Seen::default();
    let client = Client::connect(options(rig), Screen(seen)).unwrap();
    assert!(!client.host_second_factor);
    client.close();
    let setup = rig.host.enable_second_factor().unwrap();
    assert!(setup.uri.starts_with("otpauth://totp/remoteman:desk?secret=") && setup.uri.ends_with("&issuer=remoteman"));
    let secret = unbase32(&setup.secret).unwrap();
    assert!(rig.host.second_factor().is_none(), "not on before a code confirms it");
    assert!(!rig.host.confirm_second_factor("000000") || totp(&secret, remoteman_core::second_factor::now()) == 0);
    assert!(rig.host.confirm_second_factor(&format!("{:06}", totp(&secret, remoteman_core::second_factor::now()))));
    assert!(rig.host.second_factor().is_some());
    rig.connected.store(0, Ordering::SeqCst);
    secret
}

fn code(secret: &[u8]) -> String {
    format!("{:06}", totp(secret, remoteman_core::second_factor::now()))
}

/// A code that is right at no time in the next hour.
fn wrong(secret: &[u8]) -> String {
    let now = remoteman_core::second_factor::now();
    (0..1_000_000u32).map(|n| format!("{n:06}")).find(|c| (0..120).all(|i| !remoteman_core::second_factor::verify(secret, c, now + i * 30))).unwrap()
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..100 {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

fn asked(client: &Client) -> mpsc::Receiver<Option<u32>> {
    let (tx, rx) = mpsc::channel();
    client.on_second_factor(move |left| {
        let _ = tx.send(left);
    });
    rx
}

#[test]
fn nothing_comes_before_the_code_and_a_right_one_lets_the_session_go_on() {
    let rig = start("flow");
    let secret = paired_with_factor(&rig);
    let seen = Seen::default();
    let client = Client::connect(options(&rig), Screen(seen.clone())).unwrap();
    assert!(client.host_second_factor);
    let rx = asked(&client);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), None, "asked the first time");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(seen.streams.load(Ordering::SeqCst), 0, "no stream before the code");
    assert_eq!(rig.connected.load(Ordering::SeqCst), 0, "the host's handler hasn't heard of the client");

    client.second_factor(&wrong(&secret), false);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Some(4));
    assert_eq!(seen.streams.load(Ordering::SeqCst), 0);

    client.second_factor(&code(&secret), true);
    wait_for("the stream", || seen.streams.load(Ordering::SeqCst) == 1);
    assert_eq!(rig.connected.load(Ordering::SeqCst), 1);
    let events: Vec<String> = rig.host.connections(50).into_iter().map(|e| e.event).collect();
    assert!(events.iter().any(|e| e == "second_factor_wrong") && events.iter().any(|e| e == "second_factor_ok"), "{events:?}");
    assert_eq!(rig.host.second_factor().unwrap().remembered, 1);
    client.close();

    // Told "don't ask again": the next connection isn't asked.
    let seen = Seen::default();
    let client = Client::connect(options(&rig), Screen(seen.clone())).unwrap();
    assert!(!client.host_second_factor);
    wait_for("the stream", || seen.streams.load(Ordering::SeqCst) == 1);
    client.close();

    // Forgotten: asked again.
    rig.host.forget_remembered().unwrap();
    let client = Client::connect(options(&rig), Screen(Seen::default())).unwrap();
    assert!(client.host_second_factor);
    client.close();

    // Turned off: not asked, and the state forgets the secret.
    rig.host.disable_second_factor().unwrap();
    let seen = Seen::default();
    let client = Client::connect(options(&rig), Screen(seen.clone())).unwrap();
    assert!(!client.host_second_factor);
    wait_for("the stream", || seen.streams.load(Ordering::SeqCst) == 1);
    client.close();
    let _ = std::fs::remove_dir_all(&rig.dir);
}

#[test]
fn five_wrong_codes_close_the_connection_and_the_device_is_refused_for_a_while() {
    let rig = start("lockout");
    let secret = paired_with_factor(&rig);
    let seen = Seen::default();
    let client = Client::connect(options(&rig), Screen(seen.clone())).unwrap();
    let rx = asked(&client);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), None);
    for left in [4, 3, 2, 1] {
        client.second_factor(&wrong(&secret), false);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Some(left));
    }
    client.second_factor(&wrong(&secret), false);
    wait_for("the close", || seen.closed.lock().unwrap().is_some());
    let reason = seen.closed.lock().unwrap().clone().unwrap();
    assert!(reason.starts_with("second_factor: "), "{reason}");
    assert_eq!(rig.connected.load(Ordering::SeqCst), 0);
    client.close();

    // Refused at the hello: the connect itself fails with the reason, before any code is asked.
    let err = Client::connect(options(&rig), Screen(Seen::default())).err().expect("refused");
    assert!(format!("{err:#}").contains("second_factor"), "{err:#}");    let _ = std::fs::remove_dir_all(&rig.dir);
}

#[test]
fn a_client_that_never_answers_is_told_to_update() {
    let rig = start("old");
    let _ = paired_with_factor(&rig);
    rig.host.set_second_factor_wait(Duration::from_millis(800));
    let seen = Seen::default();
    // (No callback set, as a client that doesn't know the field would have none.)
    let client = Client::connect(options(&rig), Screen(seen.clone())).unwrap();
    wait_for("the close", || seen.closed.lock().unwrap().is_some());
    let reason = seen.closed.lock().unwrap().clone().unwrap();
    assert!(reason.contains("This computer asks for a code from an authenticator app; update remoteman"), "{reason}");
    assert_eq!(rig.connected.load(Ordering::SeqCst), 0);
    client.close();
    let _ = std::fs::remove_dir_all(&rig.dir);
}
