//! The one Direct3D 11 device: the decoder writes pictures with it and the window draws them
//! with it, so a picture never has to leave it.

use anyhow::{Context, Result, bail};
use remoteman_core::{Chroma, Codec, Mode};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_P010;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{GUID, Interface};

#[derive(Clone)]
pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    /// The adapter's name, for the log.
    pub name: String,
}

/// The adapter to decode and draw on: the one Windows calls the high-performance GPU. On a
/// machine whose screen hangs off something else (a laptop's integrated graphics, or a VM's
/// basic display with the real card beside it) the default adapter may not decode at all;
/// Windows carries the finished picture across when presenting.
fn adapter() -> Option<(IDXGIAdapter1, String)> {
    // SAFETY: plain DXGI calls; the factory and adapters are owned here.
    unsafe {
        let factory: IDXGIFactory6 = CreateDXGIFactory1().ok()?;
        for i in 0.. {
            let adapter: IDXGIAdapter1 = factory.EnumAdapterByGpuPreference(i, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE).ok()?;
            let desc = adapter.GetDesc1().ok()?;
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let len = desc.Description.iter().position(|c| *c == 0).unwrap_or(desc.Description.len());
            return Some((adapter, String::from_utf16_lossy(&desc.Description[..len])));
        }
        None
    }
}

impl Gpu {
    pub fn open() -> Result<Self> {
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        // Video support for the decoder, BGRA for whatever draws an interface on it later.
        let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        let (mut device, mut context) = (None, None);
        let chosen = adapter();
        // SAFETY: the out pointers are valid for the call.
        unsafe {
            match &chosen {
                Some((adapter, _)) => D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, HMODULE::default(), flags, Some(&levels), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context)),
                None => D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), flags, Some(&levels), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context)),
            }
            .context("create the Direct3D 11 device")?;
        }
        let (device, context): (ID3D11Device, ID3D11DeviceContext) = (device.context("no device")?, context.context("no device context")?);
        // The decode thread and the draw thread both use the immediate context.
        // SAFETY: a call on an interface we hold.
        unsafe {
            let _ = context.cast::<ID3D11Multithread>().context("no ID3D11Multithread")?.SetMultithreadProtected(true);
        }
        Ok(Self { device, context, name: chosen.map_or("the default adapter".into(), |(_, name)| name) })
    }

    /// Whether this card really decodes `mode`: FFmpeg has the decoder with a D3D11VA path, the
    /// driver lists the profile with 10-bit output, and a decoder of that profile opens. (Asking
    /// FFmpeg alone says nothing: it only goes to the card at the first frame.)
    pub fn decodes(&self, mode: Mode) -> Result<()> {
        if mode.chroma != Chroma::C420 || mode.depth != 10 {
            bail!("only 10-bit 4:2:0 is drawn so far");
        }
        crate::decode::has_d3d11va(mode.codec)?;
        let profile = match mode.codec {
            Codec::Av1 => AV1_PROFILE0,
            Codec::Hevc => HEVC_MAIN10,
        };
        // SAFETY: calls on the video device with structures that live through each call.
        unsafe {
            let video: ID3D11VideoDevice = self.device.cast().context("the device has no video interface")?;
            let listed = (0..video.GetVideoDecoderProfileCount()).any(|i| video.GetVideoDecoderProfile(i).is_ok_and(|g| g == profile));
            if !listed {
                bail!("the driver doesn't list the profile");
            }
            if !video.CheckVideoDecoderFormat(&profile, DXGI_FORMAT_P010).is_ok_and(|ok| ok.as_bool()) {
                bail!("the driver doesn't decode it to P010");
            }
            let desc = D3D11_VIDEO_DECODER_DESC { Guid: profile, SampleWidth: 1920, SampleHeight: 1080, OutputFormat: DXGI_FORMAT_P010 };
            let count = video.GetVideoDecoderConfigCount(&desc).context("no decoder configurations")?;
            if count == 0 {
                bail!("the driver has no configuration for it");
            }
            let mut config = D3D11_VIDEO_DECODER_CONFIG::default();
            video.GetVideoDecoderConfig(&desc, 0, &mut config).context("read the decoder configuration")?;
            video.CreateVideoDecoder(&desc, &config).context("open a decoder")?;
        }
        Ok(())
    }
}

// The decoder profiles, from dxva.h.
const HEVC_MAIN10: GUID = GUID::from_u128(0x107af0e0_ef1a_4d19_aba8_67a163073d13);
const AV1_PROFILE0: GUID = GUID::from_u128(0xb8be4ccb_cf53_46ba_8d59_d6b8a6da5d2a);
