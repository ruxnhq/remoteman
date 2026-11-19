//! The command line.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};

use remoteman_core::{Chroma, Codec, Mode};

pub struct Args {
    /// A computer to connect to at once; `None`: the app opens on its list.
    pub host: Option<String>,
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
}

const USAGE: &str = "usage: remoteman [HOST:PORT] [--pair] [--state DIR] [--size WxH] [--refresh HZ] [--decoders av1,hevc] [--seconds N] [--fullscreen] [--shot FILE.png] [--stats] [--input-demo] [--log FILE] [--sound] [--ui STEPS --shots DIR [--dark|--light]]";

fn mode(word: &str) -> Option<Mode> {
    // 4:4:4 isn't offered: D3D11VA hands 4:2:0 pictures out as P010, and what a 4:4:4 one looks
    // like there is untried.
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
    let host = words.next_if(|h| !h.starts_with("--"));
    let mut args = Args {
        host,
        pair: false,
        state: default_state(),
        size: None,
        refresh: None,
        decoders: vec![mode("av1").unwrap(), mode("hevc").unwrap(), mode("hevc444").unwrap()],
        seconds: None,
        fullscreen: false,
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
            other => bail!("unknown option {other}\n{USAGE}"),
        }
    }
    anyhow::ensure!(args.ui.is_none() || args.shots.is_some(), "--ui needs --shots DIR");
    Ok(args)
}
