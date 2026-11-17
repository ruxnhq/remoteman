//! Spike S2, the real path: DMA-BUF → one Vulkan pass → NVENC through NVIDIA's API.
//! 10-bit throughout, 4:2:0 or 4:4:4, HEVC or AV1. Halfway through it halves the bitrate on the
//! running encoder, and three quarters through it invalidates a reference frame; neither may
//! produce a keyframe.

use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::linux::nvenc::{Codec, Config, Encoder};
use crate::linux::rawcap::{self, Event};
use crate::linux::vkconv::{Chroma, Converter};
use crate::pct;

struct Converted {
    slot: usize,
    captured: Instant,
    convert_ms: f64,
}

pub fn run(node: u32, w: u32, h: u32, fps: u32, secs: u64, codec: Codec, chroma: Chroma) -> Result<()> {
    let bitrate: u32 = std::env::var("RM_BITRATE").ok().and_then(|v| v.parse().ok()).unwrap_or(60_000_000);
    let preset: u32 = std::env::var("RM_PRESET").ok().and_then(|v| v.trim_start_matches('p').parse().ok()).unwrap_or(1);
    let warmup = Duration::from_secs(std::env::var("RM_WARMUP").ok().and_then(|v| v.parse().ok()).unwrap_or(4));
    const SLOTS: usize = 4;

    let mut conv = Converter::new(w, h, chroma, SLOTS).context("Vulkan converter")?;
    let modifiers = conv.modifiers()?;
    anyhow::ensure!(!modifiers.is_empty(), "this GPU offers no DMA-BUF layout for BGRx");
    let mut enc = Encoder::new(&Config { codec, chroma444: chroma == Chroma::C444, width: w, height: h, fps, bitrate, preset }).context("NVENC")?;
    println!("encoder: {codec:?} {chroma:?} 10-bit, P{preset}, {} Mbit/s; {}", bitrate / 1_000_000, enc.note);
    let pitch = conv.pitch;
    for s in &mut conv.slots {
        let idx = enc.add_input(s.fd, s.size, pitch)?;
        s.fd = -1;
        debug_assert!(idx < SLOTS);
    }

    // Two may wait and one may be encoding, so with four slots the next to write is always free.
    let (tx, rx) = mpsc::sync_channel::<Converted>(2);
    let (quit_tx, quit_rx) = pipewire::channel::channel();
    let (mut next, mut dropped_tx) = (0usize, 0u32);
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let dropped_w = dropped.clone();
    let thread = std::thread::Builder::new().name("capture".into()).spawn(move || {
        let on_event = Box::new(move |ev: Event| match ev {
            Event::Format { width, height, modifier } => {
                println!("stream: {width}x{height} DMA-BUF, modifier {modifier:#x}");
                conv.reset_images();
            }
            Event::Frame(f) => {
                if (f.width, f.height) != (conv.width, conv.height) {
                    return;
                }
                let captured = Instant::now();
                match conv.convert(f.fd, f.modifier, f.offset, f.stride, next) {
                    Ok(()) => {
                        let c = Converted { slot: next, captured, convert_ms: captured.elapsed().as_secs_f64() * 1000.0 };
                        if tx.try_send(c).is_ok() {
                            next = (next + 1) % SLOTS;
                        } else {
                            dropped_tx += 1;
                            dropped_w.store(dropped_tx, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    Err(e) => eprintln!("convert: {e:#}"),
                }
            }
        });
        if let Err(e) = rawcap::run(node, (w, h), fps, &modifiers, on_event, quit_rx) {
            eprintln!("capture: {e:#}");
        }
    })?;

    let mut out = match std::env::var("RM_OUT") {
        Ok(p) => Some(std::fs::File::create(&p).with_context(|| format!("create {p}"))?),
        Err(_) => None,
    };
    let (mut arrivals, mut conv_ms, mut enc_ms, mut total_ms) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut bytes_a, mut bytes_b, mut frames_a, mut frames_b, mut keys, mut n) = (0usize, 0usize, 0u32, 0u32, 0u32, 0u64);
    let (mut halved, mut invalidated) = (None::<Instant>, false);
    let (mut key_after_change, mut key_after_invalidate) = (false, false);
    let start = Instant::now();
    let end = warmup + Duration::from_secs(secs);
    while start.elapsed() < end {
        let Ok(c) = rx.recv_timeout(Duration::from_millis(200)) else { continue };
        let measured = start.elapsed() >= warmup;
        let since = start.elapsed().saturating_sub(warmup);
        if measured && halved.is_none() && since >= Duration::from_secs(secs) / 2 {
            enc.set_bitrate(bitrate / 2).context("change bitrate")?;
            halved = Some(Instant::now());
        }
        let mut just_invalidated = false;
        if measured && !invalidated && since >= Duration::from_secs(secs) * 3 / 4 && n > 2 {
            // Pretend the frame before last never arrived.
            enc.invalidate(n - 2).context("invalidate reference")?;
            (invalidated, just_invalidated) = (true, true);
        }
        let t = Instant::now();
        let (data, key) = enc.encode(c.slot, n, n == 0)?;
        let e = t.elapsed().as_secs_f64() * 1000.0;
        n += 1;
        if let Some(f) = out.as_mut() {
            f.write_all(data)?;
        }
        if measured {
            arrivals.push(c.captured);
            conv_ms.push(c.convert_ms);
            enc_ms.push(e);
            total_ms.push(c.captured.elapsed().as_secs_f64() * 1000.0);
            keys += key as u32;
            match halved {
                None => {
                    bytes_a += data.len();
                    frames_a += 1;
                }
                // Give the rate control half a second to settle before counting.
                Some(at) if at.elapsed() > Duration::from_millis(500) => {
                    bytes_b += data.len();
                    frames_b += 1;
                }
                Some(at) => key_after_change |= key && at.elapsed() < Duration::from_millis(500),
            }
            key_after_invalidate |= just_invalidated && key;
        }
    }
    let _ = quit_tx.send(());
    let _ = thread.join();

    let count = arrivals.len();
    println!("frames {count}, dropped before the encoder {}", dropped.load(std::sync::atomic::Ordering::Relaxed));
    if count > 2 {
        let span = (arrivals[count - 1] - arrivals[0]).as_secs_f64();
        let mut iv: Vec<f64> = arrivals.windows(2).map(|p| (p[1] - p[0]).as_secs_f64() * 1000.0).collect();
        let sort = |v: &mut Vec<f64>| v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        sort(&mut iv);
        sort(&mut conv_ms);
        sort(&mut enc_ms);
        sort(&mut total_ms);
        let line = |name: &str, v: &[f64]| println!("{name} ms: median {:.2}  p99 {:.2}  max {:.2}", pct(v, 0.5), pct(v, 0.99), v[v.len() - 1]);
        println!("rate {:.1} fps", (count - 1) as f64 / span);
        line("interval", &iv);
        line("convert ", &conv_ms);
        line("encode  ", &enc_ms);
        line("capture to encoded", &total_ms);
        let mbit = |bytes: usize, frames: u32| bytes as f64 * 8.0 * fps as f64 / frames.max(1) as f64 / 1e6;
        println!(
            "bitrate: {:.1} Mbit/s asked {}, then {:.1} asked {} (keyframe at the change: {})",
            mbit(bytes_a, frames_a),
            bitrate / 1_000_000,
            mbit(bytes_b, frames_b),
            bitrate / 2_000_000,
            if key_after_change { "YES" } else { "no" }
        );
        println!("reference invalidation: {} (keyframe after it: {})", if invalidated { "accepted" } else { "not run" }, if key_after_invalidate { "YES" } else { "no" });
        println!("keyframes while measuring: {keys}");
    }
    Ok(())
}
