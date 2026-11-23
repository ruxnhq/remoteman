//! A host with no screen, for testing a viewer's sound and game controllers: it lists `sound`
//! and `gamepad`, plays a 1 kHz tone when asked for sound (Opus, 10 ms packets, paced by the
//! clock), and prints every controller state and other input it gets. It pairs with the first
//! device that comes. No picture is sent.
//!
//!     padhost [--listen ADDR:PORT] [--state DIR] [--drop N] [--pause SECS:EVERY]
//!
//! `--drop N` leaves out every Nth packet (the viewer should conceal it); `--pause 1:5` stops
//! sending for 1 s every 5 s, as a quiet desktop does (`seq` goes on without a hole, `start`
//! marks the first packet after).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use remoteman_audio::{Channels, Encoder, FRAME_SAMPLES, FRAME_US, SAMPLE_RATE};
use remoteman_core::host::{Accept, ClientInfo, Host, HostHandler, HostOptions, Link};
use remoteman_core::{Chroma, Codec, Event, Message, Mode};

struct Settings {
    drop_every: u32,
    pause: Option<(u64, u64)>,
}

struct Desk {
    link: Arc<Mutex<Option<Link>>>,
    /// Bumped to stop the tone that is playing.
    tone: Arc<AtomicU64>,
    settings: Arc<Settings>,
    pads: u64,
}

impl HostHandler for Desk {
    fn connected(&mut self, client: &ClientInfo, link: Link) -> Result<Accept, String> {
        println!("connected: {client:?}");
        *self.link.lock().unwrap() = Some(link);
        Ok(Accept { mode: Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 }, width: 1280, height: 720, refresh: 60, colour: "bt709".into(), sdr_white: 0, peak: 0 })
    }
    fn keyframe(&mut self) {}
    fn lost(&mut self, _first: u32, _last: u32) {}
    fn input(&mut self, event: Event) {
        match event {
            Event::Gamepad { pad, flags, buttons, left_trigger, right_trigger, left_x, left_y, right_x, right_y } => {
                self.pads += 1;
                println!(
                    "gamepad #{} pad {pad} {} buttons {buttons:#06x} triggers {left_trigger:.2} {right_trigger:.2} left {left_x:+.2} {left_y:+.2} right {right_x:+.2} {right_y:+.2}",
                    self.pads,
                    if flags & 1 != 0 { "connected" } else { "unplugged" }
                );
            }
            other => println!("input: {other:?}"),
        }
    }
    fn message(&mut self, message: &Message) {
        println!("message: {message:?}");
        if let Message::Sound { on } = message {
            let generation = self.tone.fetch_add(1, Ordering::SeqCst) + 1;
            if *on {
                let (link, tone, settings) = (self.link.clone(), self.tone.clone(), self.settings.clone());
                std::thread::spawn(move || {
                    if let Err(e) = play(link, tone, generation, settings) {
                        println!("tone: {e:#}");
                    }
                });
            }
        }
    }
    fn left(&mut self) {
        println!("left");
        self.tone.fetch_add(1, Ordering::SeqCst);
        *self.link.lock().unwrap() = None;
    }
}

/// A 1 kHz tone at half scale, stereo, one packet every 10 ms by the clock, until `tone` moves on.
fn play(link: Arc<Mutex<Option<Link>>>, tone: Arc<AtomicU64>, generation: u64, settings: Arc<Settings>) -> Result<()> {
    let Some(l) = link.lock().unwrap().clone() else { return Ok(()) };
    l.send_message(Message::SoundFormat { codec: "opus".into(), rate: SAMPLE_RATE, channels: 2, frame_us: FRAME_US as u32 });
    let mut encoder = Encoder::new(Channels::Stereo)?;
    let began = Instant::now();
    // `tick` counts 10 ms of the clock, `seq` the packets made: a pause moves only the first.
    let (mut tick, mut seq, mut sample, mut sent, mut dropped) = (0u64, 0u32, 0u64, 0u64, 0u64);
    let (mut paused, mut start) = (false, true);
    println!("tone: playing");
    while tone.load(Ordering::SeqCst) == generation {
        let due = began + Duration::from_micros(tick * FRAME_US);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        tick += 1;
        let t = tick * FRAME_US / 1_000_000;
        let quiet = settings.pause.is_some_and(|(secs, every)| every > 0 && t % every >= every - secs);
        if quiet != paused {
            paused = quiet;
            println!("tone: {}", if quiet { "pause" } else { "on again" });
            start = true;
        }
        if quiet {
            continue;
        }
        let mut frame = vec![0f32; FRAME_SAMPLES * 2];
        for i in 0..FRAME_SAMPLES {
            let v = 0.5 * (2.0 * std::f64::consts::PI * 1000.0 * (sample + i as u64) as f64 / SAMPLE_RATE as f64).sin() as f32;
            frame[2 * i] = v;
            frame[2 * i + 1] = v;
        }
        sample += FRAME_SAMPLES as u64;
        let data = encoder.encode(&frame).context("encode")?;
        if settings.drop_every > 0 && seq % settings.drop_every == settings.drop_every - 1 {
            dropped += 1;
        } else {
            l.send_sound(start, seq, l.now_us(), &data);
            sent += 1;
            start = false;
            if sent % 500 == 0 {
                println!("tone: {sent} packets sent, {dropped} left out");
            }
        }
        seq = seq.wrapping_add(1);
    }
    println!("tone: stopped after {sent} packets, {dropped} left out");
    Ok(())
}

fn main() -> Result<()> {
    let mut listen = "127.0.0.1:7798".to_owned();
    let mut state = std::env::temp_dir().join("remoteman-padhost");
    let mut drop_every = 0;
    let mut pause = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--listen" => listen = value()?,
            "--state" => state = PathBuf::from(value()?),
            "--drop" => drop_every = value()?.parse()?,
            "--pause" => {
                let v = value()?;
                let (s, e) = v.split_once(':').context("--pause SECS:EVERY")?;
                pause = Some((s.parse()?, e.parse()?));
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let features = ["video", "input", "sound", "gamepad"].map(String::from).to_vec();
    let desk = Desk { link: Default::default(), tone: Default::default(), settings: Arc::new(Settings { drop_every, pause }), pads: 0 };
    let host = Host::start(HostOptions { listen: listen.parse()?, state, name: "padhost".into(), os: "test".into(), features, pair: true, test_drop_one_in: None }, desk)?;
    println!("padhost listening on {}", host.address);
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
