//! `--probe-av1`: what each GPU's encoders say about AV1, for deciding whether a Direct3D 12 AV1
//! encoder is worth writing for a card (issue #3: a Radeon 780M in a GPU-PV guest, where AMD's
//! Media Foundation AV1 encoder won't be made). It only asks: no display driver, no screens, no
//! frame encoded. For every hardware adapter, Direct3D 12 Video Encode's answers for AV1 (and
//! HEVC beside them, which is known to work, as a check of the questions); then every
//! hardware encoder Media Foundation lists for AV1 and HEVC is made and let go of again.

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};
use windows::Win32::Media::MediaFoundation::*;
use windows::core::Interface;

/// Said both on the console and in the log.
macro_rules! say {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        println!("{line}");
        tracing::info!("probe-av1: {line}");
    }};
}

pub fn run() -> Result<()> {
    // SAFETY: DXGI calls as documented; the adapters' descriptions are plain data.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.context("DXGI")?;
    for i in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        let name = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_owned();
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        say!("adapter {i}: {name} (vendor {:04x}, device {:04x}, LUID {:08x}:{:08x})", desc.VendorId, desc.DeviceId, desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart);
        let mut device: Option<ID3D12Device> = None;
        if let Err(e) = unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) } {
            say!("  no Direct3D 12 device: {e}");
            continue;
        }
        let Some(video) = device.and_then(|d| d.cast::<ID3D12VideoDevice3>().ok()) else {
            say!("  no Direct3D 12 video device (ID3D12VideoDevice3): this adapter has no Direct3D 12 Video Encode");
            continue;
        };
        hevc(&video);
        av1(&video);
    }
    media_foundation();
    say!("probe-av1 done");
    Ok(())
}

fn check<T>(video: &ID3D12VideoDevice3, feature: D3D12_FEATURE_VIDEO, data: &mut T) -> std::result::Result<(), String> {
    // SAFETY: `data` is the feature's structure and its size is passed with it.
    unsafe { video.CheckFeatureSupport(feature, data as *mut T as *mut _, std::mem::size_of::<T>() as u32) }.map_err(|e| format!("{e}"))
}

/// What the question came back with, in words.
fn answer(said: std::result::Result<(), String>, supported: bool) -> String {
    match said {
        Ok(()) if supported => "yes".into(),
        Ok(()) => "no".into(),
        Err(e) => format!("the question was refused: {e}"),
    }
}

/// HEVC, as a check that the questions are asked right on this adapter.
fn hevc(video: &ID3D12VideoDevice3) {
    let mut codec = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC { Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC, ..Default::default() };
    let said = check(video, D3D12_FEATURE_VIDEO_ENCODER_CODEC, &mut codec);
    say!("  Direct3D 12 HEVC encode: {}", answer(said, codec.IsSupported.as_bool()));
}

