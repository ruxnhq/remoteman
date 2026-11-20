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
        // Video support for the decoder, BGRA for whatever draws an interface on it later.
        Self::with(D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT)
    }

    /// A device on the same card that only presents (full screen's, `draw.rs`): a swap chain on
    /// the device that decodes, or that presents into another swap chain, was measured not to
    /// make a monitor vary its refresh.
    pub fn presenter() -> Result<Self> {
        Self::with(D3D11_CREATE_DEVICE_BGRA_SUPPORT)
    }

    fn with(flags: D3D11_CREATE_DEVICE_FLAG) -> Result<Self> {
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
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
        if mode.codec == Codec::Hevc && mode.chroma == Chroma::C444 && mode.depth == 10 {
            // The drawing takes Y410 and AYUV (draw.rs; tried with `REMOTEMAN_Y410=1`, which packs the
            // CPU's pictures that way), but whatever the card lists (an RTX 5090: HEVC Main 4:4:4 10
            // into Y410), FFmpeg's HEVC
            // decoder offers no D3D11VA picture format for 4:4:4 (FFmpeg 9.0: it chooses
            // yuv444p10le on the CPU), so it is decoded on the CPU (decode.rs).
            // SAFETY: calls on the video device.
            let listed = unsafe {
                let video: ID3D11VideoDevice = self.device.cast().context("the device has no video interface")?;
                (0..video.GetVideoDecoderProfileCount()).any(|i| video.GetVideoDecoderProfile(i).is_ok_and(|g| g == HEVC_MAIN444_10))
            };
            bail!("{}, but FFmpeg's HEVC decoder has no D3D11VA path for 4:4:4", if listed { "the driver lists HEVC Main 4:4:4 10" } else { "the driver doesn't list HEVC Main 4:4:4 10" });
        }
        if mode.chroma != Chroma::C420 || mode.depth != 10 {
            bail!("only 10-bit 4:2:0 and HEVC 4:4:4 are drawn so far");
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

/// Windows' white for SDR content on an HDR monitor when it can't be asked: where it puts it
/// until the person moves the "SDR content brightness" slider.
pub const USUAL_SDR_WHITE: f32 = 200.0;

/// How the screen showing a window shows light: whether it is in HDR mode, where Windows puts
/// SDR white on it (the "SDR content brightness" slider), and the brightest it says it goes,
/// in nits. An HDR picture is shown so that the computer's SDR white lands on `white` and its
/// brightest within `peak` (`draw.rs`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub hdr: bool,
    pub white: f32,
    pub peak: f32,
}

impl Light {
    /// How many times brighter than its SDR white the screen goes: the protocol's `headroom`
    /// ("how many times brighter than its SDR white the screen can go"). 0 for an SDR screen.
    pub fn headroom(&self) -> f32 {
        if self.hdr { (self.peak / self.white.max(1.0)).max(1.0) } else { 0.0 }
    }
}

/// The screen showing `hwnd`, as [`Light`] says.
pub fn screen_light(hwnd: windows::Win32::Foundation::HWND) -> Light {
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
    let sdr = Light { hdr: false, white: 80.0, peak: 80.0 };
    // SAFETY: DXGI queries with structures that live through each call.
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return sdr };
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                if let Ok(desc) = output.GetDesc() {
                    if desc.Monitor == monitor {
                        if let Ok(desc) = output.cast::<IDXGIOutput6>().and_then(|o6| o6.GetDesc1()) {
                            let hdr = desc.ColorSpace == windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
                            if !hdr {
                                return sdr;
                            }
                            let len = desc.DeviceName.iter().position(|c| *c == 0).unwrap_or(desc.DeviceName.len());
                            let white = sdr_white(&String::from_utf16_lossy(&desc.DeviceName[..len])).unwrap_or(USUAL_SDR_WHITE);
                            return Light { hdr, white, peak: desc.MaxLuminance.max(white) };
                        }
                    }
                }
                o += 1;
            }
            a += 1;
        }
        sdr
    }
}

