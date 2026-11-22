//! Encoder-level tests of the encoders that are not NVENC (Media Foundation's, and Direct3D 12's
//! again for the long run), on any GPU: a synthetic moving picture through the real converter and
//! the encoder, the stream decoded by FFmpeg (C:\ffmpeg\bin, or `REMOTEMAN_FFMPEG`).
//!
//! `REMOTEMAN_TEST_ADAPTER` says which GPU: a PCI vendor id in hex (`1002` AMD, `10de` NVIDIA,
//! `8086` Intel) or a part of its name. With it set, an encoder that won't open is a failure;
//! without it the test skips (as on a computer with no such encoder). `REMOTEMAN_TEST_OUT` keeps
//! the streams. `REMOTEMAN_LONG_FRAMES` sets the length of the long runs (3200).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use remoteman_core::{Chroma, Codec, Mode};
use remoteman_encode::{FrameEncoder, How};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_SAMPLE_DESC};

use crate::capture::Gpu;
use crate::convert::{Converter, Input, Output};
use crate::d3d12test::{FPS, ONE_AT_A_TIME, decode, dump, ffmpeg, gpu, nal_types, out_dir, paint, size, source};
use crate::encode::Settings;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Which {
    MediaFoundation,
    Direct3D12,
}

fn open(which: Which, gpu: &Gpu, input: &ID3D11Texture2D, s: &Settings) -> Result<Box<dyn FrameEncoder>> {
    Ok(match which {
        Which::MediaFoundation => Box::new(crate::mf::Encoder::open(gpu, input, s)?),
        Which::Direct3D12 => Box::new(crate::d3d12enc::Encoder::open(gpu, input, s)?),
    })
}

/// A GPU without the encoder skips the test, unless the test was told which GPU to use.
fn missing(e: &anyhow::Error) {
    assert!(std::env::var_os("REMOTEMAN_TEST_ADAPTER").is_none(), "the encoder on the GPU asked for failed to open: {e:?}");
    eprintln!("no such encoder here: skipped ({e:#})");
}

struct Frame {
    data: Vec<u8>,
    key: bool,
    ms: f64,
}

struct Done {
    frames: Vec<Frame>,
    note: String,
}

/// A moving picture of any size: a gradient, drifting stripes, texture, a box that travels.
fn paint_small(gpu: &Gpu, tex: &ID3D11Texture2D, w: u32, h: u32, f: u32) {
    let mut px = vec![0u32; (w * h) as usize];
    let (bw, bh) = (w / 5, h / 4);
    let (bx, by) = ((f * 5) % (w - bw), h / 6 + (f * 3) % (h / 2));
    for y in 0..h {
        for x in 0..w {
            let mut r = (x * 700 / w + 100) as i32;
            let mut g = (y * 700 / h + 100) as i32;
            let mut b = 400 + (((x + f * 3) / 12 + y / 12) % 2) as i32 * 200;
            let tex = ((x * 31 + y * 17 + f * 5) % 23) as i32 - 11;
            r += tex;
            g += tex;
            b += tex;
            if x >= bx && x < bx + bw && y >= by && y < by + bh {
                r = 900;
                g = 200 + ((x - bx) * 2) as i32;
                b = 100 + ((y - by) * 2) as i32;
            }
            px[(y * w + x) as usize] = (r.clamp(0, 1023) as u32) | (g.clamp(0, 1023) as u32) << 10 | (b.clamp(0, 1023) as u32) << 20 | 3 << 30;
        }
    }
    // SAFETY: the pixels live through the call.
    unsafe { gpu.context.UpdateSubresource(tex, 0, None, px.as_ptr() as *const _, w * 4, 0) };
}

/// Encode `count` pictures of `w` x `h` the pipeline's way. `key_at` asks for a keyframe before
/// that picture, `rate_at` for a new bitrate. With `src`, the converted pictures are written as
/// raw P010 (only at the size of the shared test picture).
fn run(gpu: &Gpu, which: Which, (w, h): (u32, u32), count: u32, bitrate: u32, key_at: impl Fn(u32) -> bool, rate_at: impl Fn(u32) -> Option<u32>, mut src: Option<&mut dyn Write>) -> Result<Done> {
    // SAFETY: Direct3D calls as documented.
    let (tex, view) = unsafe {
        let desc = D3D11_TEXTURE2D_DESC { Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_R10G10B10A2_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
        let mut t = None;
        gpu.device.CreateTexture2D(&desc, None, Some(&mut t))?;
        let t = t.unwrap();
        let mut v = None;
        gpu.device.CreateShaderResourceView(&t, None, Some(&mut v))?;
        (t, v.unwrap())
    };
    let shared = (w, h) == size();
    let big = shared.then(|| source(gpu));
    let conv = Converter::new(gpu, w, h, Input::Encoded, Output::Sdr, false)?;
    let settings = Settings { mode: Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 }, width: w, height: h, fps: FPS, bitrate, hdr: false, light: None };
    let mut e = open(which, gpu, &conv.output, &settings)?;
    let note = e.note().to_owned();
    let mut frames = Vec::with_capacity(count as usize);
    for f in 0..count {
        match &big {
            Some(s) => {
                paint(gpu, s, f);
                conv.convert(gpu, &s.view);
                if let Some(out) = src.as_deref_mut() {
                    dump(gpu, s, &conv.output, out);
                }
            }
            None => {
                paint_small(gpu, &tex, w, h, f);
                conv.convert(gpu, &view);
            }
        }
        if let Some(b) = rate_at(f) {
            e.set_bitrate(b)?;
        }
        let t = std::time::Instant::now();
        let made = e.encode_how(0, f as u64, How { idr: key_at(f), ..Default::default() })?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        frames.push(Frame { data: made.data.to_vec(), key: made.key, ms });
    }
    Ok(Done { frames, note })
}