fn av1(video: &ID3D12VideoDevice3) {
    let codec_id = D3D12_VIDEO_ENCODER_CODEC_AV1;
    let mut codec = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC { Codec: codec_id, ..Default::default() };
    let said = check(video, D3D12_FEATURE_VIDEO_ENCODER_CODEC, &mut codec);
    say!("  Direct3D 12 AV1 encode: {}", answer(said.clone(), codec.IsSupported.as_bool()));
    if said.is_err() || !codec.IsSupported.as_bool() {
        return;
    }

    let mut profile = D3D12_VIDEO_ENCODER_AV1_PROFILE_MAIN;
    let profile_ptr: *mut D3D12_VIDEO_ENCODER_AV1_PROFILE = &mut profile;
    let profile_desc = || D3D12_VIDEO_ENCODER_PROFILE_DESC { DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_PROFILE>() as u32, Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 { pAV1Profile: profile_ptr } };

    // Profile and levels.
    let (mut min_l, mut max_l) = (D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS::default(), D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS::default());
    let level_size = std::mem::size_of::<D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS>() as u32;
    let mut pl = D3D12_FEATURE_DATA_VIDEO_ENCODER_PROFILE_LEVEL {
        Codec: codec_id,
        Profile: profile_desc(),
        MinSupportedLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING { DataSize: level_size, Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 { pAV1LevelSetting: &mut min_l } },
        MaxSupportedLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING { DataSize: level_size, Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 { pAV1LevelSetting: &mut max_l } },
        ..Default::default()
    };
    let said = check(video, D3D12_FEATURE_VIDEO_ENCODER_PROFILE_LEVEL, &mut pl);
    let level = |l: &D3D12_VIDEO_ENCODER_AV1_LEVEL_TIER_CONSTRAINTS| format!("{}.{} ({} tier)", 2 + l.Level.0 / 4, l.Level.0 % 4, if l.Tier.0 == 0 { "main" } else { "high" });
    let levels = if said.is_ok() && pl.IsSupported.as_bool() { format!(", levels {} to {}", level(&min_l), level(&max_l)) } else { String::new() };
    say!("  AV1 Main profile: {}{levels}", answer(said, pl.IsSupported.as_bool()));

    // What it takes in.
    for (format, name) in [(DXGI_FORMAT_P010, "P010 (10-bit, what the host converts to)"), (DXGI_FORMAT_NV12, "NV12 (8-bit)")] {
        let mut f = D3D12_FEATURE_DATA_VIDEO_ENCODER_INPUT_FORMAT { Codec: codec_id, Profile: profile_desc(), Format: DXGI_FORMAT(format.0), ..Default::default() };
        let said = check(video, D3D12_FEATURE_VIDEO_ENCODER_INPUT_FORMAT, &mut f);
        say!("  AV1 input {name}: {}", answer(said, f.IsSupported.as_bool()));
    }

    // Sizes.
    let mut r = D3D12_FEATURE_DATA_VIDEO_ENCODER_OUTPUT_RESOLUTION { Codec: codec_id, ..Default::default() };
    let said = check(video, D3D12_FEATURE_VIDEO_ENCODER_OUTPUT_RESOLUTION, &mut r);
    if said.is_ok() && r.IsSupported.as_bool() {
        say!(
            "  AV1 sizes: {}x{} to {}x{}, width a multiple of {}, height of {}",
            r.MinResolutionSupported.Width,
            r.MinResolutionSupported.Height,
            r.MaxResolutionSupported.Width,
            r.MaxResolutionSupported.Height,
            r.ResolutionWidthMultipleRequirement,
            r.ResolutionHeightMultipleRequirement
        );
    } else {
        say!("  AV1 sizes: {}", answer(said, r.IsSupported.as_bool()));
    }

    // Constant rate, which the host uses.
    let mut rc = D3D12_FEATURE_DATA_VIDEO_ENCODER_RATE_CONTROL_MODE { Codec: codec_id, RateControlMode: D3D12_VIDEO_ENCODER_RATE_CONTROL_MODE_CBR, ..Default::default() };
    let said = check(video, D3D12_FEATURE_VIDEO_ENCODER_RATE_CONTROL_MODE, &mut rc);
    say!("  AV1 constant rate: {}", answer(said, rc.IsSupported.as_bool()));

    // The coding tools, and which values the application must read back after each frame to
    // write the frame header (the host would write the sequence and frame headers itself).
    let mut sup = D3D12_VIDEO_ENCODER_AV1_CODEC_CONFIGURATION_SUPPORT::default();
    let mut cs = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT {
        Codec: codec_id,
        Profile: profile_desc(),
        CodecSupportLimits: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT { DataSize: std::mem::size_of_val(&sup) as u32, Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_0 { pAV1Support: &mut sup } },
        ..Default::default()
    };
    let said = check(video, D3D12_FEATURE_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT, &mut cs);
    if said.is_ok() && cs.IsSupported.as_bool() {
        say!(
            "  AV1 configuration: features supported {:#x}, required {:#x}, interpolation filters {:#x}, tx modes {:?}, segmentation modes {:#x}, values read back after encoding {:#x}, temporal layers {}, spatial layers {}",
            sup.SupportedFeatureFlags.0,
            sup.RequiredFeatureFlags.0,
            sup.SupportedInterpolationFilters.0,
            sup.SupportedTxModes.iter().map(|m| m.0).collect::<Vec<_>>(),
            sup.SupportedSegmentationModes.0,
            sup.PostEncodeValuesFlags.0,
            sup.MaxTemporalLayers,
            sup.MaxSpatialLayers
        );
    } else {
        say!("  AV1 configuration: {}", answer(said, cs.IsSupported.as_bool()));
    }

    // References: how many a frame may use, and which frame types (recovery from a loss needs
    // inter frames that name an older reference).
    let mut pc = D3D12_VIDEO_ENCODER_CODEC_AV1_PICTURE_CONTROL_SUPPORT::default();
    let mut ps = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT {
        Codec: codec_id,
        Profile: profile_desc(),
        PictureSupport: D3D12_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT { DataSize: std::mem::size_of_val(&pc) as u32, Anonymous: D3D12_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT_0 { pAV1Support: &mut pc } },
        ..Default::default()
    };
    let said = check(video, D3D12_FEATURE_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT, &mut ps);
    if said.is_ok() && ps.IsSupported.as_bool() {
        say!(
            "  AV1 pictures: up to {} references a frame, frame types {:#x} (1 key, 2 inter, 4 intra-only, 8 switch), prediction {} reference(s)",
            pc.MaxUniqueReferencesPerFrame,
            pc.SupportedFrameTypes.0,
            if pc.PredictionMode.0 == 0 { "single" } else { "compound" }
        );
    } else {
        say!("  AV1 pictures: {}", answer(said, ps.IsSupported.as_bool()));
    }
}

/// Every hardware encoder Media Foundation lists for AV1 and HEVC, made and let go of.
fn media_foundation() {
    for (codec, line) in crate::mf::try_making() {
        say!("Media Foundation {codec:?}: {line}");
    }
}
