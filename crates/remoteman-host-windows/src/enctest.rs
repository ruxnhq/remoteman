//! Tests of the conversion and the encoder together, on a synthetic picture: no capture, no display
//! driver. They need an NVIDIA GPU with NVENC and say so and pass where there is none.
//!
//! The picture is a PQ (HDR10) R10G10B10A2 surface of colour bars and a grey ramp, which is what the
//! driver's texture holds on an HDR monitor, put through the real converter and encoder. Set
//! `REMOTEMAN_TEST_OUT` to a directory to keep the bitstreams (`hevc420.h265`, `hevc444.h265`,
//! `av1.obu`) for `ffprobe`; without it they are written to the temp directory and removed.

use anyhow::{Context, Result};
use remoteman_core::{Chroma, Codec, Mode};
use remoteman_encode::{How, HdrLight};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11ShaderResourceView};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

use crate::capture::Gpu;
use crate::convert::{Converter, Input, Output};
use crate::encode::{self, Settings};

const W: u32 = 1280;
const H: u32 = 720;
const WHITE: f32 = 203.0;

fn nvidia() -> Option<Gpu> {
    let _ = HMODULE::default();
    // SAFETY: plain DXGI enumeration.
    let luid = unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut i = 0;
        loop {
            let a = factory.EnumAdapters1(i).ok()?;
            let d = a.GetDesc1().ok()?;
            if d.VendorId == 0x10de {
                break (d.AdapterLuid.LowPart, d.AdapterLuid.HighPart);
            }
            i += 1;
        }
    };
    let gpu = Gpu::on_adapter(luid).ok()?;
    encode::library().ok()?;
    Some(gpu)
}

/// PQ code (0..1) for a luminance in nits.
fn pq(nits: f32) -> f32 {
    let y = (nits / 10000.0).powf(0.1593017578125);
    ((0.8359375 + 18.8515625 * y) / (1.0 + 18.6875 * y)).powf(78.84375)
}

/// The test picture's R'G'B' (PQ-coded, BT.2020) at a pixel, and the same for the check afterwards.
pub fn bar(x: u32, y: u32) -> [f32; 3] {
    let col = x * 8 / W;
    if y < H / 2 {
        // Colour bars at 203 nits (SDR white) and 1000 nits.
        let level = if (y / 90) % 2 == 0 { 203.0 } else { 1000.0 };
        let c = [[1.0, 1.0, 1.0], [1.0, 1.0, 0.0], [0.0, 1.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, 0.0]][col as usize];
        [pq(c[0] * level), pq(c[1] * level), pq(c[2] * level)]
    } else {
        // A ramp from black to 1000 nits (linear in light), grey.
        let v = pq(1000.0 * (x as f32 / (W - 1) as f32));
        [v, v, v]
    }
}

fn source(gpu: &Gpu) -> Result<ID3D11ShaderResourceView> {
    let mut data = vec![0u32; (W * H) as usize];
    for y in 0..H {
        for x in 0..W {
            let [r, g, b] = bar(x, y).map(|v| (v * 1023.0 + 0.5) as u32);
            data[(y * W + x) as usize] = r | g << 10 | b << 20 | 3 << 30;
        }
    }
    let desc = D3D11_TEXTURE2D_DESC { Width: W, Height: H, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_R10G10B10A2_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, ..Default::default() };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: data.as_ptr() as *const _, SysMemPitch: W * 4, SysMemSlicePitch: 0 };
    // SAFETY: the pixels live through the call.
    unsafe {
        let mut texture = None;
        gpu.device.CreateTexture2D(&desc, Some(&init), Some(&mut texture))?;
        let mut view = None;
        gpu.device.CreateShaderResourceView(&texture.context("no texture")?, None, Some(&mut view))?;
        view.context("no view")
    }
}

