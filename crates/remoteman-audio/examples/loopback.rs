//! The integration test, on the session's real PipeWire but heard by nobody:
//!
//! ```text
//! cargo run --release -p remoteman-audio --example loopback
//! ```
//!
//! Speaker path: a [`SpeakerSink`] named `remoteman-test-sink-<pid>`, a tone played into it and
//! into nothing else by a [`Player`], the sink's packets decoded and measured.
//! Microphone path: a [`MicSource`] named `remoteman-test-mic-<pid>`, tone packets pushed into
//! it, recorded from it and from nothing else by a [`MicCapture`], and measured.
//!
//! It prints PASS or FAIL for each check and exits with 0 only if all passed. It takes about
//! four seconds, and its four nodes are on the graph for that long. It leaves the default sink
//! and source, volumes and every node it didn't make alone, and it checks that it did: it stops
//! before making anything if a new node could become the default, and fails if a link ever
//! joins one of its nodes to a foreign one or if the default metadata changed.

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use remoteman_audio::testing::{frequency, peak, sine_from};
use remoteman_audio::{Channels, Decoder, Encoder, FRAME_SAMPLES, FRAME_US, JitterStats, MicCapture, MicSource, Node, Packet, Player, SAMPLE_RATE, SpeakerSink, default_sink};

const PREFIX: &str = "remoteman-test-";
const TONE_HZ: f32 = 1000.0;
const TONE_LEVEL: f32 = 0.25;
/// Frames of tone sent on each path: one second.
const TONE_FRAMES: usize = 100;

/// The whole run ends by itself after this long, whatever happened. The nodes belong to this
/// process, so PipeWire removes them when it exits.
const WATCHDOG: Duration = Duration::from_secs(13);

struct Checks {
    failed: bool,
}

impl Checks {
    fn check(&mut self, ok: bool, what: &str) -> bool {
        println!("{} {what}", if ok { "PASS" } else { "FAIL" });
        self.failed |= !ok;
        ok
    }
}

fn output(program: &str, args: &[&str]) -> String {
    match Command::new(program).args(args).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(e) => {
            println!("FAIL run {program}: {e}");
            std::process::exit(1);
        }
    }
}

/// The default sink and source, configured and current, as `pw-metadata` prints them.
fn defaults() -> Vec<String> {
    let mut lines: Vec<String> = output("pw-metadata", &["-n", "default", "0"]).lines().filter(|l| l.contains("key:'default.")).map(str::to_owned).collect();
    lines.sort();
    lines
}

/// The value of one key in [`defaults`].
fn default_of<'a>(defaults: &'a [String], key: &str) -> Option<&'a str> {
    let line = defaults.iter().find(|l| l.contains(&format!("key:'{key}'")))?;
    Some(line.split_once("value:'")?.1.split_once('\'')?.0)
}

/// Every link on the graph as (output port, input port), ports as `node.name:port`.
fn links() -> Vec<(String, String)> {
    let mut links = Vec::new();
    let mut port = String::new();
    for line in output("pw-link", &["-l"]).lines() {
        if let Some((_, peer)) = line.split_once("|->") {
            links.push((port.clone(), peer.trim().to_owned()));
        } else if let Some((_, peer)) = line.split_once("|<-") {
            links.push((peer.trim().to_owned(), port.clone()));
        } else {
            port = line.trim().to_owned();
        }
    }
    links.sort();
    links.dedup();
    links
}

/// Wait until `ready` holds, for at most `limit`.
fn wait(limit: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    ready()
}

/// Wait until the session manager has given the device node `name` its ports: from then on it
/// can be a stream's target.
fn has_ports(name: &str, direction: &str) -> bool {
    wait(Duration::from_secs(3), || output("pw-link", &[direction]).lines().any(|l| l.trim().starts_with(&format!("{name}:"))))
}

/// Wait until `stream` runs, linked to `device` and to nothing else, and no link joins a test
/// node to a foreign one. The links are (stream port, device port) or the other way round.
fn linked_only_to_each_other(checks: &mut Checks, stream: &Node, device: &Node) -> bool {
    let running = wait(Duration::from_secs(3), || stream.is_streaming() && device.is_streaming() || stream.error().is_some());
    if !checks.check(running && stream.error().is_none(), &format!("{} is linked and running ({})", stream.name(), stream.error().unwrap_or_else(|| "no error".into()))) {
        return false;
    }
    let ours: Vec<(String, String)> = links().into_iter().filter(|(a, b)| a.starts_with(PREFIX) || b.starts_with(PREFIX)).collect();
    let between = |a: &str, b: &str| (a.starts_with(&format!("{}:", stream.name())) && b.starts_with(&format!("{}:", device.name()))) || (a.starts_with(&format!("{}:", device.name())) && b.starts_with(&format!("{}:", stream.name())));
    let ok = !ours.is_empty() && ours.iter().all(|(a, b)| between(a, b));
    for (a, b) in &ours {
        println!("     link {a} -> {b}");
    }
    checks.check(ok, &format!("{} is linked to {} and to nothing else", stream.name(), device.name()))
}