fn frames_of(f: &[Frame]) -> Vec<u8> {
    f.iter().flat_map(|f| f.data.iter().copied()).collect()
}

fn latency(label: &str, frames: &[Frame]) {
    let mut ms: Vec<f64> = frames.iter().skip(1).map(|f| f.ms).collect();
    ms.sort_by(f64::total_cmp);
    eprintln!("{label}: encode latency median {:.2} ms p99 {:.2} ms worst {:.2} ms", ms[ms.len() / 2], ms[ms.len() * 99 / 100], ms[ms.len() - 1]);
}

fn average(frames: &[Frame], a: usize, b: usize) -> usize {
    frames[a..b].iter().map(|f| f.data.len()).sum::<usize>() / (b - a)
}

fn target(bitrate: u32) -> usize {
    bitrate as usize / 8 / FPS as usize
}

fn setup() -> Option<(Gpu, PathBuf, bool)> {
    let gpu = gpu()?;
    let (dir, keep) = out_dir();
    std::fs::create_dir_all(&dir).unwrap();
    Some((gpu, dir, keep))
}

fn key_indices(frames: &[Frame]) -> Vec<usize> {
    frames.iter().enumerate().filter(|(_, f)| nal_types(&f.data).iter().any(|&k| (16..=21).contains(&k))).map(|(i, _)| i).collect()
}

/// Which of the stream's HEVC pictures are B (slice type 0), what the longest list of earlier
/// pictures a slice names is, and how the picture order counts run: from FFmpeg's header trace.
#[derive(Debug, Default)]
struct Numbering {
    pictures: usize,
    b_slices: usize,
    max_negative: u32,
    max_positive: u32,
    /// Pictures whose count is not one more than the one before (not counting keyframes).
    jumps: Vec<(usize, u32, u32)>,
    log2_max_poc: u32,
    /// The pictures that name more earlier pictures than the usual one.
    wide: Vec<(usize, u32)>,
}

fn numbering(stream: &Path) -> Numbering {
    let o = Command::new(ffmpeg()).args(["-hide_banner", "-i"]).arg(stream).args(["-c", "copy", "-bsf:v", "trace_headers", "-f", "null", "-"]).output().expect("ffmpeg");
    let text = String::from_utf8_lossy(&o.stderr);
    let mut n = Numbering::default();
    let mut last: Option<u32> = None;
    let mut slice_nal = 0u32;
    let mut first_in_pic = false;
    for line in text.lines() {
        let Some((_, rest)) = line.split_once("] ") else { continue };
        let parts: Vec<&str> = rest.split_whitespace().collect();
        if parts.len() < 5 || parts[parts.len() - 2] != "=" {
            continue;
        }
        let (name, value) = (parts[1], parts[parts.len() - 1].parse::<i64>().unwrap_or(0));
        match name {
            "log2_max_pic_order_cnt_lsb_minus4" => n.log2_max_poc = value as u32 + 4,
            "nal_unit_type" => slice_nal = value as u32,
            "first_slice_segment_in_pic_flag" => {
                first_in_pic = value == 1;
                if first_in_pic {
                    n.pictures += 1;
                    if (16..=21).contains(&slice_nal) {
                        last = None;
                    }
                }
            }
            "slice_type" if value == 0 => n.b_slices += 1,
            "slice_pic_order_cnt_lsb" if first_in_pic => {
                let v = value as u32;
                if let Some(l) = last {
                    if (l + 1) & ((1 << n.log2_max_poc) - 1) != v {
                        n.jumps.push((n.pictures - 1, l, v));
                    }
                }
                last = Some(v);
            }
            "num_negative_pics" if first_in_pic => {
                n.max_negative = n.max_negative.max(value as u32);
                if value > 1 {
                    n.wide.push((n.pictures - 1, value as u32));
                }
            }
            "num_positive_pics" if first_in_pic => n.max_positive = n.max_positive.max(value as u32),
            _ => {}
        }
    }
    n
}