/// Where Windows puts SDR white on the monitor whose GDI name is `device` (`\\.\DISPLAY1`), in
/// nits: the "SDR content brightness" slider, through the display configuration interface (as
/// the Windows host's `hdr.rs` reads it). `None`: it isn't part of the desktop or won't say.
fn sdr_white(device: &str) -> Option<f32> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };
    use windows::Win32::Foundation::ERROR_SUCCESS;
    // SAFETY: the arrays are as long as Windows said, and each request says its own size.
    unsafe {
        let (mut paths, mut modes) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths, &mut modes) != ERROR_SUCCESS {
            return None;
        }
        let mut path = vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize];
        let mut mode = vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut paths, path.as_mut_ptr(), &mut modes, mode.as_mut_ptr(), None) != ERROR_SUCCESS {
            return None;
        }
        path.truncate(paths as usize);
        for p in path {
            let header = |kind, size: usize, adapter, id| DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id };
            let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME { header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(), p.sourceInfo.adapterId, p.sourceInfo.id), ..Default::default() };
            if DisplayConfigGetDeviceInfo(&mut name.header) != 0 {
                continue;
            }
            let gdi = &name.viewGdiDeviceName;
            if !String::from_utf16_lossy(&gdi[..gdi.iter().position(|&c| c == 0).unwrap_or(gdi.len())]).eq_ignore_ascii_case(device) {
                continue;
            }
            let mut level = DISPLAYCONFIG_SDR_WHITE_LEVEL { header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(), p.targetInfo.adapterId, p.targetInfo.id), SDRWhiteLevel: 0 };
            if DisplayConfigGetDeviceInfo(&mut level.header) != 0 || level.SDRWhiteLevel == 0 {
                return None;
            }
            // In thousandths of 80 nits.
            return Some(level.SDRWhiteLevel as f32 * 80.0 / 1000.0);
        }
        None
    }
}

impl Gpu {
    /// Every decoder profile the driver lists, by name where it is one of dxva.h's, with the
    /// formats it decodes into: for the log at the start and for `--profiles`.
    pub fn profiles(&self) -> Result<Vec<String>> {
        use windows::Win32::Graphics::Dxgi::Common::*;
        let formats = [
            (DXGI_FORMAT_NV12, "NV12"),
            (DXGI_FORMAT_P010, "P010"),
            (DXGI_FORMAT_P016, "P016"),
            (DXGI_FORMAT_YUY2, "YUY2"),
            (DXGI_FORMAT_Y210, "Y210"),
            (DXGI_FORMAT_Y216, "Y216"),
            (DXGI_FORMAT_AYUV, "AYUV"),
            (DXGI_FORMAT_Y410, "Y410"),
            (DXGI_FORMAT_Y416, "Y416"),
            (DXGI_FORMAT_420_OPAQUE, "420_OPAQUE"),
        ];
        // SAFETY: calls on the video device with values that live through each call.
        unsafe {
            let video: ID3D11VideoDevice = self.device.cast().context("the device has no video interface")?;
            let mut out = Vec::new();
            for i in 0..video.GetVideoDecoderProfileCount() {
                let Ok(guid) = video.GetVideoDecoderProfile(i) else { continue };
                let into: Vec<&str> = formats.iter().filter(|(f, _)| video.CheckVideoDecoderFormat(&guid, *f).is_ok_and(|ok| ok.as_bool())).map(|(_, n)| *n).collect();
                let name = PROFILE_NAMES.iter().find(|(g, _)| *g == guid).map_or_else(|| format!("{guid:?}"), |(_, n)| (*n).to_owned());
                out.push(format!("{name} [{}]", into.join(" ")));
            }
            Ok(out)
        }
    }
}

