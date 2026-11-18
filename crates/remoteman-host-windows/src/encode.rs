//! Encode: the picture's P010 texture into HEVC or AV1. The rest of the host knows only
//! [`open`] and the `FrameEncoder` it returns; which vendor's encoder is behind it is decided
//! here.
//!
//! SEAM (other encoders, not built): AMD's AMF, Intel's QSV and Direct3D 12 Video Encode each
//! become one more `FrameEncoder` tried in [`open`]. The long-term-reference and invalidation
//! calls are the contract with loss recovery; an encoder without them answers `invalidate`
//! with an error and reports no long-term frames, and losses then cost keyframes.

use std::path::PathBuf;

use anyhow::{Context, Result};
use remoteman_core::{Chroma, Codec, Mode};
use remoteman_encode::{Config, Encoder, FrameEncoder};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::System::LibraryLoader::{LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW, LoadLibraryW};
use windows::core::{HSTRING, Interface, w};

use crate::capture::Gpu;

/// What this host encodes, on any GPU it has an encoder for. (No 4:4:4: Direct3D has no planar
/// 10-bit 4:4:4 texture to convert into.)
pub const MODES: [Mode; 2] = [Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 }, Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 }];

pub struct Settings {
    pub mode: Mode,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
}

/// An encoder for pictures drawn into `input`, a P010 texture of `gpu`'s device. The input's
/// index for `encode_how` is 0.
pub fn open(gpu: &Gpu, input: &ID3D11Texture2D, settings: &Settings) -> Result<Box<dyn FrameEncoder>> {
    anyhow::ensure!(MODES.contains(&settings.mode), "this host doesn't encode {:?}", settings.mode);
    // NVIDIA's is the only one so far, so it is tried whatever the adapter calls itself, and
    // its own error is the answer on other vendors' GPUs.
    nvidia::open(gpu, input, settings).with_context(|| format!("NVENC on {}", gpu.adapter))
}

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
        let config = Config { codec: if s.mode.codec == Codec::Av1 { remoteman_encode::Codec::Av1 } else { remoteman_encode::Codec::Hevc }, chroma444: false, width: s.width, height: s.height, fps: s.fps, bitrate: s.bitrate, preset: 1, long_term: 2, short_term: None };
        // SAFETY: a live device with multithread protection on (`Gpu` sees to that) and a live
        // P010 texture of that device; the pipeline keeps both for longer than the encoder.
        let mut encoder = unsafe { Encoder::new_d3d11(gpu.device.as_raw(), &config) }?;
        let index = unsafe { encoder.add_texture(input.as_raw()) }.context("registering the picture's texture")?;
        anyhow::ensure!(index == 0, "the picture's texture became input {index}");
        tracing::debug!(library = %from, "NVENC");
        Ok(Box::new(encoder))
    }
}