/// 1080p: 120 pictures at 20 Mbit/s, decoded, PSNR against what went in.
fn stream(which: Which) {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some((gpu, dir, keep)) = setup() else { return eprintln!("no GPU: skipped") };
    let srcp = dir.join("src.p010");
    let mut w = std::io::BufWriter::new(std::fs::File::create(&srcp).unwrap());
    let bitrate: u32 = std::env::var("REMOTEMAN_TEST_BITRATE").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000_000);
    let done = match run(&gpu, which, size(), 120, bitrate, |_| false, |_| None, Some(&mut w)) {
        Ok(d) => d,
        Err(e) => return missing(&e),
    };
    w.flush().unwrap();
    eprintln!("{}", done.note);
    let total: usize = done.frames.iter().map(|f| f.data.len()).sum();
    eprintln!("{which:?} plain: {} bytes/frame average (target {}), keyframe {} bytes, keyframes at {:?}", total / 120, target(bitrate), done.frames[0].data.len(), key_indices(&done.frames));
    latency("plain", &done.frames);
    for (i, f) in done.frames.iter().take(4).enumerate() {
        eprintln!("frame {i}: key {} {} bytes, NAL types {:?}", f.key, f.data.len(), nal_types(&f.data));
    }
    let path = dir.join("plain.h265");
    std::fs::write(&path, frames_of(&done.frames)).unwrap();
    let (err, n) = decode(&path, &dir.join("plain.yuv"));
    eprintln!("decoded {n} frames; errors: {err:?}");
    eprintln!("{}", psnr_raw(&srcp, &dir.join("plain.yuv")));
    let k = key_indices(&done.frames);
    if !keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(err.trim().is_empty() && n == 120, "the stream doesn't decode cleanly");
    assert_eq!(k, [0], "keyframes nobody asked for");
}

/// 1080p: the rate drops from 20 to 5 Mbit/s at picture 60, a keyframe is asked for at 100.
fn events(which: Which) {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some((gpu, dir, keep)) = setup() else { return eprintln!("no GPU: skipped") };
    let done = match run(&gpu, which, size(), 140, 20_000_000, |f| f == 100, |f| (f == 60).then_some(5_000_000), None) {
        Ok(d) => d,
        Err(e) => return missing(&e),
    };
    let f = &done.frames;
    eprintln!("{which:?} rate change at 60: bytes/frame 30..60 {} (20 Mbit/s = {}), 70..100 {} (5 Mbit/s = {}), 110..140 {}", average(f, 30, 60), target(20_000_000), average(f, 70, 100), target(5_000_000), average(f, 110, 140));
    let keys = key_indices(f);
    eprintln!("keyframes at {keys:?}; frame 100: key {} {} bytes, first NAL types {:?}", f[100].key, f[100].data.len(), &nal_types(&f[100].data)[..3.min(nal_types(&f[100].data).len())]);
    latency("events", f);
    let path = dir.join("events.h265");
    std::fs::write(&path, frames_of(f)).unwrap();
    let (err, n) = decode(&path, &dir.join("events.yuv"));
    eprintln!("decoded {n} frames; errors: {err:?}");
    // A decoder that joins at the asked-for keyframe.
    let tail: Vec<u8> = f[100..].iter().flat_map(|f| f.data.iter().copied()).collect();
    let tailp = dir.join("tail.h265");
    std::fs::write(&tailp, tail).unwrap();
    let (err2, n2) = decode(&tailp, &dir.join("tail.yuv"));
    eprintln!("a decoder joining at the keyframe: {n2} frames; errors: {err2:?}");
    if !keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert_eq!(keys, [0, 100]);
    assert!(f[100].key);
    assert!(err.trim().is_empty() && n == 140);
    assert!(err2.trim().is_empty() && n2 == 40);
    let (before, after) = (average(f, 30, 60) as f64, average(f, 70, 100) as f64);
    assert!(after < before * 0.5, "the rate change did nothing: {before} -> {after}");
}

