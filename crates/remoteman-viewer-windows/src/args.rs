//! The command line.
//!
//! ```text
//! remoteman [HOST:PORT] [options]
//! remoteman connect ADDRESS:PORT --expect FINGERPRINT --identity CERT.der KEY.der [--placement extend|main|only|mirror] [options]
//! ```
//!
//! The second is for a program that holds the keys itself (Guesthouse, for its virtual
//! machines): the computer's fingerprint and this PC's identity for it are given, not paired,
//! and win over what the state folder keeps for that computer, which they leave as it is.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use remoteman_core::{Chroma, Codec, Mode, Placement};

/// What `remoteman connect` gives for the computer it names, for every connection to it in this
/// run (trying again, connecting again for another placement).
#[derive(Clone)]
pub struct Given {
    /// The computer, `address:port`, as the core is given it.
    pub address: String,
    /// Its certificate's fingerprint, 64 hex digits in lower case: only that computer is
    /// accepted, with no pairing.
    pub expect: String,
    /// This PC's certificate and key for it (DER), instead of the identity in the state folder.
    pub cert: Vec<u8>,
    pub key: Vec<u8>,
    /// Where its display goes, whatever was chosen for it before; `None` once a placement is
    /// picked in the session.
    pub placement: Option<Placement>,
}

impl Given {
    /// As the core takes it.
    pub fn core(&self) -> remoteman_core::Given {
        remoteman_core::Given { identity: Some((self.cert.clone(), self.key.clone())), expect: Some(self.expect.clone()) }
    }
}

pub struct Args {
    /// A computer to connect to at once; `None`: the app opens on its list.
    pub host: Option<String>,
    /// `remoteman connect`: the keys and placement given for `host`.
    pub given: Option<Given>,
    pub pair: bool,
    pub state: PathBuf,
    /// `None`: this monitor's own (or what the computer's settings say).
    pub size: Option<(u32, u32)>,
    /// `None`: the monitor's own rate.
    pub refresh: Option<u32>,
    /// What to offer the host if the card decodes it, most preferred first.
    pub decoders: Vec<Mode>,
    pub seconds: Option<u64>,
    pub fullscreen: bool,
    /// Full screen draws its bar and menus with the old kit (selectable; the app's WinUI bar is the default).
    pub kit_bar: bool,
    pub shot: Option<PathBuf>,
    pub stats: bool,
    pub input_demo: bool,
    pub log: Option<PathBuf>,
    /// Pictures of the window with nobody there: the screens and states to go through, by
    /// name (`walk.rs`), with a made-up list and no network. `all`: every one.
    pub ui: Option<String>,
    /// Where those pictures go.
    pub shots: Option<PathBuf>,
    /// With `--ui`: the dark colours.
    pub dark: Option<bool>,
    /// Start the computer's sound with the session.
    pub sound: bool,
    /// Add a second display once the session has started.
    pub second: bool,
    /// For tests: a file whose first line is typed as the password of a locked computer.
    pub password_from: Option<PathBuf>,
    /// For tests: say the screen is (or isn't) HDR whatever Windows says.
    pub screen_hdr: Option<bool>,
    /// For tests: play this HEVC (Annex B, with access unit delimiters) as a made-up session's
    /// picture, through the same decoding and drawing; `--play-colour bt2020-pq` for HDR.
    pub play: Option<PathBuf>,
    pub play_colour: String,
    /// For tests: the mode of what `--play` plays (`hevc`, `hevc444`), and how many pictures
    /// a second it is fed at (0: as fast as they are decoded).
    pub play_mode: Mode,
    pub play_rate: u32,
    /// List the card's decoder profiles and leave.
    pub profiles: bool,
    /// For tests: the monitor's refreshes in the log (`monitor::watch_vblanks`).
    pub watch_vblank: bool,
    /// For tests, the old program only: the variable refresh control (`probe.rs`), for so
    /// many seconds.
    pub vrr_probe: Option<u64>,
    /// For tests: what the made-up session's computer says it can do besides `video`
    /// (`--play-features mic,camera`), so those controls can be tried with no computer.
    pub play_features: Vec<String>,
    /// For tests: browse for this DNS-SD service instead of remoteman's (`_name._udp.local`),
    /// so what a test advertises shows as nearby without a remoteman host on the network.
    pub browse: Option<String>,
}

