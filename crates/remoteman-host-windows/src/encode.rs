//! Encode: the picture's P010 texture into HEVC or AV1. The rest of the host knows only
//! [`open`] and the `FrameEncoder` it returns; which vendor's encoder is behind it is decided
//! here.
//!
//! On an NVIDIA card that is NVENC (`remoteman-encode`), which keeps long-term frames and can
//! be told to stop referring to a lost one, so a loss is answered without a keyframe. On any
//! other maker's card it is the hardware encoder that maker's driver registers with Media
//! Foundation (`mf.rs`), which can do neither: it answers `invalidate` with an error and
//! reports no long-term frames, and a loss then costs a keyframe. `--encoder nvenc|mf` asks
//! for one of them whatever the card.
//!
//! SEAM (not built): AMD's AMF and Intel's own interface, or Direct3D 12 Video Encode for
//! both, where the application keeps the reference frames itself and loss recovery without a
//! keyframe comes back. Each becomes one more `FrameEncoder` tried in [`open`].

use std::path::PathBuf;

use anyhow::{Context, Result};
use remoteman_core::{Chroma, Codec, Mode};
use remoteman_encode::{Config, Encoder, FrameEncoder, How};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::System::LibraryLoader::{LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW, LoadLibraryW};
use windows::core::{HSTRING, Interface, w};

use crate::capture::Gpu;

/// What this host encodes. HEVC 4:4:4 only through NVENC, on a card whose NVENC has it (the
/// encoder says when it is opened; where it doesn't, the next mode the client offered is
/// taken), in SDR or HDR.
pub const MODES: [Mode; 3] = [
    Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 },
    Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 },
    Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 },
];

pub struct Settings {
    pub mode: Mode,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    /// BT.2020 PQ rather than BT.709 (only where [`can_hdr`] said so).
    pub hdr: bool,
    /// What an HDR stream says of the picture's light (mastering display, MaxCLL, MaxFALL).
    pub light: Option<remoteman_encode::HdrLight>,
}

/// Whether the encoder this GPU gets can say BT.2020 PQ in its stream: NVENC's can; Media
/// Foundation's encoders are not told (not built), so their pictures stay SDR.
pub fn can_hdr(gpu: &Gpu) -> bool {
    match WHICH.load(std::sync::atomic::Ordering::Relaxed) {
        NVENC | D3D12 => true,
        ANY_MAKERS => false,
        _ => gpu.vendor == NVIDIA,
    }
}

/// What an HDR stream says about its light, from what the host knows of the monitor it
/// captures (the frame channel's metadata, which the driver fills from the monitor's EDID):
/// - mastering display: BT.2020 primaries and D65 white (the picture is converted to them,
///   whatever the monitor's own gamut); peak luminance the monitor's, or PQ's usual 1000 nits
///   if the driver gives none; black level the monitor's, or 0.0001 nits;
/// - MaxCLL: that peak (a desktop can show anything up to it; the real figure isn't measured);
/// - MaxFALL: the monitor's if it gives one, else SDR white (a screen full of SDR white is what
///   a desktop's brightest frame averages to), never above the peak.
pub fn hdr_light(meta: &crate::vdd::Metadata, sdr_white: f32) -> remoteman_encode::HdrLight {
    let peak = if meta.max_nits >= 50.0 { meta.max_nits.min(10000.0) } else { 1000.0 };
    let min = if meta.min_nits > 0.0 && meta.min_nits < 1.0 { meta.min_nits } else { 0.0001 };
    let fall = if meta.max_fall >= 1.0 { meta.max_fall } else { sdr_white };
    remoteman_encode::HdrLight { peak, min, max_cll: peak.round() as u32, max_fall: fall.min(peak).round() as u32 }
}