type Received = Arc<Mutex<Vec<(u64, Packet)>>>;

/// Send `TONE_FRAMES` frames of tone as packets through `push`, one every 10 ms. Returns when
/// the first one was pushed, on `clock`.
fn send_tone(channels: Channels, clock: &impl Fn() -> u64, mut push: impl FnMut(Packet)) -> u64 {
    let mut encoder = Encoder::new(channels).expect("encoder");
    let start = Instant::now();
    let mut first = 0;
    for i in 0..TONE_FRAMES {
        if let Some(wait) = (start + Duration::from_micros(i as u64 * FRAME_US)).checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let frame = sine_from(i * FRAME_SAMPLES, TONE_HZ, TONE_LEVEL, FRAME_SAMPLES, channels.count());
        let packet = Packet { seq: i as u32, capture_us: clock(), data: encoder.encode(&frame).expect("encode") };
        if i == 0 {
            first = packet.capture_us;
        }
        push(packet);
    }
    first
}

/// Decode what a path delivered and check it against what was sent.
fn measure(checks: &mut Checks, path: &str, channels: Channels, received: &Received, before_tone: usize, first_push_us: u64) {
    // The tone is over. Sending must stop by itself: nothing more after the silent run.
    std::thread::sleep(Duration::from_millis(400));
    let count = received.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(300));
    let received = received.lock().unwrap().clone();
    checks.check(before_tone == 0, &format!("{path}: no packets while nothing plays ({before_tone})"));
    checks.check(received.len() == count, &format!("{path}: packets stop after the sound ({} then {count})", received.len()));
    // The tone, the frames made up when it ran out and the silent run that ends a sound.
    let expected = TONE_FRAMES..=TONE_FRAMES + 16;
    checks.check(expected.contains(&received.len()), &format!("{path}: {} packets for {TONE_FRAMES} frames of tone (expected {expected:?})", received.len()));
    if received.is_empty() {
        return;
    }
    checks.check(received.iter().enumerate().all(|(i, (_, p))| p.seq == i as u32), &format!("{path}: seq counts up from 0 without a hole"));

    let n = channels.count();
    let mut decoder = Decoder::new(channels).expect("decoder");
    let mut first_channel = Vec::new();
    let mut decoded = true;
    for (_, packet) in &received {
        let mut out = vec![0.0; FRAME_SAMPLES * n];
        decoded &= decoder.decode(&packet.data, &mut out).is_ok();
        first_channel.extend(out.iter().step_by(n));
    }
    checks.check(decoded, &format!("{path}: every packet decodes to a 10 ms frame"));
    // The middle of the tone, clear of its start and end.
    let frames = received.len().min(TONE_FRAMES);
    let middle = &first_channel[frames / 5 * FRAME_SAMPLES..frames * 4 / 5 * FRAME_SAMPLES];
    let (hz, level) = (frequency(middle), peak(middle));
    checks.check((hz - TONE_HZ).abs() < 5.0, &format!("{path}: tone is {hz:.1} Hz (sent {TONE_HZ} Hz)"));
    checks.check((level - TONE_LEVEL).abs() < 0.05, &format!("{path}: tone peaks at {level:.3} (sent {TONE_LEVEL})"));
    // A dropout or a made-up frame inside the tone would show as a step between two samples
    // larger than the sine's own steepest one.
    let steepest = TONE_LEVEL * std::f32::consts::TAU * TONE_HZ / SAMPLE_RATE as f32;
    let step = middle.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
    checks.check(step < 2.0 * steepest, &format!("{path}: no click in the tone (largest step {step:.4}, the sine's own {steepest:.4})"));

    let bytes: usize = received.iter().take(frames).map(|(_, p)| p.data.len()).sum();
    println!("     {path}: {} bit/s over the tone", bytes * 8 * 100 / frames);
    // From the capture time stamped on a packet to the moment it was handed over.
    let delays: Vec<f64> = received.iter().map(|(at, p)| (*at as f64 - p.capture_us as f64) / 1000.0).collect();
    let (min, max) = delays.iter().fold((f64::MAX, f64::MIN), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
    let mean = delays.iter().sum::<f64>() / delays.len() as f64;
    checks.check(max < 40.0, &format!("{path}: capture to packet delay min {min:.1} ms, mean {mean:.1} ms, max {max:.1} ms"));
    // From the first tone packet pushed in at the far end to the first packet out here: the
    // jitter buffer, the graph and the framing together.
    let onset = (received[0].0 as f64 - first_push_us as f64) / 1000.0;
    checks.check((0.0..120.0).contains(&onset), &format!("{path}: first packet in to first packet out {onset:.1} ms"));
}

/// The jitter buffer of the playing end got every packet in time and in order: it played them
/// all, and made up only the two frames after the last one.
fn played_all(checks: &mut Checks, who: &str, stats: JitterStats) {
    let expected = JitterStats { played: TONE_FRAMES as u64, concealed: 2, late: 0, skipped: 0 };
    checks.check(stats == expected, &format!("{who} played every packet ({stats:?})"));
}