const USAGE: &str = "usage: remoteman [HOST:PORT] [--pair] [--state DIR] [--size WxH] [--refresh HZ] [--decoders av1,hevc] [--seconds N] [--fullscreen] [--shot FILE.png] [--stats] [--input-demo] [--log FILE] [--sound] [--ui STEPS --shots DIR [--dark|--light]] [--profiles] [--vrr-probe SECONDS] [--watch-vblank] [--screen-hdr yes|no] [--play FILE [--play-colour bt709|bt2020-pq] [--play-mode hevc|hevc444] [--play-rate N] [--play-features mic,camera]] [--browse _name._udp.local]\n       remoteman connect ADDRESS:PORT --expect FINGERPRINT --identity CERT.der KEY.der [--placement extend|main|only|mirror] [the same options]";

const CONNECT_USAGE: &str = "remoteman connect ADDRESS:PORT --expect FINGERPRINT --identity CERT.der KEY.der [--placement extend|main|only|mirror] [--state DIR]";

fn mode(word: &str) -> Option<Mode> {
    // 4:4:4 is decoded on the processor (this FFmpeg's HEVC decoder has no D3D11VA path for
    // it, though the card lists the profile) and offered where that keeps up (`App::withholds_444`).
    Some(match word {
        "av1" => Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 },
        "hevc" => Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 },
        "hevc444" => Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 },
        _ => return None,
    })
}

/// Where the identity and the known hosts live when `--state` doesn't say.
fn default_state() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into()).join("remoteman")
}

pub fn parse() -> Result<Args> {
    parse_from(std::env::args().skip(1).collect())
}

/// The same from words given, for the engine (the WinUI 3 app passes on what is its own).
pub fn parse_from(words: Vec<String>) -> Result<Args> {
    let mut words = words.into_iter().peekable();
    let connect = words.next_if(|w| w == "connect").is_some();
    let host = words.next_if(|h| !h.starts_with("--"));
    // `connect`'s own, checked once everything is read.
    let (mut expect, mut identity, mut placement) = (None::<String>, None::<(PathBuf, PathBuf)>, None::<String>);
    let mut args = Args {
        host,
        given: None,
        pair: false,
        state: default_state(),
        size: None,
        refresh: None,
        decoders: vec![mode("av1").unwrap(), mode("hevc").unwrap(), mode("hevc444").unwrap()],
        seconds: None,
        fullscreen: false,
        kit_bar: false,
        shot: None,
        stats: false,
        input_demo: false,
        log: None,
        ui: None,
        shots: None,
        dark: None,
        sound: false,
        second: false,
        password_from: None,
        screen_hdr: None,
        play: None,
        play_colour: "bt709".into(),
        play_mode: mode("hevc").unwrap(),
        play_rate: 60,
        profiles: false,
        watch_vblank: false,
        vrr_probe: None,
        play_features: Vec::new(),
        browse: None,
    };
    while let Some(arg) = words.next() {
        let mut value = || words.next().ok_or_else(|| anyhow!("{arg} needs a value"));
        match arg.as_str() {
            "--pair" => args.pair = true,
            "--state" => args.state = value()?.into(),
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').context("--size WxH")?;
                args.size = Some((w.parse()?, h.parse()?));
            }
            "--refresh" => args.refresh = Some(value()?.parse()?),
            "--decoders" => {
                let v = value()?;
                args.decoders = v.split(',').map(|w| mode(w).ok_or_else(|| anyhow!("unknown decoder {w}"))).collect::<Result<_>>()?;
            }
            "--seconds" => args.seconds = Some(value()?.parse()?),
            "--fullscreen" => args.fullscreen = true,
            "--kit-bar" => args.kit_bar = true,
            "--shot" => args.shot = Some(value()?.into()),
            "--stats" => args.stats = true,
            "--input-demo" => args.input_demo = true,
            "--log" => args.log = Some(value()?.into()),
            "--ui" => args.ui = Some(value()?),
            "--shots" => args.shots = Some(value()?.into()),
            "--dark" => args.dark = Some(true),
            "--light" => args.dark = Some(false),
            "--sound" => args.sound = true,
            "--second" => args.second = true,
            "--password-from" => args.password_from = Some(value()?.into()),
            "--screen-hdr" => args.screen_hdr = Some(value()? == "yes"),
            "--play" => args.play = Some(value()?.into()),
            "--play-colour" => args.play_colour = value()?,
            "--play-mode" => {
                let v = value()?;
                args.play_mode = mode(&v).ok_or_else(|| anyhow!("unknown mode {v}"))?;
            }
            "--play-rate" => args.play_rate = value()?.parse()?,
            "--profiles" => args.profiles = true,
            "--watch-vblank" => args.watch_vblank = true,
            "--vrr-probe" => args.vrr_probe = Some(value()?.parse()?),
            "--browse" => args.browse = Some(value()?),
            "--play-features" => args.play_features = value()?.split(',').map(str::to_owned).filter(|f| !f.is_empty()).collect(),
            "--expect" | "--identity" | "--placement" if !connect => bail!("{arg} is for remoteman connect: {CONNECT_USAGE}"),
            "--expect" => expect = Some(value()?),
            "--identity" => identity = Some((value()?.into(), words.next().ok_or_else(|| anyhow!("--identity needs the certificate and the key: --identity CERT.der KEY.der"))?.into())),
            "--placement" => placement = Some(value()?),
            other => bail!("unknown option {other}\n{USAGE}"),
        }
    }
    anyhow::ensure!(args.ui.is_none() || args.shots.is_some(), "--ui needs --shots DIR");
    if connect {
        args.given = Some(given(args.host.as_deref(), expect, identity, placement)?);
    }
    Ok(args)
}

