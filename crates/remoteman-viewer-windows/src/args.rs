//! The command line.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use remoteman_core::{Chroma, Codec, Mode};

pub struct Args {
    pub host: String,
    pub pair: bool,
    pub state: PathBuf,
    pub size: (u32, u32),
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
}

const USAGE: &str = "usage: remoteman-windows HOST:PORT [--pair] [--state DIR] [--size WxH] [--refresh HZ] [--decoders av1,hevc] [--seconds N] [--fullscreen] [--shot FILE.png] [--stats] [--input-demo] [--log FILE]";

fn mode(word: &str) -> Option<Mode> {
    // 4:4:4 isn't offered: D3D11VA hands 4:2:0 pictures out as P010, and what a 4:4:4 one looks
    // like there is untried.
    Some(match word {
        "av1" => Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 },
        "hevc" => Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 },
        _ => return None,
    })
}

/// Where the identity and the known hosts live when `--state` doesn't say.
fn default_state() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into()).join("remoteman")
}

pub fn parse() -> Result<Args> {
    let mut words = std::env::args().skip(1);
    let host = words.next().filter(|h| !h.starts_with("--")).ok_or_else(|| anyhow!(USAGE))?;
    let mut args = Args {
        host,
        pair: false,
        state: default_state(),
        size: (1920, 1080),
        refresh: None,
        decoders: vec![mode("av1").unwrap(), mode("hevc").unwrap()],
        seconds: None,
        fullscreen: false,
        shot: None,
        stats: false,
        input_demo: false,
        log: None,
    };
    while let Some(arg) = words.next() {
        let mut value = || words.next().ok_or_else(|| anyhow!("{arg} needs a value"));
        match arg.as_str() {
            "--pair" => args.pair = true,
            "--state" => args.state = value()?.into(),
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').context("--size WxH")?;
                args.size = (w.parse()?, h.parse()?);
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
            other => bail!("unknown option {other}\n{USAGE}"),
        }
    }
    Ok(args)
}