/// dxva.h's names for the decoder profiles a driver may list (the ones of codecs still met).
const PROFILE_NAMES: [(GUID, &str); 28] = [
    (GUID::from_u128(0xee27417f_5e28_4e65_beea_1d26b508adc9), "MPEG-2 VLD"),
    (GUID::from_u128(0x86695f12_340e_4f04_9fd3_9253dd327460), "MPEG-1/2 VLD"),
    (GUID::from_u128(0x1b81be68_a0c7_11d3_b984_00c04f2e73c5), "H.264 VLD"),
    (GUID::from_u128(0x1b81be69_a0c7_11d3_b984_00c04f2e73c5), "H.264 VLD with film grain"),
    (GUID::from_u128(0xd79be8da_0cf1_4c81_b82a_69a4e236f43d), "H.264 stereo progressive"),
    (GUID::from_u128(0xf9aaccbb_c2b6_4cfc_8779_5707b1760552), "H.264 stereo"),
    (GUID::from_u128(0x705b9d82_76cf_49d6_b7e6_ac8872db013c), "H.264 multiview"),
    (GUID::from_u128(0x1b81bea3_a0c7_11d3_b984_00c04f2e73c5), "VC-1 VLD"),
    (GUID::from_u128(0x1b81bea4_a0c7_11d3_b984_00c04f2e73c5), "VC-1 D2010"),
    (GUID::from_u128(0xefd64d74_c9e8_41d7_a5e9_e9b0e39fa319), "MPEG-4 part 2 simple"),
    (GUID::from_u128(0xed418a9f_010d_4eda_9ae3_9a65358d8d2e), "MPEG-4 part 2 advanced simple"),
    (GUID::from_u128(0x5b11d51b_2f4c_4452_bcc3_09f2a1160cc0), "HEVC Main"),
    (HEVC_MAIN10, "HEVC Main 10"),
    (GUID::from_u128(0x0685b993_3d8c_43a0_8b28_d74c2d6899a4), "HEVC Monochrome"),
    (GUID::from_u128(0x142a1d0f_69dd_4ec9_8591_b12ffcb91a29), "HEVC Monochrome 10"),
    (GUID::from_u128(0x1a72925f_0c2c_4f15_96fb_b17d1473603f), "HEVC Main 12"),
    (GUID::from_u128(0x0bac4fe5_1532_4429_a854_f84de04953db), "HEVC Main 10 4:2:2"),
    (GUID::from_u128(0x55bcac81_f311_4093_a7d0_1cbc0b849bee), "HEVC Main 12 4:2:2"),
    (GUID::from_u128(0x4008018f_f537_4b36_98cf_61af8a2c1a33), "HEVC Main 4:4:4"),
    (GUID::from_u128(0x9cc55490_e37c_4932_8684_4920f9f6409c), "HEVC Main 10 Ext"),
    (HEVC_MAIN444_10, "HEVC Main 4:4:4 10"),
    (GUID::from_u128(0x9798634d_fe9d_48e5_b4da_dbec45b3df01), "HEVC Main 4:4:4 12"),
    (GUID::from_u128(0x463707f8_a1d0_4585_876d_83aa6d60b89e), "VP9 profile 0"),
    (GUID::from_u128(0xa4c749ef_6ecf_48aa_8448_50a7a1165ff7), "VP9 10-bit profile 2"),
    (GUID::from_u128(0x90b899ea_3a62_4705_88b3_8df04b2744e7), "VP8"),
    (AV1_PROFILE0, "AV1 profile 0"),
    (GUID::from_u128(0x6936ff0f_45b1_4163_9cc1_646ef6946108), "AV1 profile 1 (4:4:4)"),
    (GUID::from_u128(0x0c5f2aa1_e541_4089_bb7b_98110a19d7c8), "AV1 profile 2"),
];

// The decoder profiles, from dxva.h.
const HEVC_MAIN10: GUID = GUID::from_u128(0x107af0e0_ef1a_4d19_aba8_67a163073d13);
/// DXVA_ModeHEVC_VLD_Main444_10_Ext, which Intel's and NVIDIA's drivers list where they have it.
const HEVC_MAIN444_10: GUID = GUID::from_u128(0x0dabeffa_4458_4602_bc03_0795659d617c);
const AV1_PROFILE0: GUID = GUID::from_u128(0xb8be4ccb_cf53_46ba_8d59_d6b8a6da5d2a);