/// How many mastering display and content light level messages a stream has: HEVC's prefix SEI
/// messages of payload types 137 and 144, AV1's metadata OBUs of types 2 and 1.
fn count_light(stream: &[u8], av1: bool) -> (u32, u32) {
    let (mut mastering, mut light) = (0, 0);
    if av1 {
        let mut i = 0;
        while i < stream.len() {
            let (kind, ext) = ((stream[i] >> 3) & 15, (stream[i] >> 2) & 1);
            let (mut j, mut size, mut shift) = (i + 1 + ext as usize, 0usize, 0);
            loop {
                let byte = stream[j];
                j += 1;
                size |= ((byte & 127) as usize) << shift;
                shift += 7;
                if byte & 128 == 0 {
                    break;
                }
            }
            if kind == 5 {
                match stream[j] {
                    2 => mastering += 1,
                    1 => light += 1,
                    _ => {}
                }
            }
            i = j + size;
        }
    } else {
        // Annex B: start codes 00 00 01; a prefix SEI NAL (type 39) holds payload type, size, payload.
        let mut i = 0;
        while i + 6 < stream.len() {
            if stream[i..i + 3] == [0, 0, 1] && (stream[i + 3] >> 1) & 63 == 39 {
                match stream[i + 5] {
                    137 => mastering += 1,
                    144 => light += 1,
                    _ => {}
                }
                i += 5;
            }
            i += 1;
        }
    }
    (mastering, light)
}

/// Encode six frames (a keyframe, four more, another keyframe) in `mode`, as HDR with `light`,
/// into `path`; returns the number of bytes.
fn run(gpu: &Gpu, mode: Mode, hdr: bool, light: Option<HdrLight>, path: &std::path::Path) -> Result<usize> {
    let view = source(gpu)?;
    let output = if hdr { Output::Pq { sdr_white: WHITE } } else { Output::Sdr };
    let converter = Converter::new(gpu, W, H, Input::Pq { white: WHITE }, output, mode.chroma == Chroma::C444)?;
    let mut encoder = encode::open(gpu, &converter.output, &Settings { mode, width: W, height: H, fps: 60, bitrate: 20_000_000, hdr, light })?;
    let mut out = Vec::new();
    for n in 0..6u64 {
        converter.convert(gpu, &view);
        let made = encoder.encode_how(0, n, How { idr: n == 5, ..Default::default() })?;
        anyhow::ensure!(made.key == (n == 0 || n == 5), "frame {n}: keyframe {}", made.key);
        out.extend_from_slice(made.data);
    }
    std::fs::write(path, &out)?;
    // Two keyframes, so two of each message; none for an HDR stream told nothing, or an SDR one.
    let expected = if hdr && light.is_some() { (2, 2) } else { (0, 0) };
    anyhow::ensure!(count_light(&out, mode.codec == Codec::Av1) == expected, "mastering display and light level messages: {:?}, expected {expected:?}", count_light(&out, mode.codec == Codec::Av1));
    Ok(out.len())
}

fn out_dir() -> (std::path::PathBuf, bool) {
    match std::env::var_os("REMOTEMAN_TEST_OUT") {
        Some(d) => (d.into(), true),
        None => (std::env::temp_dir().join(format!("remoteman-enctest-{}", std::process::id())), false),
    }
}

/// HEVC 4:2:0, HEVC 4:4:4 and AV1, all BT.2020 PQ with the light metadata.
#[test]
fn hdr_streams() {
    let Some(gpu) = nvidia() else {
        eprintln!("no NVIDIA GPU with NVENC here: skipped");
        return;
    };
    let (dir, keep) = out_dir();
    std::fs::create_dir_all(&dir).unwrap();
    // The values of a monitor with a 1000-nit peak, as the host would take them.
    let light = HdrLight { peak: 1000.0, min: 0.0001, max_cll: 1000, max_fall: 203 };
    let hevc = |chroma| Mode { codec: Codec::Hevc, chroma, depth: 10 };
    let cases = [("hevc420.h265", hevc(Chroma::C420)), ("hevc444.h265", hevc(Chroma::C444)), ("av1.obu", Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 })];
    for (name, mode) in cases {
        let n = run(&gpu, mode, true, Some(light), &dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        eprintln!("{name}: {n} bytes");
        assert!(n > 1000);
    }
    // Without the light (or in SDR) there are no such messages.
    run(&gpu, hevc(Chroma::C420), true, None, &dir.join("none.h265")).unwrap();
    run(&gpu, hevc(Chroma::C444), false, None, &dir.join("sdr444.h265")).unwrap();
    if !keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
}
