//! Tests of the Direct3D 12 encoder: a synthetic moving picture through the converter and the
//! encoder, the stream decoded by FFmpeg (C:\ffmpeg\bin, or REMOTEMAN_FFMPEG). They skip where
//! there is no Direct3D 12 encoder. REMOTEMAN_TEST_OUT keeps the bitstreams.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use remoteman_core::{Chroma, Codec, Mode};
use remoteman_encode::FrameEncoder;
use remoteman_encode::recovery::{Recovery, Sent};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_P010, DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

use crate::capture::Gpu;
use crate::convert::{Converter, Input, Output};
use crate::d3d12enc::Encoder;
use crate::encode::Settings;

const W: u32 = 1920;
const H: u32 = 1080;
const FPS: u32 = 60;

fn gpu() -> Option<Gpu> {
    // SAFETY: plain DXGI enumeration.
    let (luid, name) = unsafe {
        let f: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let want = std::env::var("REMOTEMAN_D3D12_VENDOR").ok().and_then(|v| u32::from_str_radix(&v, 16).ok());
        let mut best = None;
        let mut i = 0;
        while let Ok(a) = f.EnumAdapters1(i) {
            let d = a.GetDesc1().ok()?;
            if d.Flags & 2 == 0 && want.is_none_or(|w| w == d.VendorId) && best.is_none() {
                best = Some(((d.AdapterLuid.LowPart, d.AdapterLuid.HighPart), String::from_utf16_lossy(&d.Description)));
            }
            i += 1;
        }
        best?
    };
    eprintln!("adapter: {}", name.trim_end_matches('\0'));
    Gpu::on_adapter(luid).ok()
}

fn ffmpeg() -> PathBuf {
    std::env::var_os("REMOTEMAN_FFMPEG").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ffmpeg\bin\ffmpeg.exe"))
}

fn out_dir() -> (PathBuf, bool) {
    match std::env::var_os("REMOTEMAN_TEST_OUT") {
        Some(d) => (d.into(), true),
        None => (std::env::temp_dir().join(format!("remoteman-d3d12-{}", std::process::id())), false),
    }
}

struct Source {
    tex: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
    staging: ID3D11Texture2D,
}

