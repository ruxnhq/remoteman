//! `remoteman-testclient HOST:PORT [options]`: a client with no screen.
//!
//!     --state DIR        where this client's identity and known hosts live
//!     --pair             accept whichever host answers, and remember it
//!     --size WxH         the display to ask for (default 2752x2064)
//!     --scale S          its scale (default 2)
//!     --refresh HZ       its refresh rate (default 120)
//!     --decoders LIST    in order of preference, from av1, hevc444, hevc (default av1,hevc444,hevc)
//!     --seconds N        how long to run (default 10)
//!     --out FILE         save the video stream, bare (playable by FFmpeg)
//!     --record FILE      save what the app would be handed, frame by frame (format below)
//!     --input-demo       move the pointer, click, type and touch while running
//!     --pointer-demo     move the pointer in a circle and nothing else (safe on a desktop in use)
//!     --script FILE      send input from a script while running, one step a line:
//!                          wait SECONDS | move X Y | click X Y | key enter|tab|esc|space|HEX
//!                          text WORDS | textfile PATH | mark NAME
//!                          sound on|off | mic PACKETS | camera FRAMES | clip WORDS
//!                          file PATH | bitrate BITS | resize WxH
//!                        X and Y are fractions of the display. `textfile` types a file's
//!                        contents without showing them. `mark` prints how many frames have
//!                        arrived, to find that moment in a stream saved with --out.
//!                        The last row is the features: ask for the desktop's sound; send
//!                        made-up microphone packets (100 a second) or camera frames (30 a
//!                        second); copy WORDS to the host's clipboard; send a file; cap the
//!                        bitrate; change the display's size. What comes back is printed.
//!     --cursor           draw the cursor ourselves: ask for its shape and position apart
//!     --drop-one-in N    throw away one video packet in N, as a lossy network would
//!     --delay-lost MS    hold each loss report back this long, as a slow path to the host would
//!     --placement P      extend (default), main or only: where the display sits on the host

//!
//! A recording (`--record`) is for testing decoders without a host. It is:
//!   the line `RMREC1`, then one line of JSON, the stream description:
//!     {"codec":"av1"|"hevc","chroma":"420"|"444","depth":10,"width":W,"height":H,"refresh":HZ,"colour":"bt709"}
//!   then, for every frame in order, little-endian:
//!     u32 length of the data, u8 keyframe (1 or 0), u32 frame number, u64 capture time in
//!     microseconds on the host's clock, and the data (HEVC Annex-B, or AV1 OBUs).
//! The first frame is always a keyframe.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use remoteman_core::media::{BLOB_CLIPBOARD, BLOB_FILE, ClipboardHeader, FileHeader};
use remoteman_core::{BlobEvent, Button, Chroma, Client, Codec, CursorShape, Display, Event, Frame, Handler, Message, Mode, Options, Packet, Placement, StreamInfo, TouchPhase};

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() { 0.0 } else { sorted[((sorted.len() - 1) as f64 * p) as usize] }
}

#[derive(Default)]
struct Seen {
    arrivals: Vec<Instant>,
    ages: Vec<f64>,
    bytes: usize,
    keys: u32,
    recoveries: u32,
    /// Cursor shapes and positions reported, and the last of each.
    shapes: Vec<(u32, u32, i32, i32)>,
    positions: u32,
    last_at: Option<(f32, f32, bool)>,
    closed: Option<String>,
    /// Every frame handed over, the first two seconds' included (what --out has saved).
    saved: u32,
    /// Sound packets, how many were missing in between, and the last one's number.
    sound: (u32, u32, Option<u32>),
    /// Blobs coming in: kind, header, content so far.
    blobs: std::collections::BTreeMap<u32, (u8, String, Vec<u8>)>,
}

struct Watcher {
    seen: Arc<Mutex<Seen>>,
    file: Option<std::fs::File>,
    record: Option<std::fs::File>,
    warm: Instant,
}

