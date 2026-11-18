//! `remotemand test-agent`: an agent with no desktop, for testing the daemon without a GPU or
//! a compositor. It answers a client with made-up frames, 120 a second, each beginning with a
//! line of text saying who sent it and its number, so a saved stream shows exactly which frames
//! reached the client and in what order.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use remoteman_core::host::{Accept, ClientInfo, HostHandler, Link};
use remoteman_core::{Chroma, Codec, Event, Mode};

struct StandIn {
    label: String,
    /// Set to end the thread making frames.
    stop: Option<Arc<AtomicBool>>,
    thread: Option<std::thread::JoinHandle<()>>,
    key: Arc<AtomicBool>,
    inputs: u32,
}

impl HostHandler for StandIn {
    fn connected(&mut self, client: &ClientInfo, link: Link) -> Result<Accept, String> {
        self.left();
        let stop = Arc::new(AtomicBool::new(false));
        let (label, stopped, key) = (self.label.clone(), stop.clone(), self.key.clone());
        key.store(true, Ordering::Relaxed);
        self.stop = Some(stop);
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
        Ok(Accept { mode: Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 }, width: client.display.width, height: client.display.height, refresh: 120, colour: "bt709".into() })
    }

    fn keyframe(&mut self) {
        self.key.store(true, Ordering::Relaxed);
    }

    fn input(&mut self, _event: Event) {
        self.inputs += 1;
    }

    fn left(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            println!("{}: client left after {} inputs", self.label, std::mem::take(&mut self.inputs));
        }
    }
}

pub fn run(socket: &Path, label: String) -> Result<()> {
    let stream = UnixStream::connect(socket).with_context(|| format!("no daemon at {}", socket.display()))?;
    let handler = StandIn { label, stop: None, thread: None, key: Arc::default(), inputs: 0 };
    remoteman_core::relay::run_agent(stream, vec!["video".into(), "input".into()], handler)
}
