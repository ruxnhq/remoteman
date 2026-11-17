//! Spikes S1 and S2. `capture-spike NODE WIDTH HEIGHT FPS SECONDS [none|hevc|av1]`
//!
//! Reads the PipeWire node of a Mutter virtual monitor, reports how frames arrive (rate, pacing,
//! GPU or CPU memory), and optionally encodes each one with NVENC and reports the time it takes.

mod direct;
mod linux;

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use linux::capture::{Capture, FrameData};
use linux::encode::{Encoder, EncoderConfig, VideoCodec};

pub fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() { 0.0 } else { sorted[((sorted.len() - 1) as f64 * p) as usize] }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let a: Vec<String> = std::env::args().collect();
    anyhow::ensure!(a.len() >= 6, "usage: capture-spike NODE WIDTH HEIGHT FPS SECONDS [none|hevc|av1]");
    let node: u32 = a[1].parse()?;
    let (w, h, fps, secs): (u32, u32, u32, u64) = (a[2].parse()?, a[3].parse()?, a[4].parse()?, a[5].parse()?);
    // "direct-hevc", "direct-hevc444", "direct-av1": the real path (Vulkan pass, NVENC's own API).
    if let Some(mode) = a.get(6).and_then(|m| m.strip_prefix("direct-")) {
        use linux::{nvenc::Codec, vkconv::Chroma};
        let (codec, chroma) = match mode {
            "hevc" => (Codec::Hevc, Chroma::C420),
            "hevc444" => (Codec::Hevc, Chroma::C444),
            "av1" => (Codec::Av1, Chroma::C420),
            _ => anyhow::bail!("direct modes: direct-hevc, direct-hevc444, direct-av1"),
        };
        return direct::run(node, w, h, fps, secs, codec, chroma);
    }
    let codec = match a.get(6).map(String::as_str) {
        Some("hevc") => Some(VideoCodec::Hevc),
        Some("av1") => Some(VideoCodec::Av1),
        _ => None,
    };
    let bitrate: u32 = std::env::var("RM_BITRATE").ok().and_then(|v| v.parse().ok()).unwrap_or(60_000_000);

    let mut cap = Capture::from_node(node, (w, h), fps).context("open stream")?;
    let mut enc: Option<Encoder> = None;
    let (mut arrivals, mut enc_ms, mut bytes, mut gpu, mut cpu, mut keys) = (Vec::new(), Vec::new(), 0usize, 0u32, 0u32, 0u32);
    let mut size = (0, 0);
    let (mut last_sec, mut last_d, mut per_sec) = (0u64, 0u32, Vec::new());
    let start = Instant::now();
    let warmup = Duration::from_secs(std::env::var("RM_WARMUP").ok().and_then(|v| v.parse().ok()).unwrap_or(2));
    while start.elapsed() < Duration::from_secs(secs) + warmup {
        let sec = start.elapsed().as_secs();
        if sec != last_sec {
            let d = linux::capture::DEQUEUED.load(std::sync::atomic::Ordering::Relaxed);
            per_sec.push(d - last_d);
            (last_sec, last_d) = (sec, d);
        }
        let Some(frame) = cap.next_frame(Duration::from_millis(200))? else { continue };
        let measured = start.elapsed() >= warmup;
        size = (frame.width, frame.height);
        if measured {
            arrivals.push(frame.captured);
            match frame.data {
                FrameData::Gpu(_) => gpu += 1,
                FrameData::Cpu { .. } => cpu += 1,
            }
        }
        if let Some(codec) = codec {
            if enc.is_none() {
                enc = Some(Encoder::new(EncoderConfig { codec, width: frame.width, height: frame.height, fps, bitrate }, &frame)?);
            }
            let t = Instant::now();
            let out = enc.as_mut().unwrap().encode(&frame, start.elapsed().as_nanos() as i64 / 100)?;
            if measured {
                enc_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                bytes += out.iter().map(|f| f.data.len()).sum::<usize>();
                keys += out.iter().filter(|f| f.keyframe).count() as u32;
            }
        }
        cap.recycle(frame);
    }
    println!("buffers dequeued {} (empty {})", linux::capture::DEQUEUED.load(std::sync::atomic::Ordering::Relaxed), linux::capture::EMPTY.load(std::sync::atomic::Ordering::Relaxed));
    println!("buffers per second: {per_sec:?}");
    {
        use std::sync::atomic::Ordering::Relaxed;
        let d = linux::capture::DEQUEUED.load(Relaxed).max(1) as f64;
        println!(
            "import ms: mean {:.2}  max {:.2}  over 6 ms: {}",
            linux::capture::IMPORT_US_SUM.load(Relaxed) as f64 / 1000.0 / d,
            linux::capture::IMPORT_US_MAX.load(Relaxed) as f64 / 1000.0,
            linux::capture::IMPORT_SLOW.load(Relaxed)
        );
    }
    let n = arrivals.len();
    println!("size {}x{}  frames {} (gpu {}, cpu {})", size.0, size.1, n, gpu, cpu);
    if n > 2 {
        let span = (arrivals[n - 1] - arrivals[0]).as_secs_f64();
        let mut iv: Vec<f64> = arrivals.windows(2).map(|p| (p[1] - p[0]).as_secs_f64() * 1000.0).collect();
        iv.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("rate {:.1} fps  interval ms: median {:.2}  p99 {:.2}  max {:.2}", (n - 1) as f64 / span, pct(&iv, 0.5), pct(&iv, 0.99), iv[iv.len() - 1]);
        if !enc_ms.is_empty() {
            enc_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
            println!(
                "encode {} ms: median {:.2}  p99 {:.2}  max {:.2}  | {:.1} Mbit/s, {} keyframes",
                enc.as_ref().map(|e| e.name.as_str()).unwrap_or(""),
                pct(&enc_ms, 0.5), pct(&enc_ms, 0.99), enc_ms[enc_ms.len() - 1],
                bytes as f64 * 8.0 / span / 1e6, keys
            );
        }
    }
    Ok(())
}
