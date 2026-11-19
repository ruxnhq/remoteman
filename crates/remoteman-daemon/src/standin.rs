//! `remotemand test-agent`: an agent with no desktop, for testing the daemon without a GPU or
//! a compositor. It answers a client with made-up frames, 120 a second, each beginning with a
//! line of text saying who sent it and its number, so a saved stream shows exactly which frames
//! reached the client and in what order.
//!
//! It also plays the host's part in every feature, in a way a test can check: sound is made-up
//! packets that count up; microphone packets and camera frames are counted; a clipboard blob
//! comes back in capitals; a file is counted and answered; `configure` restarts the picture at
//! the new size and reports the bitrate back.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use remoteman_core::host::{Accept, ClientInfo, HostHandler, Link};
use remoteman_core::media::{BLOB_CLIPBOARD, BLOB_FILE, ClipboardHeader, FileHeader};
use remoteman_core::{BlobEvent, Chroma, Codec, Event, Frame, Message, Mode, Packet};

struct StandIn {
    label: String,
    /// Set to end the thread making frames.
    stop: Option<Arc<AtomicBool>>,
    thread: Option<std::thread::JoinHandle<()>>,
    key: Arc<AtomicBool>,
    inputs: u32,
    link: Option<Link>,
    sound: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    mic: (u32, u32),
    camera: (u32, u32),
    /// Blobs coming in: the kind, the header and the content so far.
    blobs: std::collections::BTreeMap<u32, (u8, String, Vec<u8>)>,
}

impl StandIn {
    fn stop_sound(&mut self) {
        if let Some((stop, thread)) = self.sound.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = thread.join();
        }
    }
}

impl HostHandler for StandIn {
    fn connected(&mut self, client: &ClientInfo, link: Link) -> Result<Accept, String> {
        self.left();
        let stop = Arc::new(AtomicBool::new(false));
        let (label, stopped, key) = (self.label.clone(), stop.clone(), self.key.clone());
        key.store(true, Ordering::Relaxed);
        self.stop = Some(stop);
        self.link = Some(link.clone());
        self.thread = Some(std::thread::spawn(move || {
            let start = Instant::now();
            let mut n = 0u32;
            while !stopped.load(Ordering::Relaxed) {
                let mut frame = format!("\n{label} frame {n}\n").into_bytes();
                frame.resize(20_000, b'.');
                if !link.send_frame(&frame, key.swap(false, Ordering::Relaxed), link.now_us()) {
                    break;
                }
                n += 1;
                // 120 a second, without drifting.
                let due = start + Duration::from_micros(n as u64 * 8333);
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
        }));
        println!("{}: client {} ({}x{})", self.label, client.name, client.display.width, client.display.height);
        Ok(Accept { mode: Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 }, width: client.display.width, height: client.display.height, refresh: 120, colour: "bt709".into(), sdr_white: 0, peak: 0 })
    }

    fn keyframe(&mut self) {
        self.key.store(true, Ordering::Relaxed);
    }

    fn input(&mut self, _event: Event) {
        self.inputs += 1;
    }