/// An encoder for pictures drawn into `input`, a texture of `gpu`'s device (P010, or for 4:4:4 10-bit
/// R'G'B' that NVENC converts with the matrix the stream says: BT.709, or BT.2020 for HDR). The input's index for `encode_how` is 0.
pub fn open(gpu: &Gpu, input: &ID3D11Texture2D, settings: &Settings) -> Result<Box<dyn FrameEncoder>> {
    anyhow::ensure!(MODES.contains(&settings.mode), "this host doesn't encode {:?}", settings.mode);
    if WHICH.load(std::sync::atomic::Ordering::Relaxed) == D3D12 {
        // AV1 isn't this encoder's (it makes HEVC 4:2:0 alone), but the card may well have one:
        // Intel's Arc cards encode AV1, and their driver offers it to Media Foundation. Without
        // this the pipeline went down to HEVC, with a client that asked for AV1 first.
        if settings.mode.codec == Codec::Av1 && !settings.hdr {
            tracing::info!("Direct3D 12 encoding here is HEVC only; AV1 from Media Foundation's encoder");
            return crate::mf::Encoder::open(gpu, input, settings)
                .map(|e| Box::new(e) as Box<dyn FrameEncoder>)
                .with_context(|| format!("Media Foundation's AV1 encoder on {}", gpu.adapter));
        }
        let tried = d3d12_tried(gpu, input, settings).with_context(|| format!("Direct3D 12 Video Encode on {}", gpu.adapter));
        return match tried {
            Ok(e) => {
                tracing::info!(note = e.note(), "encoder");
                Ok(e)
            }
            // Asked for, and no picture from it: Media Foundation's encoder, for what it makes.
            Err(first) if settings.mode.codec == Codec::Hevc && settings.mode.chroma == Chroma::C420 && !settings.hdr => {
                tracing::warn!("{first:#}; Media Foundation's encoder instead");
                crate::mf::Encoder::open(gpu, input, settings)
                    .map(|e| Box::new(e) as Box<dyn FrameEncoder>)
                    .with_context(|| format!("{first:#}; and Media Foundation's encoder on {}", gpu.adapter))
            }
            Err(e) => Err(e),
        };
    }
    if settings.mode.chroma == Chroma::C444 || settings.hdr {
        // Only NVENC: Media Foundation's encoders are given 4:2:0 SDR alone.
        return match WHICH.load(std::sync::atomic::Ordering::Relaxed) {
            ANY_MAKERS => anyhow::bail!("4:4:4 and HDR are NVENC's only"),
            NVENC => nvidia::open(gpu, input, settings),
            _ if gpu.vendor == NVIDIA => nvidia::open(gpu, input, settings),
            _ => anyhow::bail!("4:4:4 and HDR are NVENC's only, and this isn't an NVIDIA card"),
        }
        .with_context(|| format!("NVENC on {}", gpu.adapter));
    }
    let any = || crate::mf::Encoder::open(gpu, input, settings).map(|e| Box::new(e) as Box<dyn FrameEncoder>).with_context(|| format!("Media Foundation's encoder on {}", gpu.adapter));
    let nvenc = || nvidia::open(gpu, input, settings).with_context(|| format!("NVENC on {}", gpu.adapter));
    let d3d12 = || d3d12_tried(gpu, input, settings).with_context(|| format!("Direct3D 12 Video Encode on {}", gpu.adapter));
    match WHICH.load(std::sync::atomic::Ordering::Relaxed) {
        NVENC => nvenc(),
        ANY_MAKERS => any(),
        // The card's own best first; on an NVIDIA card whose NVENC won't open, the other.
        _ if gpu.vendor == NVIDIA => nvenc().or_else(|first| any().map_err(|second| anyhow::anyhow!("{first:#}; and {second:#}"))),
        // AMD's: Direct3D 12 Video Encode, which on a Radeon (the Ryzen 9 9950X3D's, RDNA 2) passed
        // every test of `d3d12test.rs` and a 3200-picture run, with loss recovery without a
        // keyframe and live rate changes that Media Foundation's AMD encoder lacks (it overshot
        // the rate by 40% on busy pictures). HEVC 4:2:0 SDR only: for AV1, or when it won't open,
        // Media Foundation's.
        _ if gpu.vendor == AMD => d3d12().or_else(|first| any().map_err(|second| anyhow::anyhow!("{first:#}; and {second:#}"))),
        _ => any(),
    }
}

/// Direct3D 12 Video Encode, given the stream only once it has made a picture: a driver can take
/// every argument and still give nothing back (Intel's, for an Arc card in a guest, wrote 0 bytes
/// a frame), and the caller then has another encoder. The trial encoder is put away and a fresh
/// one opened, so that the stream starts at its first frame.
fn d3d12_tried(gpu: &Gpu, input: &ID3D11Texture2D, settings: &Settings) -> Result<Box<dyn FrameEncoder>> {
    let mut trial = crate::d3d12enc::Encoder::open(gpu, input, settings)?;
    trial.encode_how(0, 0, How { idr: true, mark: None, refer: 0 }).context("its trial picture")?;
    drop(trial);
    Ok(Box::new(crate::d3d12enc::Encoder::open(gpu, input, settings)?))
}