impl Handler for Watcher {
    fn stream(&mut self, s: &StreamInfo) {
        println!("stream: {:?} {:?} {}-bit, {}x{} at {} Hz, {}", s.mode.codec, s.mode.chroma, s.mode.depth, s.width, s.height, s.refresh, s.colour);
        if let Some(f) = self.record.as_mut() {
            let codec = if s.mode.codec == Codec::Av1 { "av1" } else { "hevc" };
            let chroma = if s.mode.chroma == Chroma::C444 { "444" } else { "420" };
            let _ = writeln!(
                f,
                "RMREC1\n{{\"codec\":\"{codec}\",\"chroma\":\"{chroma}\",\"depth\":{},\"width\":{},\"height\":{},\"refresh\":{},\"colour\":\"{}\"}}",
                s.mode.depth, s.width, s.height, s.refresh, s.colour
            );
        }
    }

    fn frame(&mut self, frame: &Frame, age_us: Option<i64>) {
        if let Some(f) = self.file.as_mut() {
            let _ = f.write_all(&frame.data);
        }
        if let Some(f) = self.record.as_mut() {
            let mut head = Vec::with_capacity(17);
            head.extend_from_slice(&(frame.data.len() as u32).to_le_bytes());
            head.push(frame.key as u8);
            head.extend_from_slice(&frame.number.to_le_bytes());
            head.extend_from_slice(&frame.capture_us.to_le_bytes());
            let _ = f.write_all(&head).and_then(|()| f.write_all(&frame.data));
        }
        let mut s = self.seen.lock().unwrap();
        s.saved += 1;
        if Instant::now() < self.warm {
            return;
        }
        s.arrivals.push(Instant::now());
        s.bytes += frame.data.len();
        s.keys += frame.key as u32;
        s.recoveries += frame.recovery as u32;
        if let Some(age) = age_us {
            s.ages.push(age as f64 / 1000.0);
        }
    }

    fn cursor_shape(&mut self, s: &CursorShape) {
        self.seen.lock().unwrap().shapes.push((s.width, s.height, s.hot_x, s.hot_y));
    }

    fn cursor_at(&mut self, x: f32, y: f32, visible: bool) {
        let mut s = self.seen.lock().unwrap();
        s.positions += 1;
        s.last_at = Some((x, y, visible));
    }

    fn message(&mut self, message: &Message) {
        println!("host says: {}", serde_json::to_string(message).unwrap_or_default());
    }

    fn sound(&mut self, packet: &Packet) {
        let mut s = self.seen.lock().unwrap();
        // A packet marked as a start begins a new count (after a pause, or from another agent).
        let missing = s.sound.2.filter(|_| !packet.start).map_or(0, |last| packet.seq.saturating_sub(last).saturating_sub(1));
        s.sound = (s.sound.0 + 1, s.sound.1 + missing, Some(packet.seq));
    }