/// `remoteman connect`'s words, checked: the computer with its port, its fingerprint, files that
/// can be read and hold a certificate and its key, a placement there is.
fn given(host: Option<&str>, expect: Option<String>, identity: Option<(PathBuf, PathBuf)>, placement: Option<String>) -> Result<Given> {
    let address = host.ok_or_else(|| anyhow!("connect needs the computer: {CONNECT_USAGE}"))?.trim();
    let port_given = address.rsplit_once(':').is_some_and(|(h, p)| !h.is_empty() && p.parse::<u16>().is_ok_and(|p| p > 0));
    anyhow::ensure!(port_given, "connect needs the computer as ADDRESS:PORT (as 172.29.1.2:7791), not {address}");
    let expect = expect.ok_or_else(|| anyhow!("connect needs the computer's fingerprint: --expect FINGERPRINT"))?;
    let expect = expect.trim().to_ascii_lowercase();
    let fingerprint = "--expect takes the computer's fingerprint: the SHA-256 of its certificate, as 64 hex digits";
    anyhow::ensure!(expect.bytes().all(|b| b.is_ascii_hexdigit()), "{fingerprint}; this has characters that aren't hex digits");
    anyhow::ensure!(expect.len() == 64, "{fingerprint}; this has {}", expect.len());
    let (cert_path, key_path) = identity.ok_or_else(|| anyhow!("connect needs this PC's identity for the computer: --identity CERT.der KEY.der"))?;
    let read = |what: &str, path: &Path| std::fs::read(path).with_context(|| format!("can't read the {what} {}", path.display()));
    let (cert, key) = (read("certificate", &cert_path)?, read("key", &key_path)?);
    remoteman_core::check_identity(&cert, &key).with_context(|| format!("--identity {} {} isn't a certificate and its key", cert_path.display(), key_path.display()))?;
    let placement = match placement {
        Some(p) => Some(crate::store::placement_from_key(&p).ok_or_else(|| anyhow!("--placement is extend, main, only or mirror, not {p}"))?),
        None => None,
    };
    Ok(Given { address: crate::store::with_port(address), expect, cert, key, placement })
}