/// NVIDIA's PCI id.
const NVIDIA: u32 = 0x10de;
/// AMD's.
const AMD: u32 = 0x1002;
pub const AUTO: u8 = 0;
pub const NVENC: u8 = 1;
pub const ANY_MAKERS: u8 = 2;
/// Direct3D 12 Video Encode (`d3d12enc.rs`): any maker's GPU, with loss recovery and live
/// rate changes. Asked for with `--encoder d3d12`; the default on AMD cards (HEVC, SDR).
pub const D3D12: u8 = 3;
/// Which encoder `--encoder` asked for; the card's own by default.
pub static WHICH: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(AUTO);

/// Where NVIDIA's encoder library was found, for `--probe`.
pub fn library() -> Result<String> {
    nvidia::load()
}

/// NVENC: NVIDIA's encoder, through `remoteman-encode`.
mod nvidia {
    use super::*;

    const LIBRARY: &str = "nvEncodeAPI64.dll";

    /// Make sure NVIDIA's encoder library is loaded into this process, and say from where.
    /// It is usually in System32. In a guest that borrows its GPU from the host (GPU-PV) the
    /// driver's files are only in the host's driver store, where the loader doesn't look, so
    /// it is looked for there; once it is loaded, the encoder wrapper finds it by name.
    pub fn load() -> Result<String> {
        // SAFETY: loading a library by name, and then by full path.
        unsafe {
            if LoadLibraryW(w!("nvEncodeAPI64.dll")).is_ok() {
                return Ok(format!("{LIBRARY} (the usual search path)"));
            }
            let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32");
            let mut looked = Vec::new();
            for store in ["DriverStore\\FileRepository", "HostDriverStore\\FileRepository"] {
                let store = system.join(store);
                looked.push(store.display().to_string());
                let Ok(packages) = std::fs::read_dir(&store) else { continue };
                // NVIDIA's display driver packages: nv_dispi.inf_..., nvlt.inf_..., and so on.
                let mut found: Vec<PathBuf> = packages
                    .flatten()
                    .filter(|p| p.file_name().to_string_lossy().to_lowercase().starts_with("nv"))
                    .map(|p| p.path().join(LIBRARY))
                    .filter(|p| p.is_file())
                    .collect();
                // The newest package, if a store holds several.
                found.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
                if let Some(path) = found.pop() {
                    LoadLibraryExW(&HSTRING::from(path.as_os_str()), None, LOAD_WITH_ALTERED_SEARCH_PATH).map(|_: HMODULE| ()).with_context(|| format!("loading {}", path.display()))?;
                    return Ok(path.display().to_string());
                }
            }
            anyhow::bail!("{LIBRARY} is neither on the search path nor in an NVIDIA driver package under {}", looked.join(" or "))
        }
    }

    pub fn open(gpu: &Gpu, input: &ID3D11Texture2D, s: &Settings) -> Result<Box<dyn FrameEncoder>> {
        let from = load()?;
        // The Linux host's settings: the fastest preset, two long-term frames, and the usual
        // number of recent ones for the picture's size.
        let config = Config { codec: if s.mode.codec == Codec::Av1 { remoteman_encode::Codec::Av1 } else { remoteman_encode::Codec::Hevc }, chroma444: s.mode.chroma == Chroma::C444, width: s.width, height: s.height, fps: s.fps, bitrate: s.bitrate, preset: 1, long_term: 2, short_term: None, slices: if s.mode.codec == Codec::Hevc && s.mode.chroma == Chroma::C444 { hevc444_slices() } else { 0 } };
        // SAFETY: a live device with multithread protection on (`Gpu` sees to that) and a live
        // texture of that device; the pipeline keeps both for longer than the encoder.
        let mut encoder = unsafe { Encoder::new_d3d11_light(gpu.device.as_raw(), &config, s.hdr, s.light) }?;
        let index = unsafe { encoder.add_texture(input.as_raw()) }.context("registering the picture's texture")?;
        anyhow::ensure!(index == 0, "the picture's texture became input {index}");
        tracing::debug!(library = %from, "NVENC");
        Ok(Box::new(encoder))
    }
}

/// How many slices NVENC cuts an HEVC 4:4:4 picture into: one, by default. More do not help the
/// viewer's CPU decode: measured with FFmpeg 9.0, its HEVC decoder only threads frames (and
/// wavefront rows, which NVENC cannot write), so 8 slices decode as slowly as one and cost 0.2 to
/// 0.4% more bytes. `REMOTEMAN_HEVC444_SLICES` sets a count to try.
fn hevc444_slices() -> u32 {
    std::env::var("REMOTEMAN_HEVC444_SLICES").ok().and_then(|v| v.parse().ok()).unwrap_or(0)
}