    fn message(&mut self, message: &Message) {
        let Some(link) = self.link.clone() else { return };
        match message {
            Message::Sound { on } => {
                self.stop_sound();
                if *on {
                    link.send_message(Message::SoundFormat { codec: "opus".into(), rate: 48000, channels: 2, frame_us: 10_000 });
                    let stop = Arc::new(AtomicBool::new(false));
                    let (stopped, label) = (stop.clone(), self.label.clone());
                    let thread = std::thread::spawn(move || {
                        let start = Instant::now();
                        let mut seq = 0u32;
                        while !stopped.load(Ordering::Relaxed) {
                            link.send_sound(seq == 0, seq, link.now_us(), format!("{label} sound {seq}").as_bytes());
                            seq += 1;
                            let due = start + Duration::from_millis(seq as u64 * 10);
                            std::thread::sleep(due.saturating_duration_since(Instant::now()));
                        }
                    });
                    self.sound = Some((stop, thread));
                }
            }
            Message::Configure { display, bitrate, .. } => {
                if let Some(d) = display {
                    // The picture starts afresh at the new size; the next frame is a keyframe.
                    self.key.store(true, Ordering::Relaxed);
                    link.restart(Accept { mode: Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 }, width: d.width, height: d.height, refresh: d.refresh, colour: "bt709".into(), sdr_white: 0, peak: 0 });
                }
                if let Some(b) = bitrate {
                    link.send_message(Message::Status { bitrate: Some(*b) });
                }
            }
            other => println!("{}: {}", self.label, serde_line(other)),
        }
    }

    fn mic(&mut self, packet: &Packet) {
        self.mic = (self.mic.0 + 1, self.mic.1 + packet.data.len() as u32);
    }

    fn camera(&mut self, frame: &Frame) {
        self.camera = (self.camera.0 + 1, self.camera.1 + frame.key as u32);
    }

    fn blob(&mut self, event: &BlobEvent) {
        if std::env::var_os("REMOTEMAN_DEBUG").is_some() {
            eprintln!("{}: blob event {:?}", self.label, match event { BlobEvent::Data { id, bytes } => format!("data {id} {} bytes", bytes.len()), other => format!("{other:?}") });
        }
        let Some(link) = self.link.clone() else { return };
        match event {
            BlobEvent::Open { id, kind, header } => {
                self.blobs.insert(*id, (*kind, header.clone(), Vec::new()));
            }
            BlobEvent::Data { id, bytes } => {
                if let Some(b) = self.blobs.get_mut(id) {
                    b.2.extend_from_slice(bytes);
                }
            }
            BlobEvent::Close { id, complete } => {
                let Some((kind, header, content)) = self.blobs.remove(id) else { return };
                if !complete {
                    println!("{}: a blob was given up after {} bytes", self.label, content.len());
                    return;
                }
                if kind == BLOB_CLIPBOARD {
                    // Back it goes, in capitals, as if copied here.
                    let text = String::from_utf8_lossy(&content).to_uppercase();
                    let seq = serde_json::from_str::<ClipboardHeader>(&header).map(|h| h.seq).unwrap_or(0);
                    let answer = ClipboardHeader { seq: seq + 1, mime: "text/plain;charset=utf-8".into(), size: text.len() as u64 };
                    let out = link.open_blob(BLOB_CLIPBOARD, serde_json::to_string(&answer).unwrap());
                    out.write(text.as_bytes());
                    out.finish();
                } else if kind == BLOB_FILE {
                    let (id, name, size) = serde_json::from_str::<FileHeader>(&header).map(|h| (h.id, h.name, h.size)).unwrap_or((0, String::new(), 0));
                    let sum = content.iter().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(*b as u32));
                    let ok = size == content.len() as u64;
                    let name = remoteman_core::media::safe_file_name(&name);
                    link.send_message(Message::FileResult { id, ok, message: format!("{name}: {} bytes, sum {sum:08x}", content.len()) });
                }
            }
        }
    }

    fn left(&mut self) {
        self.stop_sound();
        self.link = None;
        self.blobs.clear();
        if let Some(stop) = self.stop.take() {
            stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            let ((mic, mic_bytes), (camera, camera_keys)) = (std::mem::take(&mut self.mic), std::mem::take(&mut self.camera));
            println!("{}: client left after {} inputs, {mic} microphone packets ({mic_bytes} bytes), {camera} camera frames ({camera_keys} keyframes)", self.label, std::mem::take(&mut self.inputs));
        }
    }
}

fn serde_line(message: &Message) -> String {
    serde_json::to_string(message).unwrap_or_default()
}

pub fn run(socket: &Path, label: String) -> Result<()> {
    let stream = UnixStream::connect(socket).with_context(|| format!("no daemon at {}", socket.display()))?;
    let handler = StandIn { label, stop: None, thread: None, key: Arc::default(), inputs: 0, link: None, sound: None, mic: (0, 0), camera: (0, 0), blobs: Default::default() };
    let features = ["video", "input", "sound", "mic", "camera", "configure", "clipboard", "files"].map(String::from).to_vec();
    remoteman_core::relay::run_agent(stream, features, handler)
}