fn speaker_path(checks: &mut Checks, clock: impl Fn() -> u64 + Clone + Send + 'static) {
    let pid = std::process::id();
    let (sink_name, player_name) = (format!("{PREFIX}sink-{pid}"), format!("{PREFIX}player-{pid}"));
    let received: Received = Arc::default();
    let sink = {
        let (received, now) = (received.clone(), clock.clone());
        SpeakerSink::open(&sink_name, "remoteman test sink", clock.clone(), move |p| received.lock().unwrap().push((now(), p)))
    };
    let sink = match sink {
        Ok(sink) => sink,
        Err(e) => {
            checks.check(false, &format!("speaker: create the sink: {e:#}"));
            return;
        }
    };
    println!("     sink {} is node {}", sink.node().name(), sink.node().id());
    if !checks.check(has_ports(&sink_name, "-i"), "speaker: the sink has its ports") {
        return;
    }
    let player = match Player::open(&player_name, Some(&sink_name)) {
        Ok(player) => player,
        Err(e) => {
            checks.check(false, &format!("speaker: create the player: {e:#}"));
            return;
        }
    };
    if !linked_only_to_each_other(checks, player.node(), sink.node()) {
        return;
    }
    // The player is running and plays silence: none of it may come out as packets.
    std::thread::sleep(Duration::from_millis(300));
    let before = received.lock().unwrap().len();
    let first = send_tone(Channels::Stereo, &clock, |p| player.push(p));
    measure(checks, "speaker", Channels::Stereo, &received, before, first);
    played_all(checks, "speaker: the player", player.stats());
}

fn microphone_path(checks: &mut Checks, clock: impl Fn() -> u64 + Clone + Send + 'static) {
    let pid = std::process::id();
    let (mic_name, recorder_name) = (format!("{PREFIX}mic-{pid}"), format!("{PREFIX}recorder-{pid}"));
    let mic = match MicSource::open(&mic_name, "remoteman test microphone") {
        Ok(mic) => mic,
        Err(e) => {
            checks.check(false, &format!("microphone: create the source: {e:#}"));
            return;
        }
    };
    println!("     source {} is node {}", mic.node().name(), mic.node().id());
    if !checks.check(has_ports(&mic_name, "-o"), "microphone: the source has its ports") {
        return;
    }
    let received: Received = Arc::default();
    let recorder = {
        let (received, now) = (received.clone(), clock.clone());
        MicCapture::open(&recorder_name, Some(&mic_name), clock.clone(), move |p| received.lock().unwrap().push((now(), p)))
    };
    let recorder = match recorder {
        Ok(recorder) => recorder,
        Err(e) => {
            checks.check(false, &format!("microphone: create the recorder: {e:#}"));
            return;
        }
    };
    if !linked_only_to_each_other(checks, recorder.node(), mic.node()) {
        return;
    }
    std::thread::sleep(Duration::from_millis(300));
    let before = received.lock().unwrap().len();
    let first = send_tone(Channels::Mono, &clock, |p| mic.push(p));
    measure(checks, "microphone", Channels::Mono, &received, before, first);
    played_all(checks, "microphone: the source", mic.stats());
}

fn main() {
    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        println!("FAIL still running after {WATCHDOG:?}: giving up");
        std::process::exit(2);
    });
    let mut checks = Checks { failed: false };

    // A new sink or source can only become the default if the user's own choice is missing or
    // not there. Then this test doesn't run.
    let before = defaults();
    for kind in ["sink", "source"] {
        let configured = default_of(&before, &format!("default.configured.audio.{kind}"));
        let current = default_of(&before, &format!("default.audio.{kind}"));
        if configured.is_none() || configured != current {
            println!("FAIL not run: the default {kind} is {current:?} but the configured one is {configured:?}; a test node could become the default");
            std::process::exit(1);
        }
    }
    if output("pw-cli", &["ls", "Node"]).contains(PREFIX) {
        println!("FAIL not run: there already are {PREFIX}* nodes");
        std::process::exit(1);
    }

    // Reading the default through the crate gives what pw-metadata shows. (Only the reading
    // half of `DefaultSink` runs here; nothing in this test sets a default.)
    let current = default_of(&before, "default.audio.sink").and_then(|v| v.split('"').nth(3)).map(str::to_owned);
    match default_sink() {
        Ok(read) => checks.check(read == current, &format!("default_sink() reads {read:?}, as pw-metadata shows")),
        Err(e) => checks.check(false, &format!("default_sink(): {e:#}")),
    };

    let epoch = Instant::now();
    let clock = move || epoch.elapsed().as_micros() as u64;
    speaker_path(&mut checks, clock);
    microphone_path(&mut checks, clock);

    // Both paths dropped their nodes when they returned.
    let gone = wait(Duration::from_secs(2), || !output("pw-cli", &["ls", "Node"]).contains(PREFIX));
    checks.check(gone, &format!("no {PREFIX}* node is left (pw-cli ls Node)"));
    checks.check(defaults() == before, "the default sink and source are what they were");
    println!("{}", if checks.failed { "FAIL" } else { "PASS" });
    std::process::exit(if checks.failed { 1 } else { 0 });
}
