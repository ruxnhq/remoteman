//! `remoteman-testclient HOST:PORT [options]`: a client with no screen.
//!
//!     --state DIR        where this client's identity and known hosts live
//!     --pair             accept whichever host answers, and remember it
//!     --size WxH         the display to ask for (default 2752x2064)
//!     --scale S          its scale (default 2)
//!     --refresh HZ       its refresh rate (default 120)
//!     --decoders LIST    in order of preference, from av1, hevc444, hevc (default av1,hevc444,hevc)
//!     --seconds N        how long to run (default 10)
//!     --out FILE         save the video stream
//!     --input-demo       move the pointer, click, type and touch while running

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use remoteman_core::{Button, Chroma, Client, Codec, Display, Event, Frame, Handler, Mode, Options, StreamInfo, TouchPhase};

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() { 0.0 } else { sorted[((sorted.len() - 1) as f64 * p) as usize] }
}

#[derive(Default)]
struct Seen {
    arrivals: Vec<Instant>,
    ages: Vec<f64>,
    bytes: usize,
    keys: u32,
    closed: Option<String>,
}

struct Watcher {
    seen: Arc<Mutex<Seen>>,
    file: Option<std::fs::File>,
    warm: Instant,
}

impl Handler for Watcher {
    fn stream(&mut self, s: &StreamInfo) {
        println!("stream: {:?} {:?} {}-bit, {}x{} at {} Hz, {}", s.mode.codec, s.mode.chroma, s.mode.depth, s.width, s.height, s.refresh, s.colour);
    }

    fn frame(&mut self, frame: &Frame, age_us: Option<i64>) {
        if let Some(f) = self.file.as_mut() {
            let _ = f.write_all(&frame.data);
        }
        if Instant::now() < self.warm {
            return;
        }
        let mut s = self.seen.lock().unwrap();
        s.arrivals.push(Instant::now());
        s.bytes += frame.data.len();
        s.keys += frame.key as u32;
        if let Some(age) = age_us {
            s.ages.push(age as f64 / 1000.0);
        }
    }

    fn closed(&mut self, reason: &str) {
        self.seen.lock().unwrap().closed = Some(reason.to_owned());
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let host = args.next().context("usage: remoteman-testclient HOST:PORT [options]")?;
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let (mut state, mut pair, mut size, mut scale, mut refresh, mut secs) = (home.join(".local/state/remoteman"), false, (2752u32, 2064u32), 2.0f32, 120u32, 10u64);
    let (mut out, mut demo, mut decoders) = (None::<PathBuf>, false, "av1,hevc444,hevc".to_owned());
    while let Some(a) = args.next() {
        let mut value = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--state" => state = value()?.into(),
            "--pair" => pair = true,
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').context("--size WxH")?;
                size = (w.parse()?, h.parse()?);
            }
            "--scale" => scale = value()?.parse()?,
            "--refresh" => refresh = value()?.parse()?,
            "--seconds" => secs = value()?.parse()?,
            "--decoders" => decoders = value()?,
            "--out" => out = Some(value()?.into()),
            "--input-demo" => demo = true,
            other => bail!("unknown option {other}"),
        }
    }
    let decoders: Vec<Mode> = decoders
        .split(',')
        .map(|d| {
            Ok(match d {
                "av1" => Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 },
                "hevc444" => Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 },
                "hevc" => Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 },
                other => bail!("unknown decoder {other}"),
            })
        })
        .collect::<Result<_>>()?;


    let file = match &out {
        Some(p) => Some(std::fs::File::create(p).with_context(|| format!("create {}", p.display()))?),
        None => None,
    };
    let seen = Arc::new(Mutex::new(Seen::default()));
    let watcher = Watcher { seen: seen.clone(), file, warm: Instant::now() + Duration::from_secs(2) };
    let options = Options { host, state, name: "testclient".into(), pair, display: Display { width: size.0, height: size.1, scale, refresh }, decoders };
    let client = Client::connect(options, watcher)?;
    println!("host: {} ({}), features {:?}, fingerprint {}", client.host_name, client.host_os, client.host_features, &client.host_fingerprint[..12]);

    let end = Instant::now() + Duration::from_secs(secs);
    if demo {
        std::thread::sleep(Duration::from_secs(3));
        let mut script = vec![Event::PointerTo { x: 0.5, y: 0.5 }];
        script.extend((0..30).map(|i| Event::PointerTo { x: 0.5 + i as f32 * 0.005, y: 0.5 }));
        script.push(Event::Button { button: Button::Left, down: true });
        script.push(Event::Button { button: Button::Left, down: false });
        // h, i by key code (HID 0x0B, 0x0C), then text as typed.
        for usage in [0x0B, 0x0C] {
            script.push(Event::Key { usage, down: true });
            script.push(Event::Key { usage, down: false });
        }
        for usage in [0x0B, 0x0C] {
            script.push(Event::Key { usage, down: true });
            script.push(Event::Key { usage, down: false });
        }
        script.push(Event::Text("é".into()));
        script.push(Event::Text("x🙂".into()));
        script.push(Event::Scroll { dx: 0.0, dy: 30.0 });
        for i in 0..2 {
            let x = 0.25 + i as f32 * 0.1;
            script.push(Event::Touch { phase: TouchPhase::Down, slot: 0, x, y: 0.25 });
            script.push(Event::Touch { phase: TouchPhase::Move, slot: 0, x: x + 0.01, y: 0.26 });
            script.push(Event::Touch { phase: TouchPhase::Up, slot: 0, x: x + 0.01, y: 0.26 });
        }
        for e in script {
            client.send(e);
            std::thread::sleep(Duration::from_millis(60));
        }
        println!("input demo sent");
    }
    while Instant::now() < end && seen.lock().unwrap().closed.is_none() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let stats = client.stats();
    client.close();

    let mut s = seen.lock().unwrap();
    if let Some(reason) = s.closed.as_deref().filter(|r| !r.contains("done")) {
        println!("the host ended the session: {reason}");
    }
    let n = s.arrivals.len();
    println!("frames {n}, lost {}, keyframes {}, keyframe requests {}", stats.lost, s.keys, stats.keyframe_requests);
    if n > 2 {
        let span = (s.arrivals[n - 1] - s.arrivals[0]).as_secs_f64();
        let mut iv: Vec<f64> = s.arrivals.windows(2).map(|p| (p[1] - p[0]).as_secs_f64() * 1000.0).collect();
        iv.sort_by(|a, b| a.partial_cmp(b).unwrap());
        s.ages.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("rate {:.1} fps, {:.1} Mbit/s", (n - 1) as f64 / span, s.bytes as f64 * 8.0 / span / 1e6);
        println!("interval ms: median {:.2}  p99 {:.2}  max {:.2}", pct(&iv, 0.5), pct(&iv, 0.99), iv[iv.len() - 1]);
        println!("capture to received ms: median {:.2}  p95 {:.2}  p99 {:.2}  (round trip {:.2} ms)", pct(&s.ages, 0.5), pct(&s.ages, 0.95), pct(&s.ages, 0.99), stats.round_trip_us as f64 / 1000.0);
    }
    Ok(())
}
