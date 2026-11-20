//! A client with no screen and nothing but the core: connects, counts what arrives and can save
//! the stream as it came. It builds wherever the core does, so it is the first thing to run on
//! a new platform.
//!   watch HOST:PORT [--pair] [--state DIR] [--seconds N] [--size WxH] [--refresh HZ]
//!         [--decoders av1,hevc,hevc444] [--record FILE] [--type TEXT]
//! `--type`: eight seconds in, a click at the middle of the display, then TEXT as one text
//! event (input kind 7), as a viewer sends what its user typed.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use remoteman_core::{Button, Chroma, Client, Codec, Display, Event, Frame, Handler, Mode, Options, Placement, StreamInfo};

#[derive(Default)]
struct Seen {
    stream: Option<StreamInfo>,
    frames: u32,
    keys: u32,
    bytes: u64,
    ages_us: Vec<i64>,
    closed: Option<String>,
}

struct Watcher {
    seen: Arc<Mutex<Seen>>,
    record: Option<std::fs::File>,
}

impl Handler for Watcher {
    fn stream(&mut self, info: &StreamInfo) {
        println!("stream: {:?} {}x{} at {} Hz, {}", info.mode, info.width, info.height, info.refresh, info.colour);
        self.seen.lock().unwrap().stream = Some(info.clone());
    }

    fn frame(&mut self, frame: &Frame, age_us: Option<i64>) {
        if let Some(file) = &mut self.record {
            let _ = file.write_all(&frame.data);
        }
        let mut seen = self.seen.lock().unwrap();
        seen.frames += 1;
        seen.keys += frame.key as u32;
        seen.bytes += frame.data.len() as u64;
        if let Some(age) = age_us {
            seen.ages_us.push(age);
        }
    }

    fn closed(&mut self, reason: &str) {
        self.seen.lock().unwrap().closed = Some(reason.to_owned());
    }
}

fn mode(word: &str) -> Option<Mode> {
    Some(match word {
        "av1" => Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 },
        "hevc" => Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 },
        "hevc444" => Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 },
        _ => return None,
    })
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let host = args.next().ok_or_else(|| anyhow::anyhow!("usage: watch HOST:PORT [--pair] [--state DIR] [--seconds N] [--size WxH] [--refresh HZ] [--decoders LIST] [--record FILE]"))?;
    let (mut pair, mut state, mut seconds, mut size, mut refresh) = (false, PathBuf::from("watch-state"), 5u64, (1920u32, 1080u32), 60u32);
    let mut decoders = vec![mode("av1").unwrap(), mode("hevc").unwrap()];
    let mut record = None;
    let mut typed = None::<String>;
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| anyhow::anyhow!("{arg} needs a value"));
        match arg.as_str() {
            "--pair" => pair = true,
            "--state" => state = value()?.into(),
            "--seconds" => seconds = value()?.parse()?,
            "--refresh" => refresh = value()?.parse()?,
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or_else(|| anyhow::anyhow!("--size WxH"))?;
                size = (w.parse()?, h.parse()?);
            }
            "--decoders" => {
                let v = value()?;
                decoders = v.split(',').map(|w| mode(w).ok_or_else(|| anyhow::anyhow!("unknown decoder {w}"))).collect::<Result<_, _>>()?;
            }
            "--record" => record = Some(std::fs::File::create(value()?)?),
            "--type" => typed = Some(value()?),
            other => anyhow::bail!("unknown option {other}"),
        }
    }

    let seen = Arc::new(Mutex::new(Seen::default()));
    let options = Options {
        host,
        state,
        name: "watch".into(),
        pair,
        display: Display { width: size.0, height: size.1, scale: 1.0, refresh, placement: Placement::default(), hdr: false, headroom: 0.0 },
        decoders,
        test_drop_one_in: None,
        test_delay_lost_ms: None,
        cursor: false,
        unlock: None,
        attach_only: false,
    };
    let client = Client::connect(options, Watcher { seen: seen.clone(), record })?;
    println!("host: {} ({}), features {:?}", client.host_name, client.host_os, client.host_features);

    let end = Instant::now() + Duration::from_secs(seconds);
    let type_at = Instant::now() + Duration::from_secs(8);
    while Instant::now() < end && seen.lock().unwrap().closed.is_none() {
        if let Some(text) = typed.take_if(|_| Instant::now() >= type_at) {
            client.send(Event::PointerTo { x: 0.5, y: 0.5 });
            for down in [true, false] {
                client.send(Event::Button { button: Button::Left, down });
            }
            std::thread::sleep(Duration::from_millis(300));
            client.send(Event::Text(text));
            println!("typed");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let stats = client.stats();
    let mut seen = seen.lock().unwrap();
    seen.ages_us.sort_unstable();
    let median = seen.ages_us.get(seen.ages_us.len() / 2).copied();
    println!(
        "frames {} ({} key), {} kB, {:.1} a second, lost {}, age median {} ms, round trip {:.1} ms",
        seen.frames,
        seen.keys,
        seen.bytes / 1000,
        seen.frames as f64 / seconds as f64,
        stats.lost,
        median.map_or("?".into(), |us| format!("{:.1}", us as f64 / 1000.0)),
        stats.round_trip_us as f64 / 1000.0,
    );
    if let Some(reason) = &seen.closed {
        println!("closed: {reason}");
    }
    anyhow::ensure!(seen.frames > 0, "no frame arrived");
    Ok(())
}
