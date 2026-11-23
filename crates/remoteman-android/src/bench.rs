//! The benchmark's timing pass: a recorded stream (RMREC1, from remoteman-testclient --record)
//! played through the app's own decoder into a Surface, at the stream's rate, timing each
//! picture from being handed to the decoder to its release to the screen.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ndk::native_window::NativeWindow;
use remoteman_core::{Chroma, Codec, Frame, Mode, StreamInfo};

use crate::decoder::Decoder;

static KEPT: std::sync::Mutex<Option<Decoder>> = std::sync::Mutex::new(None);

/// Let the last run's decoder go.
pub fn release() {
    KEPT.lock().unwrap().take();
}

pub fn run(path: &str, window: NativeWindow) -> Result<String> {
    let data = std::fs::read(path).with_context(|| format!("read {path}"))?;
    if !data.starts_with(b"RMREC1\n") {
        bail!("{path} isn't a recording");
    }
    let nl = data[7..].iter().position(|&b| b == b'\n').context("no stream description")? + 7;
    let desc: serde_json::Value = serde_json::from_slice(&data[7..nl])?;
    let codec = match desc["codec"].as_str() {
        Some("av1") => Codec::Av1,
        Some("hevc") => Codec::Hevc,
        other => bail!("codec {other:?}"),
    };
    let chroma = if desc["chroma"].as_str() == Some("444") { Chroma::C444 } else { Chroma::C420 };
    let num = |k: &str| desc[k].as_u64().unwrap_or(0) as u32;
    let info = StreamInfo {
        epoch: 1,
        mode: Mode { codec, chroma, depth: num("depth") as u8 },
        width: num("width"),
        height: num("height"),
        refresh: num("refresh").max(1),
        colour: desc["colour"].as_str().unwrap_or("bt709").to_owned(),
        sdr_white: 203,
        peak: 1000,
    };
    let mut frames = Vec::new();
    let mut i = nl + 1;
    while i + 17 <= data.len() {
        let len = u32::from_le_bytes(data[i..i + 4].try_into()?) as usize;
        let key = data[i + 4] != 0;
        let number = u32::from_le_bytes(data[i + 5..i + 9].try_into()?);
        let capture_us = u64::from_le_bytes(data[i + 9..i + 17].try_into()?);
        let body = data.get(i + 17..i + 17 + len).context("a frame cut short")?;
        frames.push(Frame { key, recovery: false, epoch: 1, number, capture_us, data: body.to_vec() });
        i += 17 + len;
    }

    release();
    crate::decoder::INPUT_WAIT_MS.store(2000, Ordering::Relaxed);
    let mut decoder = Decoder::new(window);
    let stats = decoder.stats();
    stats.timing.store(true, Ordering::Relaxed);
    decoder.stream(&info);
    let interval = Duration::from_secs_f64(1.0 / info.refresh as f64);
    let start = Instant::now();
    let mut refused = 0;
    for (n, f) in frames.iter().enumerate() {
        let due = start + interval * n as u32;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if !decoder.frame(f, None) {
            refused += 1;
        }
    }
    let fed = start.elapsed();
    // The last pictures come out a little after the last frame went in.
    let until = Instant::now() + Duration::from_secs(2);
    while (stats.decoded.load(Ordering::Relaxed) as usize) < frames.len() - stats.dropped.load(Ordering::Relaxed) as usize && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    crate::decoder::INPUT_WAIT_MS.store(4, Ordering::Relaxed);
    let name = stats.name.lock().unwrap().clone();
    let mut times = stats.decode_us.lock().unwrap().clone();
    // The decoder, and its last picture on the screen, stay until release(): the app copies
    // the screen first.
    *KEPT.lock().unwrap() = Some(decoder);
    times.sort_unstable();
    let pct = |p: f64| if times.is_empty() { 0.0 } else { times[((times.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0 };
    Ok(serde_json::json!({
        "decoder": name,
        "frames": frames.len(),
        "shown": stats.decoded.load(Ordering::Relaxed),
        "dropped": stats.dropped.load(Ordering::Relaxed),
        "refused": refused,
        "fed_s": fed.as_secs_f64(),
        "decode_ms": { "median": pct(0.5), "p95": pct(0.95), "p99": pct(0.99), "max": pct(1.0) },
    })
    .to_string())
}
