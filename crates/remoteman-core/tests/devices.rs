//! The host's list of trusted devices, taking one off it, dropping the client without
//! stopping, forgetting a host on the client, and the stages of connecting. No picture is sent.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use remoteman_core::host::{Accept, ClientInfo, Host, HostHandler, HostOptions, Link};
use remoteman_core::{Chroma, Client, Codec, Display, Event, Frame, Handler, Mode, Options, Stage, StreamInfo, forget_host, knows_host};

const MODE: Mode = Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 8 };

struct Desk(mpsc::Sender<&'static str>);

impl HostHandler for Desk {
    fn connected(&mut self, _client: &ClientInfo, _link: Link) -> Result<Accept, String> {
        let _ = self.0.send("connected");
        Ok(Accept { mode: MODE, width: 640, height: 480, refresh: 60, colour: "bt709".into() })
    }
    fn keyframe(&mut self) {}
    fn input(&mut self, _event: Event) {}
    fn left(&mut self) {
        let _ = self.0.send("left");
    }
}

enum Seen {
    Features(Vec<String>),
    Stage(Stage),
    Closed(String),
}

struct Screen(mpsc::Sender<Seen>);

impl Handler for Screen {
    fn stream(&mut self, _info: &StreamInfo) {}
    fn frame(&mut self, _frame: &Frame, _age_us: Option<i64>) {}
    fn stage(&mut self, stage: Stage) {
        let _ = self.0.send(Seen::Stage(stage));
    }
    fn message(&mut self, message: &remoteman_core::Message) {
        if let remoteman_core::Message::Features { features } = message {
            let _ = self.0.send(Seen::Features(features.clone()));
        }
    }
    fn closed(&mut self, reason: &str) {
        let _ = self.0.send(Seen::Closed(reason.to_owned()));
    }
}

fn options(host: &Host, state: &PathBuf, name: &str, pair: bool) -> Options {
    Options {
        host: host.address.to_string(),
        state: state.clone(),
        name: name.into(),
        pair,
        display: Display { width: 640, height: 480, scale: 1.0, refresh: 60, placement: Default::default() },
        decoders: vec![MODE],
        test_drop_one_in: None,
        test_delay_lost_ms: None,
        cursor: false,
        unlock: None,
    }
}

fn closed(seen: &mpsc::Receiver<Seen>) -> String {
    loop {
        match seen.recv_timeout(Duration::from_secs(5)).expect("the client was never told it is over") {
            Seen::Closed(reason) => return reason,
            _ => {}
        }
    }
}

fn next(calls: &mpsc::Receiver<&'static str>) -> &'static str {
    calls.recv_timeout(Duration::from_secs(5)).expect("the host's handler wasn't called")
}

#[test]
fn devices_are_listed_dropped_revoked_and_forgotten() {
    let dir = std::env::temp_dir().join(format!("remoteman-devices-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (host_state, pad, phone) = (dir.join("host"), dir.join("pad"), dir.join("phone"));

    let (calls_tx, calls) = mpsc::channel();
    let host = Host::start(
        HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: host_state, name: "desk".into(), os: "test".into(), features: vec!["video".into()], pair: true, test_drop_one_in: None },
        Desk(calls_tx),
    )
    .unwrap();
    assert!(host.devices().is_empty());
    assert!(!host.drop_client());

    // Pairing: every stage is reported, in order, before connect returns.
    let (seen_tx, seen) = mpsc::channel();
    let client = Client::connect(options(&host, &pad, "the pad", true), Screen(seen_tx)).unwrap();
    assert_eq!(client.close_code(), None);
    let stages: Vec<Stage> = seen.try_iter().filter_map(|s| if let Seen::Stage(s) = s { Some(s) } else { None }).collect();
    assert_eq!(stages, [Stage::Lookup, Stage::Handshake, Stage::Hello, Stage::Display]);
    assert_eq!(next(&calls), "connected");
    let devices = host.devices();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "the pad");
    assert!(devices[0].connected);
    let pad_fingerprint = devices[0].fingerprint.clone();

    // What the host can do changes while connected: the client is told, and stays.
    host.set_features(vec!["video".into(), "clipboard".into()]);
    let told = loop {
        match seen.recv_timeout(Duration::from_secs(5)).expect("the client wasn't told of the new features") {
            Seen::Features(f) => break f,
            _ => {}
        }
    };
    assert_eq!(told, ["video", "clipboard"]);

    // Dropped: the client is told why, the host keeps listening and still trusts it.
    assert!(host.drop_client());
    assert!(closed(&seen).contains("the host ended the connection"));
    assert_eq!(client.close_code(), Some(remoteman_core::CLOSE_DROPPED));
    assert_eq!(next(&calls), "left");
    client.close();
    assert_eq!(host.devices().len(), 1);
    assert!(!host.devices()[0].connected);

    // A second device pairs; the first comes back without pairing.
    host.set_pairing(true);
    let (seen_tx, _seen_phone) = mpsc::channel();
    let phone_client = Client::connect(options(&host, &phone, "the phone", true), Screen(seen_tx)).unwrap();
    assert_eq!(next(&calls), "connected");
    assert_eq!(host.devices().iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["the pad", "the phone"]);
    let (seen_tx, seen) = mpsc::channel();
    let client = Client::connect(options(&host, &pad, "the pad", false), Screen(seen_tx)).unwrap();
    assert_eq!(next(&calls), "left");
    assert_eq!(next(&calls), "connected");
    phone_client.close();

    // Revoked while connected: dropped with the reason, off the list, refused afterwards.
    assert!(host.revoke(&pad_fingerprint).unwrap());
    assert!(closed(&seen).contains("no longer trusted"));
    assert_eq!(client.close_code(), Some(remoteman_core::CLOSE_REVOKED));
    assert_eq!(next(&calls), "left");
    client.close();
    assert!(!host.revoke(&pad_fingerprint).unwrap());
    assert_eq!(host.devices().iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["the phone"]);
    let (seen_tx, seen) = mpsc::channel();
    let refused = Client::connect(options(&host, &pad, "the pad", false), Screen(seen_tx));
    assert!(refused.is_err(), "a revoked device got in");
    let last = seen.try_iter().filter_map(|s| if let Seen::Stage(s) = s { Some(s) } else { None }).last();
    assert!(matches!(last, Some(Stage::Handshake | Stage::Hello)), "it failed at {last:?}");

    // Forgetting the host on the device: it has to pair again.
    let address = host.address.to_string();
    assert!(knows_host(&phone, &address));
    assert!(forget_host(&phone, &address).unwrap());
    assert!(!knows_host(&phone, &address));
    assert!(!forget_host(&phone, &address).unwrap());
    let (seen_tx, _seen) = mpsc::channel();
    let unpaired = Client::connect(options(&host, &phone, "the phone", false), Screen(seen_tx));
    assert!(unpaired.err().is_some_and(|e| e.to_string().contains("isn't paired")));

    host.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