    fn blob(&mut self, event: &BlobEvent) {
        let mut s = self.seen.lock().unwrap();
        match event {
            BlobEvent::Open { id, kind, header } => {
                s.blobs.insert(*id, (*kind, header.clone(), Vec::new()));
            }
            BlobEvent::Data { id, bytes } => {
                if let Some(b) = s.blobs.get_mut(id) {
                    b.2.extend_from_slice(bytes);
                }
            }
            BlobEvent::Close { id, complete } => {
                let Some((kind, header, content)) = s.blobs.remove(id) else { return };
                if kind == BLOB_CLIPBOARD && *complete {
                    println!("clipboard from the host {header}: {}", String::from_utf8_lossy(&content));
                } else {
                    println!("blob of kind {kind:#x} {header}: {} bytes, complete {complete}", content.len());
                }
            }
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
    let (mut out, mut record, mut demo, mut cursor, mut decoders) = (None::<PathBuf>, None::<PathBuf>, false, false, "av1,hevc444,hevc".to_owned());
    let mut circle = false;
    let mut script = None::<PathBuf>;
    let mut drop_one_in = None::<u32>;
    let mut delay_lost = None::<u32>;
    let mut placement = Placement::Extend;
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
            "--record" => record = Some(value()?.into()),
            "--input-demo" => demo = true,
            "--pointer-demo" => circle = true,
            "--script" => script = Some(value()?.into()),
            "--cursor" => cursor = true,
            "--drop-one-in" => drop_one_in = Some(value()?.parse()?),
            "--delay-lost" => delay_lost = Some(value()?.parse()?),
            "--placement" => {
                placement = match value()?.as_str() {
                    "extend" => Placement::Extend,
                    "main" => Placement::Main,
                    "only" => Placement::Only,
                    other => bail!("unknown placement {other}"),
                }
            }
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
    let record = match &record {
        Some(p) => Some(std::fs::File::create(p).with_context(|| format!("create {}", p.display()))?),
        None => None,
    };
    let watcher = Watcher { seen: seen.clone(), file, record, warm: Instant::now() + Duration::from_secs(2) };
    let options = Options { host, state, name: "testclient".into(), pair, display: Display { width: size.0, height: size.1, scale, refresh, placement }, decoders, test_drop_one_in: drop_one_in, test_delay_lost_ms: delay_lost, cursor };
    let client = Client::connect(options, watcher)?;
    println!("host: {} ({}), features {:?}, fingerprint {}", client.host_name, client.host_os, client.host_features, &client.host_fingerprint[..12]);

    let end = Instant::now() + Duration::from_secs(secs);
    if let Some(path) = &script {
        let steps = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        for (i, line) in steps.lines().enumerate() {
            if seen.lock().unwrap().closed.is_some() {
                break;
            }
            let line = line.trim();
            let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
            let xy = || -> Result<(f32, f32)> {
                let (x, y) = rest.trim().split_once(' ').with_context(|| format!("line {}: X Y", i + 1))?;
                Ok((x.parse()?, y.trim().parse()?))
            };
            match word {
                "" | "#" => {}
                "wait" => std::thread::sleep(Duration::from_secs_f32(rest.trim().parse().with_context(|| format!("line {}: wait SECONDS", i + 1))?)),
                "move" => {
                    let (x, y) = xy()?;
                    client.send(Event::PointerTo { x, y });
                }
                "click" => {
                    let (x, y) = xy()?;
                    client.send(Event::PointerTo { x, y });
                    std::thread::sleep(Duration::from_millis(80));
                    client.send(Event::Button { button: Button::Left, down: true });
                    std::thread::sleep(Duration::from_millis(60));
                    client.send(Event::Button { button: Button::Left, down: false });
                }
                "key" => {
                    let usage = match rest.trim() {
                        "enter" => 0x28,
                        "esc" => 0x29,
                        "tab" => 0x2B,
                        "space" => 0x2C,
                        hex => u16::from_str_radix(hex.trim_start_matches("0x"), 16).with_context(|| format!("line {}: key NAME or HEX", i + 1))?,
                    };
                    client.send(Event::Key { usage, down: true });
                    std::thread::sleep(Duration::from_millis(40));
                    client.send(Event::Key { usage, down: false });
                }
                "text" => client.send(Event::Text(rest.to_owned())),
                "textfile" => {
                    let text = std::fs::read_to_string(rest.trim()).with_context(|| format!("line {}: read {}", i + 1, rest.trim()))?;
                    client.send(Event::Text(text.trim_end_matches('\n').to_owned()));
                }
                "mark" => println!("mark {}: {} frames so far", rest.trim(), seen.lock().unwrap().saved),
                "sound" => client.send_message(Message::Sound { on: rest.trim() == "on" }),
                "mic" => {
                    let n: u32 = rest.trim().parse().with_context(|| format!("line {}: mic PACKETS", i + 1))?;
                    client.send_message(Message::Mic { on: true, codec: "opus".into(), rate: 48000, channels: 1, frame_us: 10_000 });
                    let start = Instant::now();
                    for seq in 0..n {
                        client.send_mic(seq == 0, seq, start.elapsed().as_micros() as u64, format!("mic {seq}").as_bytes());
                        std::thread::sleep((start + Duration::from_millis((seq as u64 + 1) * 10)).saturating_duration_since(Instant::now()));
                    }
                    client.send_message(Message::Mic { on: false, codec: String::new(), rate: 0, channels: 0, frame_us: 0 });
                }
                "camera" => {
                    let n: u32 = rest.trim().parse().with_context(|| format!("line {}: camera FRAMES", i + 1))?;
                    client.send_message(Message::Camera { on: true, codec: "h264".into(), width: 1280, height: 720, fps: 30 });
                    let start = Instant::now();
                    for f in 0..n {
                        // About what a 2.5 Mbit/s camera sends: 10 kB a frame, a keyframe every 2 s.
                        client.send_camera(&vec![f as u8; 10_000], f % 60 == 0, start.elapsed().as_micros() as u64);
                        std::thread::sleep((start + Duration::from_millis((f as u64 + 1) * 33)).saturating_duration_since(Instant::now()));
                    }
                    client.send_message(Message::Camera { on: false, codec: String::new(), width: 0, height: 0, fps: 0 });
                }
                "clip" => {
                    client.send_message(Message::Clipboard { on: true });
                    let header = ClipboardHeader { seq: 1, mime: "text/plain;charset=utf-8".into(), size: rest.len() as u64 };
                    let blob = client.open_blob(BLOB_CLIPBOARD, serde_json::to_string(&header)?);
                    blob.write(rest.as_bytes());
                    blob.finish();
                }
                "file" => {
                    let path = std::path::Path::new(rest.trim());
                    let content = std::fs::read(path).with_context(|| format!("line {}: read {}", i + 1, path.display()))?;
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let sum = content.iter().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(*b as u32));
                    println!("sending {name}: {} bytes, sum {sum:08x}", content.len());
                    let blob = client.open_blob(BLOB_FILE, serde_json::to_string(&FileHeader { id: i as u32, name, size: content.len() as u64 })?);
                    for piece in content.chunks(64 * 1024) {
                        if !blob.write(piece) {
                            break;
                        }
                    }
                    blob.finish();
                }
                "bitrate" => client.send_message(Message::Configure { display: None, bitrate: Some(rest.trim().parse().with_context(|| format!("line {}: bitrate BITS", i + 1))?), decoders: None }),
                "resize" => {
                    let (w, h) = rest.trim().split_once('x').with_context(|| format!("line {}: resize WxH", i + 1))?;
                    let display = Display { width: w.parse()?, height: h.parse()?, scale, refresh, placement };
                    client.send_message(Message::Configure { display: Some(display), bitrate: None, decoders: None });
                }
                other => bail!("line {}: unknown step {other}", i + 1),
            }
            // Moves follow each other like a hand's; everything else gets time to land.
            std::thread::sleep(Duration::from_millis(if word == "move" { 12 } else { 60 }));
        }
        println!("script done");
    }
    if circle {
        std::thread::sleep(Duration::from_secs(3));
        for i in 0..120 {
            let a = i as f32 / 120.0 * std::f32::consts::TAU;
            client.send(Event::PointerTo { x: 0.5 + 0.2 * a.cos(), y: 0.5 + 0.2 * a.sin() });
            std::thread::sleep(Duration::from_millis(16));
        }
        println!("pointer demo sent");
    }
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
    if let Some(reason) = s.closed.as_deref().filter(|r| !r.contains("done") && *r != "closed") {
        println!("the host ended the session: {reason}");
    }
    if s.sound.0 > 0 {
        println!("sound: {} packets, {} missing in between", s.sound.0, s.sound.1);
    }
    if cursor {
        println!("cursor: {} shapes {:?}, {} positions, last {:?}", s.shapes.len(), s.shapes, s.positions, s.last_at);
    }
    let n = s.arrivals.len();
    println!("frames {n}, lost {}, keyframes {}, recovery points {}, loss reports {}", stats.lost, s.keys, s.recoveries, stats.keyframe_requests);
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