/// Thousands of pictures at a low resolution with one keyframe in the middle (past the point a
/// short picture order count wraps, 2048, when the count has 11 bits): no B-frames, no
/// reordering, every picture's count the next one, no picture naming more earlier pictures than
/// the usual one; the whole stream decodes, and a fresh decoder joining at the second keyframe
/// does too (docs/PROTOCOL.md, "A stream every decoder can follow").
fn long(which: Which) {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some((gpu, dir, keep)) = setup() else { return eprintln!("no GPU: skipped") };
    let count: u32 = std::env::var("REMOTEMAN_LONG_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(3200);
    let second = std::env::var("REMOTEMAN_LONG_KEY").ok().and_then(|v| v.parse().ok()).unwrap_or(count * 4 / 5);
    let (w, h) = std::env::var("REMOTEMAN_LONG_SIZE").ok().and_then(|v| v.split_once('x').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))).unwrap_or((640u32, 360u32));
    let bitrate: u32 = std::env::var("REMOTEMAN_TEST_BITRATE").ok().and_then(|v| v.parse().ok()).unwrap_or(3_000_000);
    let t = std::time::Instant::now();
    let done = match run(&gpu, which, (w, h), count, bitrate, |f| f == second, |_| None, None) {
        Ok(d) => d,
        Err(e) => return missing(&e),
    };
    eprintln!("{which:?} long: {count} pictures {w}x{h} in {:.1} s; {}", t.elapsed().as_secs_f64(), done.note);
    let total: usize = done.frames.iter().map(|f| f.data.len()).sum();
    eprintln!("average {} bytes/frame (target {}); keyframes at {:?}", total / count as usize, target(bitrate), key_indices(&done.frames));
    latency("long", &done.frames);
    let path = dir.join("long.h265");
    std::fs::write(&path, frames_of(&done.frames)).unwrap();
    let n = numbering(&path);
    eprintln!("{n:?}");
    let (err, decoded) = decode_small(&path, &dir.join("long.yuv"), w, h);
    eprintln!("decoded {decoded} of {count} frames; errors: {err:?}");
    let tailp = dir.join("tail.h265");
    std::fs::write(&tailp, frames_of(&done.frames[second as usize..])).unwrap();
    let (err2, decoded2) = decode_small(&tailp, &dir.join("tail.yuv"), w, h);
    eprintln!("a fresh decoder joining at picture {second}: {decoded2} of {} frames; errors: {err2:?}", count - second);
    let keys = key_indices(&done.frames);
    if !keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert_eq!(keys, [0, second as usize], "keyframes nobody asked for");
    assert_eq!(n.pictures, count as usize);
    assert_eq!(n.b_slices, 0, "B slices");
    assert_eq!(n.max_positive, 0, "pictures that follow come before");
    assert!(n.jumps.is_empty(), "picture order counts that don't count up one by one: {:?}", &n.jumps[..n.jumps.len().min(8)]);
    if which == Which::MediaFoundation {
        // (Direct3D 12's encoder keeps up to five pictures and every picture names all of them,
        // always the same way; that decodes with a decoder that keeps just what is named.)
        assert!(n.wide.is_empty(), "pictures that name more than the one earlier picture: {:?}", &n.wide[..n.wide.len().min(8)]);
    } else {
        assert!(n.max_negative <= 5, "pictures that name more than the five it keeps: {:?}", &n.wide[..n.wide.len().min(8)]);
    }
    assert!(err.trim().is_empty() && decoded == count as u64, "the stream doesn't decode cleanly");
    assert!(err2.trim().is_empty() && decoded2 == (count - second) as u64, "a decoder joining at the keyframe fails");
}

/// PSNR of the decoded pictures (raw P010) against the pictures that went in (raw P010): compared
/// as they are, not through a conversion FFmpeg would apply by the stream's colour tags.
fn psnr_raw(src: &Path, dec: &Path) -> String {
    let (w, h) = size();
    let size = format!("{w}x{h}");
    let o = Command::new(ffmpeg()).args(["-hide_banner", "-f", "rawvideo", "-pix_fmt", "p010le", "-video_size", &size, "-i"]).arg(src).args(["-f", "rawvideo", "-pix_fmt", "p010le", "-video_size", &size, "-i"]).arg(dec).args(["-lavfi", "[0:v][1:v]psnr", "-f", "null", "-"]).output().expect("ffmpeg");
    String::from_utf8_lossy(&o.stderr).lines().filter(|l| l.contains("PSNR")).collect::<Vec<_>>().join(" | ")
}

/// Not a check: what Media Foundation lists (for the report of what a GPU offers).
#[test]
fn mf_lists() {
    for n in crate::mf::names() {
        eprintln!("Media Foundation lists: {n}");
    }
}

fn decode_small(path: &Path, raw: &Path, w: u32, h: u32) -> (String, u64) {
    let o = Command::new(ffmpeg()).args(["-v", "error", "-y", "-i"]).arg(path).args(["-f", "rawvideo", "-pix_fmt", "p010le"]).arg(raw).output().expect("ffmpeg");
    let size = std::fs::metadata(raw).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_file(raw);
    (String::from_utf8_lossy(&o.stderr).into_owned(), size / (w as u64 * h as u64 * 3))
}

#[test]
fn mf_stream() {
    stream(Which::MediaFoundation);
}

#[test]
fn mf_events() {
    events(Which::MediaFoundation);
}

#[test]
fn mf_long() {
    long(Which::MediaFoundation);
}

#[test]
fn d3d12_long() {
    long(Which::Direct3D12);
}