fn source(gpu: &Gpu) -> Source {
    // SAFETY: Direct3D calls as documented.
    unsafe {
        let desc = D3D11_TEXTURE2D_DESC { Width: W, Height: H, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_R10G10B10A2_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
        let mut t = None;
        gpu.device.CreateTexture2D(&desc, None, Some(&mut t)).unwrap();
        let t = t.unwrap();
        let mut v = None;
        gpu.device.CreateShaderResourceView(&t, None, Some(&mut v)).unwrap();
        let sd = D3D11_TEXTURE2D_DESC { Width: W, Height: H, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_P010, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_STAGING, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, ..Default::default() };
        let mut s = None;
        gpu.device.CreateTexture2D(&sd, None, Some(&mut s)).unwrap();
        Source { tex: t, view: v.unwrap(), staging: s.unwrap() }
    }
}

/// A moving picture: a gradient, drifting stripes, a box that travels, some texture.
fn paint(gpu: &Gpu, s: &Source, f: u32) {
    let mut px = vec![0u32; (W * H) as usize];
    let (bx, by) = ((f * 9) % (W - 400), 200 + (f * 4) % 500);
    for y in 0..H {
        for x in 0..W {
            let mut r = (x * 700 / W + 100) as i32;
            let mut g = (y * 700 / H + 100) as i32;
            let mut b = 400 + (((x + f * 6) / 24 + y / 24) % 2) as i32 * 200;
            let tex = ((x * 31 + y * 17 + f * 5) % 23) as i32 - 11;
            r += tex;
            g += tex;
            b += tex;
            if x >= bx && x < bx + 400 && y >= by && y < by + 300 {
                r = 900;
                g = 200 + ((x - bx) * 2) as i32;
                b = 100 + ((y - by) * 2) as i32;
            }
            px[(y * W + x) as usize] = (r.clamp(0, 1023) as u32) | (g.clamp(0, 1023) as u32) << 10 | (b.clamp(0, 1023) as u32) << 20 | 3 << 30;
        }
    }
    // SAFETY: the pixels live through the call.
    unsafe { gpu.context.UpdateSubresource(&s.tex, 0, None, px.as_ptr() as *const _, W * 4, 0) };
}

fn dump(gpu: &Gpu, s: &Source, converted: &ID3D11Texture2D, out: &mut dyn Write) {
    // SAFETY: a staging texture mapped for reading, rows copied out within its pitch.
    unsafe {
        gpu.context.CopyResource(&s.staging, converted);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context.Map(&s.staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
        let base = m.pData as *const u8;
        for y in 0..H + H / 2 {
            out.write_all(std::slice::from_raw_parts(base.add(y as usize * m.RowPitch as usize), W as usize * 2)).unwrap();
        }
        gpu.context.Unmap(&s.staging, 0);
    }
}

/// NAL types in a frame's data.
fn nal_types(d: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    let mut i = 0;
    while i + 4 < d.len() {
        if d[i..i + 3] == [0, 0, 1] {
            v.push((d[i + 3] >> 1) & 63);
            i += 3;
        } else {
            i += 1;
        }
    }
    v
}

struct Frame {
    data: Vec<u8>,
    key: bool,
    recovery: bool,
    ms: f64,
}

/// Encode `count` frames the pipeline's way: `lost_at(frame)` gives a loss report to answer before
/// that frame, `rate_at(frame)` a new bitrate. With `src`, the converted pictures are written to it
/// as raw P010.
fn run(gpu: &Gpu, count: u32, bitrate: u32, mut lost_at: impl FnMut(u32) -> Option<(u32, u32)>, mut rate_at: impl FnMut(u32) -> Option<u32>, mut src: Option<&mut dyn Write>) -> Result<(Vec<Frame>, String)> {
    let s = source(gpu);
    let conv = Converter::new(gpu, W, H, Input::Encoded, Output::Sdr, false)?;
    let settings = Settings { mode: Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 }, width: W, height: H, fps: FPS, bitrate, hdr: false, light: None };
    let mut e = Encoder::open(gpu, &conv.output, &settings)?;
    let note = e.note().to_owned();
    let mut r = Recovery::new(e.short_term(), e.long_term(), FPS);
    let mut frames = Vec::new();
    for f in 0..count {
        paint(gpu, &s, f);
        conv.convert(gpu, &s.view);
        if let Some(w) = src.as_deref_mut() {
            dump(gpu, &s, &conv.output, w);
        }
        if let Some(b) = rate_at(f) {
            e.set_bitrate(b)?;
        }
        let n = r.number();
        let reported = lost_at(f);
        let plan = r.plan(n == 0, reported, &mut |t| e.invalidate(t));
        let t = std::time::Instant::now();
        let made = e.encode_how(0, n, plan.how)?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let (data, key) = (made.data.to_vec(), made.key);
        let sent = r.made(&plan, key, None, 0);
        frames.push(Frame { data, key, recovery: sent == Sent::Recovery, ms });
    }
    Ok((frames, note))
}

fn decode(path: &std::path::Path, raw: &std::path::Path) -> (String, u64) {
    let o = Command::new(ffmpeg()).args(["-v", "error", "-y", "-i"]).arg(path).args(["-f", "rawvideo", "-pix_fmt", "p010le"]).arg(raw).output().expect("ffmpeg");
    let size = std::fs::metadata(raw).map(|m| m.len()).unwrap_or(0);
    (String::from_utf8_lossy(&o.stderr).into_owned(), size / (W as u64 * H as u64 * 3))
}

fn psnr(src: &std::path::Path, stream: &std::path::Path) -> String {
    let o = Command::new(ffmpeg())
        .args(["-hide_banner", "-f", "rawvideo", "-pix_fmt", "p010le", "-video_size", &format!("{W}x{H}"), "-i"])
        .arg(src)
        .arg("-i")
        .arg(stream)
        .args(["-lavfi", "[0:v][1:v]psnr", "-f", "null", "-"])
        .output()
        .expect("ffmpeg");
    String::from_utf8_lossy(&o.stderr).lines().filter(|l| l.contains("PSNR")).collect::<Vec<_>>().join(" | ")
}

fn frames_wanted() -> u32 {
    std::env::var("REMOTEMAN_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(120)
}

fn setup() -> Option<(Gpu, PathBuf, bool)> {
    let gpu = gpu()?;
    let (dir, keep) = out_dir();
    std::fs::create_dir_all(&dir).unwrap();
    Some((gpu, dir, keep))
}

fn join(frames: &[Frame], skip: std::ops::RangeInclusive<usize>) -> Vec<u8> {
    frames.iter().enumerate().filter(|(i, _)| !skip.contains(i)).flat_map(|(_, f)| f.data.iter().copied()).collect()
}

fn stats(label: &str, frames: &[Frame], bitrate: u32) {
    let total: usize = frames.iter().map(|f| f.data.len()).sum();
    let mut ms: Vec<f64> = frames.iter().skip(1).map(|f| f.ms).collect();
    ms.sort_by(f64::total_cmp);
    eprintln!(
        "{label}: {} frames, {} bytes/frame average (target {} at {FPS} fps), keyframe {} bytes, encode latency median {:.2} ms p99 {:.2} ms worst {:.2} ms",
        frames.len(),
        total / frames.len(),
        bitrate as usize / 8 / FPS as usize,
        frames[0].data.len(),
        ms[ms.len() / 2],
        ms[ms.len() * 99 / 100],
        ms[ms.len() - 1]
    );
}

#[test]
fn plain_stream() {
    let Some((gpu, dir, keep)) = setup() else { return eprintln!("no GPU: skipped") };
    let srcp = dir.join("src.p010");
    let mut w = std::io::BufWriter::new(std::fs::File::create(&srcp).unwrap());
    let (frames, note) = match run(&gpu, frames_wanted(), 20_000_000, |_| None, |_| None, Some(&mut w)) {
        Ok(x) => x,
        Err(e) => return eprintln!("no Direct3D 12 encoder: {e:?}"),
    };
    w.flush().unwrap();
    eprintln!("{note}");
    stats("plain", &frames, 20_000_000);
    let stream = dir.join("plain.h265");
    std::fs::write(&stream, join(&frames, 1000..=1000)).unwrap();
    let (err, n) = decode(&stream, &dir.join("plain.yuv"));
    eprintln!("decoded {n} frames; errors: {err:?}");
    eprintln!("{}", psnr(&srcp, &stream));
    if !keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(err.trim().is_empty() && n == frames_wanted() as u64);
}

#[test]
fn live_rate_change() {
    let Some((gpu, dir, keep)) = setup() else { return eprintln!("no GPU: skipped") };
    let (frames, _) = match run(&gpu, 120, 20_000_000, |_| None, |f| (f == 60).then_some(5_000_000), None) {
        Ok(x) => x,
        Err(e) => return eprintln!("no Direct3D 12 encoder: {e:?}"),
    };
    let avg = |a: usize, b: usize| frames[a..b].iter().map(|f| f.data.len()).sum::<usize>() / (b - a);
    eprintln!("rate change at 60: bytes/frame 30..60 {} (20 Mbit/s = {}), 90..120 {} (5 Mbit/s = {})", avg(30, 60), 20_000_000 / 8 / FPS, avg(90, 120), 5_000_000 / 8 / FPS);
    let idrs: Vec<usize> = frames.iter().enumerate().filter(|(_, f)| nal_types(&f.data).iter().any(|&k| (16..=21).contains(&k))).map(|(i, _)| i).collect();
    eprintln!("IDR frames: {idrs:?}");
    let stream = dir.join("rate.h265");
    std::fs::write(&stream, join(&frames, 1000..=1000)).unwrap();
    let (err, n) = decode(&stream, &dir.join("rate.yuv"));
    eprintln!("decoded {n} frames; errors: {err:?}");
    if !keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert_eq!(idrs, [0]);
    assert!(err.trim().is_empty() && n == 120);
}

#[test]
fn recovery_without_a_keyframe() {
    let Some((gpu, dir, keep)) = setup() else { return eprintln!("no GPU: skipped") };
    // Frames 60 to 62 are lost; the report reaches the host before frame 63.
    let (frames, _) = match run(&gpu, 120, 20_000_000, |f| (f == 63).then_some((60, 62)), |_| None, None) {
        Ok(x) => x,
        Err(e) => return eprintln!("no Direct3D 12 encoder: {e:?}"),
    };
    let t63 = nal_types(&frames[63].data);
    eprintln!("frame 63: key {} recovery {} NAL types {t63:?}, {} bytes (frame 62: {})", frames[63].key, frames[63].recovery, frames[63].data.len(), frames[62].data.len());
    let stream = dir.join("recovery.h265");
    std::fs::write(&stream, join(&frames, 60..=62)).unwrap();
    let (err, n) = decode(&stream, &dir.join("recovery.yuv"));
    eprintln!("decoded {n} frames of the edited stream (117 expected); errors: {err:?}");
    // A late report: 50..62 lost, answered at 70, so only an anchor older than 50 will do.
    let (frames2, _) = run(&gpu, 120, 20_000_000, |f| (f == 70).then_some((50, 62)), |_| None, None).unwrap();
    eprintln!("late report (50.. lost, answered at 70): key {} recovery {}", frames2[70].key, frames2[70].recovery);
    std::fs::write(&stream, join(&frames2, 50..=69)).unwrap();
    let (err2, n2) = decode(&stream, &dir.join("recovery2.yuv"));
    eprintln!("decoded {n2} frames (100 expected); errors: {err2:?}");
    if !keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(!frames[63].key && frames[63].recovery);
    assert!(t63.iter().all(|&k| k < 16), "a recovery frame is not an IRAP picture");
    assert!(err.trim().is_empty() && n == 117);
}