impl Args {
    /// What `remoteman connect` gave for the computer at `address`, if it named that one.
    pub fn given_for(&self, address: &str) -> Option<&Given> {
        self.given.as_ref().filter(|g| g.address == address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use remoteman_core::host::{Accept, ClientInfo, Host, HostHandler, HostOptions, Link};

    struct Nobody;

    impl HostHandler for Nobody {
        fn connected(&mut self, _client: &ClientInfo, _link: Link) -> Result<Accept, String> {
            Err("no".into())
        }
        fn keyframe(&mut self) {}
        fn input(&mut self, _event: remoteman_core::Event) {}
        fn left(&mut self) {}
    }

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    fn error(line: &str) -> String {
        format!("{:#}", parse_from(words(line)).err().expect(line))
    }

    #[test]
    fn connect_takes_the_keys_and_checks_them() {
        let dir = std::env::temp_dir().join(format!("remoteman-args-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A certificate and its key as remoteman keeps them: a host makes its own in `host`.
        Host::start(HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: dir.clone(), name: "made".into(), os: "test".into(), features: Vec::new(), pair: false, test_drop_one_in: None }, Nobody).unwrap().stop();
        let (cert, key, d) = (dir.join("host").join("cert.der"), dir.join("host").join("key.der"), dir.display());
        let (c, k) = (cert.display(), key.display());
        let fp = "AB".repeat(32);

        let args = parse_from(words(&format!("connect 172.29.1.2:7791 --expect {fp} --identity {c} {k} --placement main --state {d} --log {d}/log"))).unwrap();
        assert_eq!(args.host.as_deref(), Some("172.29.1.2:7791"));
        assert_eq!(args.state, dir);
        let given = args.given_for("172.29.1.2:7791").expect("nothing given for the computer named");
        assert_eq!(given.expect, "ab".repeat(32));
        assert_eq!((given.cert.clone(), given.key.clone()), (std::fs::read(&cert).unwrap(), std::fs::read(&key).unwrap()));
        assert_eq!(given.placement, Some(Placement::Main));
        assert!(args.given_for("172.29.1.3:7791").is_none(), "given for a computer it didn't name");
        let core = given.core();
        assert_eq!(core.expect.as_deref(), Some("ab".repeat(32).as_str()));
        assert!(core.identity.is_some());
        // No placement: none given, the computer's own goes on.
        assert!(parse_from(words(&format!("connect [fe80::1]:7791 --expect {fp} --identity {c} {k}"))).unwrap().given.unwrap().placement.is_none());

        // What is wrong is said, plainly.
        assert!(error(&format!("connect --expect {fp} --identity {c} {k}")).contains("connect needs the computer"));
        assert!(error(&format!("connect 172.29.1.2 --expect {fp} --identity {c} {k}")).contains("ADDRESS:PORT"));
        assert!(error(&format!("connect 172.29.1.2:7791 --identity {c} {k}")).contains("--expect FINGERPRINT"));
        assert!(error(&format!("connect 172.29.1.2:7791 --expect abcd --identity {c} {k}")).ends_with("as 64 hex digits; this has 4"));
        assert!(error(&format!("connect 172.29.1.2:7791 --expect {} --identity {c} {k}", "zz".repeat(32))).contains("aren't hex digits"));
        assert!(error(&format!("connect 172.29.1.2:7791 --expect {fp}")).contains("--identity CERT.der KEY.der"));
        assert!(error(&format!("connect 172.29.1.2:7791 --expect {fp} --identity {c}")).contains("the certificate and the key"));
        assert!(error(&format!("connect 172.29.1.2:7791 --expect {fp} --identity {d}/none.der {k}")).contains("can't read the certificate"));
        assert!(error(&format!("connect 172.29.1.2:7791 --expect {fp} --identity {c} {d}/none.der")).contains("can't read the key"));
        assert!(error(&format!("connect 172.29.1.2:7791 --expect {fp} --identity {k} {k}")).contains("isn't a certificate and its key"));
        assert!(error(&format!("connect 172.29.1.2:7791 --expect {fp} --identity {c} {k} --placement beside")).contains("extend, main, only or mirror, not beside"));
        // Without `connect` they are nobody's.
        assert!(error(&format!("172.29.1.2:7791 --expect {fp}")).contains("is for remoteman connect"));
        assert!(error("172.29.1.2:7791 --placement main").contains("is for remoteman connect"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_old_way_still_starts() {
        let args = parse_from(words("172.29.1.2:7791 --state C:/somewhere --pair")).unwrap();
        assert_eq!(args.host.as_deref(), Some("172.29.1.2:7791"));
        assert_eq!(args.state, PathBuf::from("C:/somewhere"));
        assert!(args.pair && args.given.is_none());
        assert!(parse_from(Vec::new()).unwrap().host.is_none());
    }
}
