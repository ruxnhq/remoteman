//! Direct3D 12 Video Encode: a hardware HEVC encoder that works on any maker's GPU (AMD, Intel,
//! NVIDIA), where the application keeps the reference pictures itself. That is what gives the
//! Media Foundation path what it lacks: a loss is answered with a recovery frame that refers
//! only to a picture the client is known to have, and the rate changes live.
//!
//! What the driver writes and what this module writes: the driver writes each frame's slice
//! NAL units (headers included). The parameter sets (VPS, SPS, PPS) and the HDR SEI messages
//! are written here (`Bits`, `parameter_sets`), and put in front of every keyframe. Everything
//! in them that changes how a slice is parsed has to agree with what the driver assumes; that
//! is decided in `Params` and checked by decoding with FFmpeg (the tests at the end).
//!
//! The pictures kept: `DPB` reconstructed pictures live in one texture array (one slice
//! each, one more for the picture being made). Each frame refers to the newest one the client
//! has, and its reference picture set names all the others too, so the client's decoder keeps
//! them for later. The newest `RECENT` are kept as they come; the older places hold anchors a
//! quarter of a second apart, so that a loss reported late still finds a picture older than
//! the loss. `invalidate` drops a picture (and so the RPS stops naming it); when none is
//! left the answer is an error and so a keyframe.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result, bail, ensure};
use remoteman_encode::{FrameEncoder, HdrLight, How, Made};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D11::{D3D11_BOX, ID3D11Device5, ID3D11DeviceContext4, ID3D11Fence, ID3D11Texture2D};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_P010, DXGI_FORMAT_UNKNOWN, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory4};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
use windows::core::{Interface, BOOL};

use crate::capture::Gpu;
use crate::encode::Settings;

/// Reconstructed pictures kept to refer to: 5 is what a level 4.1 and up allows at 1080p and
/// 4K (6 in the decoder's picture buffer, counting the one being decoded).
const DPB: usize = 5;
/// The newest this many are always kept.
const RECENT: usize = 3;
/// POC's low bits: 16.
const POC_BITS: u8 = 16;

// ---------------------------------------------------------------------------------------------
// The bits of the parameter sets.

struct Bits {
    out: Vec<u8>,
    cur: u8,
    n: u8,
}

impl Bits {
    fn new() -> Self {
        Self { out: Vec::new(), cur: 0, n: 0 }
    }
    fn u(&mut self, bits: u32, v: u64) {
        for i in (0..bits).rev() {
            self.cur = self.cur << 1 | ((v >> i) & 1) as u8;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.cur);
                self.cur = 0;
                self.n = 0;
            }
        }
    }
    fn flag(&mut self, v: bool) {
        self.u(1, v as u64)
    }
    fn ue(&mut self, v: u32) {
        let v = v as u64 + 1;
        let len = 64 - v.leading_zeros();
        self.u(len - 1, 0);
        self.u(len, v);
    }
    fn se(&mut self, v: i32) {
        self.ue(if v > 0 { (2 * v - 1) as u32 } else { (-2 * v) as u32 })
    }
    fn trailing(&mut self) {
        self.flag(true);
        while self.n != 0 {
            self.flag(false);
        }
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.u(8, x as u64);
        }
    }
}

/// A NAL unit with its start code and header, emulation prevention applied to the payload.
fn nal(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 1, kind << 1, 1];
    let mut zeros = 0;
    for &b in payload {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// What the parameter sets say, from what the encoder was configured with.
#[derive(Clone, Debug)]
struct Params {
    width: u32,
    height: u32,
    /// The coded size (a multiple of 16) the driver encodes.
    coded_w: u32,
    coded_h: u32,
    level: i32,
    min_cb_log2: u32,
    max_cb_log2: u32,
    min_tb_log2: u32,
    max_tb_log2: u32,
    depth_inter: u32,
    depth_intra: u32,
    amp: bool,
    sao: bool,
    hdr: bool,
    light: Option<HdrLight>,
    // The choices the driver doesn't tell, found by decoding (see the tests).
    cu_qp_delta: bool,
    cabac_init_present: bool,
    strong_intra: bool,
    temporal_mvp: bool,
    transform_skip: bool,
}

fn ptl(b: &mut Bits, level: u32) {
    b.u(2, 0); // profile space
    b.u(1, 0); // main tier
    b.u(5, 2); // Main 10
    b.u(32, 1 << (31 - 2)); // compatible with profile 2
    b.u(4, 0b1001); // progressive, not interlaced, no non-packed constraint, frame only
    b.u(32, 0);
    b.u(12, 0); // 43 reserved bits and one more
    b.u(8, level as u64);
}

impl Params {
    fn level_idc(&self) -> u32 {
        // D3D12's HEVC level enum counts 1, 2, 2.1, 3, ... in steps its own way; the table
        // gives general_level_idc (30 times the level).
        [30, 60, 63, 90, 93, 120, 123, 150, 153, 156, 180, 183, 186][self.level as usize]
    }

    fn vps(&self) -> Vec<u8> {
        let mut b = Bits::new();
        b.u(4, 0);
        b.u(2, 3);
        b.u(6, 0);
        b.u(3, 0);
        b.flag(true);
        b.u(16, 0xffff);
        ptl(&mut b, self.level_idc());
        b.flag(true);
        b.ue(DPB as u32);
        b.ue(0);
        b.ue(0);
        b.u(6, 0);
        b.ue(0);
        b.flag(false);
        b.flag(false);
        b.trailing();
        nal(32, &b.out)
    }

    fn sps(&self) -> Vec<u8> {
        let mut b = Bits::new();
        b.u(4, 0); // vps id
        b.u(3, 0); // max sub layers - 1
        b.flag(true);
        ptl(&mut b, self.level_idc());
        b.ue(0); // sps id
        b.ue(1); // 4:2:0
        b.ue(self.coded_w);
        b.ue(self.coded_h);
        let crop = self.coded_w != self.width || self.coded_h != self.height;
        b.flag(crop);
        if crop {
            b.ue(0);
            b.ue((self.coded_w - self.width) / 2);
            b.ue(0);
            b.ue((self.coded_h - self.height) / 2);
        }
        b.ue(2); // luma 10 bits
        b.ue(2);
        b.ue(POC_BITS as u32 - 4);
        b.flag(true); // sub layer ordering info
        b.ue(DPB as u32);
        b.ue(0);
        b.ue(0);
        b.ue(self.min_cb_log2 - 3);
        b.ue(self.max_cb_log2 - self.min_cb_log2);
        b.ue(self.min_tb_log2 - 2);
        b.ue(self.max_tb_log2 - self.min_tb_log2);
        b.ue(self.depth_inter);
        b.ue(self.depth_intra);
        b.flag(false); // scaling lists
        b.flag(self.amp);
        b.flag(self.sao);
        b.flag(false); // pcm
        b.ue(0); // short-term RPS in the SPS: none, the slices carry theirs
        b.flag(false); // long-term pictures
        b.flag(self.temporal_mvp);
        b.flag(self.strong_intra);
        b.flag(true); // VUI
        b.flag(false); // aspect ratio
        b.flag(false); // overscan
        b.flag(true); // video signal type
        b.u(3, 5);
        b.flag(false); // limited range
        b.flag(true); // colour description
        let (p, t, m) = if self.hdr { (9, 16, 9) } else { (1, 1, 1) };
        b.u(8, p);
        b.u(8, t);
        b.u(8, m);
        b.flag(false); // chroma location
        b.flag(false); // neutral chroma
        b.flag(false); // field seq
        b.flag(false); // frame field info
        b.flag(false); // default display window
        b.flag(false); // timing
        b.flag(false); // bitstream restriction
        b.flag(false); // sps extension
        b.trailing();
        nal(33, &b.out)
    }

    fn pps(&self) -> Vec<u8> {
        let mut b = Bits::new();
        b.ue(0);
        b.ue(0);
        b.flag(false); // dependent slices
        b.flag(false); // output flag present
        b.u(3, 0); // extra slice header bits
        b.flag(env_flag("REMOTEMAN_D3D12_SDH", false)); // sign data hiding
        b.flag(self.cabac_init_present);
        b.ue(0); // refs in list 0 by default: 1
        b.ue(0);
        b.se(0); // init qp - 26
        b.flag(false); // constrained intra
        b.flag(self.transform_skip);
        b.flag(self.cu_qp_delta);
        if self.cu_qp_delta {
            b.ue(std::env::var("REMOTEMAN_D3D12_CUQPDEPTH").ok().and_then(|v| v.parse().ok()).unwrap_or(0));
        }
        b.se(0);
        b.se(0);
        b.flag(true); // slice chroma qp offsets present (the driver writes them)
        b.flag(false); // weighted pred
        b.flag(false); // weighted bipred
        b.flag(false); // transquant bypass
        b.flag(false); // tiles
        b.flag(false); // entropy coding sync
        b.flag(true); // loop filter across slices
        b.flag(false); // deblocking control
        b.flag(false); // scaling list data
        b.flag(false); // lists modification
        b.ue(0); // parallel merge level
        b.flag(false); // slice header extension
        b.flag(false); // pps extension
        b.trailing();
        nal(34, &b.out)
    }

    fn sei(&self) -> Vec<u8> {
        let Some(l) = self.light.filter(|_| self.hdr) else { return Vec::new() };
        let mut out = Vec::new();
        // Mastering display colour volume (137): BT.2020 primaries, D65.
        let mut m = Bits::new();
        for v in [8500u32, 39850, 6550, 2300, 35400, 14600, 15635, 16450] {
            m.u(16, v as u64);
        }
        m.u(32, (l.peak * 10000.0) as u64);
        m.u(32, (l.min * 10000.0) as u64);
        let mut s = Bits::new();
        s.u(8, 137);
        s.u(8, m.out.len() as u64);
        s.bytes(&m.out);
        s.trailing();
        out.extend(nal(39, &s.out));
        let mut s = Bits::new();
        s.u(8, 144);
        s.u(8, 4);
        s.u(16, l.max_cll as u64);
        s.u(16, l.max_fall as u64);
        s.trailing();
        out.extend(nal(39, &s.out));
        out
    }

    fn sets(&self) -> Vec<u8> {
        let mut v = self.vps();
        v.extend(self.sps());
        v.extend(self.pps());
        v.extend(self.sei());
        v
    }
}

// ---------------------------------------------------------------------------------------------
// D3D12.

fn transition(res: &ID3D12Resource, sub: u32, before: D3D12_RESOURCE_STATES, after: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER { pResource: ManuallyDrop::new(Some(res.clone())), Subresource: sub, StateBefore: before, StateAfter: after }),
        },
    }
}

fn drop_barrier(b: D3D12_RESOURCE_BARRIER) {
    // SAFETY: built by `transition`, which put a clone of the resource in.
    unsafe {
        let mut t = ManuallyDrop::into_inner(b.Anonymous.Transition);
        ManuallyDrop::drop(&mut t.pResource);
    }
}

fn heap(kind: D3D12_HEAP_TYPE) -> D3D12_HEAP_PROPERTIES {
    D3D12_HEAP_PROPERTIES { Type: kind, ..Default::default() }
}

/// Memory the video encoder may write and the CPU read: a custom heap of the system's memory,
/// cached for the CPU. A readback heap isn't: only a copy may write to one, and while AMD's driver
/// lets the encoder write its bitstream and metadata there all the same, Intel's writes nothing,
/// so every frame read back as 0 bytes.
fn readable_heap() -> D3D12_HEAP_PROPERTIES {
    D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_CUSTOM,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
        MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
        ..Default::default()
    }
}

fn buffer_desc(size: u64) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_NONE,
    }
}

fn texture_desc(w: u32, h: u32, slices: u16, flags: D3D12_RESOURCE_FLAGS) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: w as u64,
        Height: h,
        DepthOrArraySize: slices,
        MipLevels: 1,
        Format: DXGI_FORMAT_P010,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: flags,
    }
}

/// What the GPU's encoder says it can do, for the log and for the choices below.
#[derive(Clone, Debug, Default)]
pub struct Caps {
    pub level: i32,
    pub max_l0_refs: u32,
    pub max_dpb: u32,
    pub max_ltr: u32,
    pub support_flags: i32,
    pub min_cb: i32,
    pub max_cb: i32,
    pub min_tb: i32,
    pub max_tb: i32,
    pub depth_inter: u8,
    pub depth_intra: u8,
    pub config_flags: i32,
    pub meta_size: u32,
}

fn d3d12_device(gpu: &Gpu) -> Result<ID3D12Device> {
    // SAFETY: DXGI and D3D12 calls as documented.
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory1()?;
        let adapter: IDXGIAdapter = factory.EnumAdapterByLuid(windows::Win32::Foundation::LUID { LowPart: gpu.luid.0, HighPart: gpu.luid.1 })?;
        if std::env::var_os("REMOTEMAN_D3D12_DEBUG").is_some() {
            let mut dbg: Option<ID3D12Debug> = None;
            if D3D12GetDebugInterface(&mut dbg).is_ok() {
                if let Some(d) = dbg {
                    d.EnableDebugLayer();
                }
            }
        }
        let mut device: Option<ID3D12Device> = None;
        D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device).context("a Direct3D 12 device")?;
        device.context("no device")
    }
}

/// A Main 10 HEVC encode configuration for this size on this video device, as the driver
/// allows it.
struct Chosen {
    caps: Caps,
    config: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC,
    level: D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC,
}

fn level_for(w: u32, h: u32) -> i32 {
    // 5.1 (index 8) covers 4K at 60; beyond 4K, 6.2 (index 12).
    if (w as u64) * (h as u64) <= 8_912_896 { D3D12_VIDEO_ENCODER_LEVELS_HEVC_51.0 } else { D3D12_VIDEO_ENCODER_LEVELS_HEVC_62.0 }
}

fn check<T>(video: &ID3D12VideoDevice3, feature: D3D12_FEATURE_VIDEO, data: &mut T) -> Result<()> {
    // SAFETY: `data` is the feature's structure and its size is passed with it.
    unsafe { video.CheckFeatureSupport(feature, data as *mut T as *mut _, std::mem::size_of::<T>() as u32) }.map_err(|e| anyhow::anyhow!("CheckFeatureSupport({}): {e}", feature.0))
}

/// The multiples of the picture size the encoder asks for: AMD's encodes rows in groups of 16,
/// and a 1080-row picture must be coded as 1088 rows and cropped (FFmpeg's encoder does the same).
/// A driver that doesn't say asks for the smallest coding unit, 8.
fn size_multiples(video: &ID3D12VideoDevice3, vendor: u32) -> (u32, u32) {
    // The driver says 1 (no requirement) all the same. AMD's encoder codes whole coding tree
    // blocks (64x64): a stream that says 1080 rows decodes wrongly in its last 56, and one that
    // says 368 (a multiple of 16, not of 64) fails to parse in a picture of a few hundred
    // (found by bisecting; 384 is clean over thousands). The picture is coded at the aligned size
    // and cropped by the stream's parameter sets. NVIDIA's takes any multiple of the smallest
    // coding unit.
    let floor: u32 = std::env::var("REMOTEMAN_D3D12_ALIGN").ok().and_then(|v| v.parse().ok()).unwrap_or(if vendor == 0x1002 { 64 } else { 8 });
    let mut r = D3D12_FEATURE_DATA_VIDEO_ENCODER_OUTPUT_RESOLUTION { Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC, ..Default::default() };
    let said = check(video, D3D12_FEATURE_VIDEO_ENCODER_OUTPUT_RESOLUTION, &mut r);
    if env_flag("REMOTEMAN_D3D12_DEBUG", false) {
        eprintln!("output resolution: {said:?} supported {} width multiple {} height multiple {} min {}x{} max {}x{}", r.IsSupported.as_bool(), r.ResolutionWidthMultipleRequirement, r.ResolutionHeightMultipleRequirement, r.MinResolutionSupported.Width, r.MinResolutionSupported.Height, r.MaxResolutionSupported.Width, r.MaxResolutionSupported.Height);
    }
    let multiple = |m: u32| if said.is_ok() && r.IsSupported.as_bool() && m.is_power_of_two() { m.max(floor) } else { floor };
    (multiple(r.ResolutionWidthMultipleRequirement), multiple(r.ResolutionHeightMultipleRequirement))
}

fn choose(video: &ID3D12VideoDevice3, vendor: u32, w: u32, h: u32, fps: u32, bitrate: u32) -> Result<Chosen> {
    let mut profile = D3D12_VIDEO_ENCODER_PROFILE_HEVC_MAIN10;
    let profile_ptr: *mut D3D12_VIDEO_ENCODER_PROFILE_HEVC = &mut profile;
    let profile_desc = || D3D12_VIDEO_ENCODER_PROFILE_DESC { DataSize: 4, Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 { pHEVCProfile: profile_ptr } };
    let mut codec = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC { Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC, ..Default::default() };
    check(video, D3D12_FEATURE_VIDEO_ENCODER_CODEC, &mut codec)?;
    ensure!(codec.IsSupported.as_bool(), "this GPU has no Direct3D 12 HEVC encoder");

    let (mut min_l, mut max_l) = (D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC::default(), D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC::default());
    let mut pl = D3D12_FEATURE_DATA_VIDEO_ENCODER_PROFILE_LEVEL {
        Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
        Profile: profile_desc(),
        MinSupportedLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING { DataSize: 8, Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 { pHEVCLevelSetting: &mut min_l } },
        MaxSupportedLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING { DataSize: 8, Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 { pHEVCLevelSetting: &mut max_l } },
        ..Default::default()
    };
    check(video, D3D12_FEATURE_VIDEO_ENCODER_PROFILE_LEVEL, &mut pl)?;
    ensure!(pl.IsSupported.as_bool(), "the HEVC encoder has no Main 10 profile");
    let level = level_for(w, h).min(max_l.Level.0);

    let mut fmt = D3D12_FEATURE_DATA_VIDEO_ENCODER_INPUT_FORMAT { Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC, Profile: profile_desc(), Format: DXGI_FORMAT_P010, ..Default::default() };
    check(video, D3D12_FEATURE_VIDEO_ENCODER_INPUT_FORMAT, &mut fmt)?;
    ensure!(fmt.IsSupported.as_bool(), "the HEVC encoder takes no P010 input");

    // The query takes the configuration we would like and says whether it is supported: try
    // from the biggest coding units down.
    let mut found = None;
    // What NVIDIA takes first, then every combination (another maker's driver may take only one).
    let mut tries: Vec<(i32, i32, i32, i32, u8)> = [(3, 3u8), (2, 3), (2, 2), (1, 2), (1, 1), (2, 1)].into_iter().map(|(m, d)| (0, m, 0, 3.min(m + 1), d)).collect();
    for max_cu in (0..=3).rev() {
        for min_cu in 0..=max_cu {
            for min_tu in 0..=3 {
                for max_tu in min_tu..=3 {
                    for depth in (0..=4u8).rev() {
                        tries.push((min_cu, max_cu, min_tu, max_tu, depth));
                    }
                }
            }
        }
    }
    for (min_cu, max_cu, min_tu, max_tu, depth) in tries {
        let mut sup = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC {
            SupportFlags: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC_FLAG_NONE,
            MinLumaCodingUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE(min_cu),
            MaxLumaCodingUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE(max_cu),
            MinLumaTransformUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE(min_tu),
            MaxLumaTransformUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE(max_tu),
            max_transform_hierarchy_depth_inter: depth,
            max_transform_hierarchy_depth_intra: depth,
        };
        let mut cs = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT {
            Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
            Profile: profile_desc(),
            CodecSupportLimits: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT {
                DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC>() as u32,
                Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_0 { pHEVCSupport: &mut sup },
            },
            ..Default::default()
        };
        let said = check(video, D3D12_FEATURE_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT, &mut cs);
        if std::env::var_os("REMOTEMAN_D3D12_DEBUG").is_some() && said.is_ok() && cs.IsSupported.as_bool() {
            eprintln!("config query max_cu {max_cu} depth {depth}: {said:?} supported {} flags {} min_cu {} max_cu {} min_tu {} max_tu {} depth {}/{}", cs.IsSupported.as_bool(), sup.SupportFlags.0, sup.MinLumaCodingUnitSize.0, sup.MaxLumaCodingUnitSize.0, sup.MinLumaTransformUnitSize.0, sup.MaxLumaTransformUnitSize.0, sup.max_transform_hierarchy_depth_inter, sup.max_transform_hierarchy_depth_intra);
        }
        if said.is_ok() && cs.IsSupported.as_bool() {
            found = Some(sup);
            break;
        }
    }
    let sup = found.context("the HEVC encoder takes none of the coding unit configurations tried")?;

    let mut pc = D3D12_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT_HEVC::default();
    let mut ps = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT {
        Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
        Profile: profile_desc(),
        PictureSupport: D3D12_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT {
            DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT_HEVC>() as u32,
            Anonymous: D3D12_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT_0 { pHEVCSupport: &mut pc },
        },
        ..Default::default()
    };
    check(video, D3D12_FEATURE_VIDEO_ENCODER_CODEC_PICTURE_CONTROL_SUPPORT, &mut ps)?;
    ensure!(ps.IsSupported.as_bool() && pc.MaxL0ReferencesForP >= 1, "the HEVC encoder takes no P frames");

    // Our configuration: the smallest coding units the encoder allows up to 32x32 (64x64 where
    // it has no smaller maximum), its transform units, its hierarchy depths; no AMP, no SAO.
    let config = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC {
        ConfigurationFlags: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_FLAGS(
            (if sup.SupportFlags.0 & D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC_FLAG_ASYMETRIC_MOTION_PARTITION_REQUIRED.0 != 0 { D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_FLAG_USE_ASYMETRIC_MOTION_PARTITION.0 } else { 0 })
                // AMD's encoder uses asymmetric partitions whether it was asked to or not, and
                // a stream whose SPS says it doesn't (the flag unset) fails to parse now and then
                // (found as a CABAC error once in some hundreds of pictures). Say so.
                | if sup.SupportFlags.0 & D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC_FLAG_ASYMETRIC_MOTION_PARTITION_SUPPORT.0 != 0 && env_flag("REMOTEMAN_D3D12_AMP", vendor == 0x1002) { D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_FLAG_USE_ASYMETRIC_MOTION_PARTITION.0 } else { 0 },
        ),
        MinLumaCodingUnitSize: sup.MinLumaCodingUnitSize,
        MaxLumaCodingUnitSize: sup.MaxLumaCodingUnitSize,
        MinLumaTransformUnitSize: sup.MinLumaTransformUnitSize,
        MaxLumaTransformUnitSize: sup.MaxLumaTransformUnitSize,
        max_transform_hierarchy_depth_inter: sup.max_transform_hierarchy_depth_inter,
        max_transform_hierarchy_depth_intra: sup.max_transform_hierarchy_depth_intra,
    };

    // The whole thing together.
    let (cw, ch) = (w, h);
    let mut cfg = config;
    let mut gop = D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC { GOPLength: 0, PPicturePeriod: 0, log2_max_pic_order_cnt_lsb_minus4: POC_BITS - 4 };
    let cbr = cbr_params(bitrate, fps, true);
    let res = D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC { Width: cw, Height: ch };
    let (mut sp_profile, mut sp_level) = (D3D12_VIDEO_ENCODER_PROFILE_HEVC_MAIN10, D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC::default());
    let mut s = D3D12_FEATURE_DATA_VIDEO_ENCODER_SUPPORT {
        Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
        InputFormat: DXGI_FORMAT_P010,
        CodecConfiguration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION { DataSize: std::mem::size_of_val(&cfg) as u32, Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 { pHEVCConfig: &mut cfg } },
        CodecGopSequence: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE { DataSize: std::mem::size_of_val(&gop) as u32, Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 { pHEVCGroupOfPictures: &mut gop } },
        RateControl: rate_control(&cbr, fps, true),
        IntraRefresh: D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE,
        SubregionFrameEncoding: D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME,
        ResolutionsListCount: 1,
        pResolutionList: &res,
        SuggestedProfile: D3D12_VIDEO_ENCODER_PROFILE_DESC { DataSize: 4, Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 { pHEVCProfile: &mut sp_profile } },
        SuggestedLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING { DataSize: 8, Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 { pHEVCLevelSetting: &mut sp_level } },
        ..Default::default()
    };
    let mut limits = D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOLUTION_SUPPORT_LIMITS::default();
    s.pResolutionDependentSupport = &mut limits;
    check(video, D3D12_FEATURE_VIDEO_ENCODER_SUPPORT, &mut s)?;
    ensure!(
        s.SupportFlags.0 & D3D12_VIDEO_ENCODER_SUPPORT_FLAG_GENERAL_SUPPORT_OK.0 != 0,
        "the HEVC encoder doesn't take this setup: validation flags {:#x}, support flags {:#x}",
        s.ValidationFlags.0,
        s.SupportFlags.0
    );

    let mut rr = D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS { Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC, Profile: profile_desc(), InputFormat: DXGI_FORMAT_P010, PictureTargetResolution: res, ..Default::default() };
    check(video, D3D12_FEATURE_VIDEO_ENCODER_RESOURCE_REQUIREMENTS, &mut rr)?;
    ensure!(rr.IsSupported.as_bool(), "no resource requirements for this size");

    let caps = Caps {
        level,
        max_l0_refs: pc.MaxL0ReferencesForP,
        max_dpb: pc.MaxDPBCapacity.max(s.MaxReferenceFramesInDPB),
        max_ltr: pc.MaxLongTermReferences,
        support_flags: s.SupportFlags.0,
        min_cb: sup.MinLumaCodingUnitSize.0,
        max_cb: sup.MaxLumaCodingUnitSize.0,
        min_tb: sup.MinLumaTransformUnitSize.0,
        max_tb: sup.MaxLumaTransformUnitSize.0,
        depth_inter: sup.max_transform_hierarchy_depth_inter,
        depth_intra: sup.max_transform_hierarchy_depth_intra,
        config_flags: sup.SupportFlags.0,
        meta_size: rr.MaxEncoderOutputMetadataBufferSize,
    };
    Ok(Chosen { caps, config, level: D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC { Level: D3D12_VIDEO_ENCODER_LEVELS_HEVC(level), Tier: D3D12_VIDEO_ENCODER_TIER_HEVC_MAIN } })
}

/// The sequence's settings the first frame says changed: each only when the driver can change it.
/// A driver that can't, such as Intel's for its Arc cards, refuses the frame as a whole ("the
/// parameter is incorrect") when asked to, even for the first frame, while AMD's, which can change
/// all of them, takes them as the first settings either way.
fn first_frame_flags(support: i32) -> D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAGS {
    let pairs = [
        (D3D12_VIDEO_ENCODER_SUPPORT_FLAG_RESOLUTION_RECONFIGURATION_AVAILABLE.0, D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_RESOLUTION_CHANGE.0),
        (D3D12_VIDEO_ENCODER_SUPPORT_FLAG_RATE_CONTROL_RECONFIGURATION_AVAILABLE.0, D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_RATE_CONTROL_CHANGE.0),
        (D3D12_VIDEO_ENCODER_SUPPORT_FLAG_SEQUENCE_GOP_RECONFIGURATION_AVAILABLE.0, D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_GOP_SEQUENCE_CHANGE.0),
        (D3D12_VIDEO_ENCODER_SUPPORT_FLAG_SUBREGION_LAYOUT_RECONFIGURATION_AVAILABLE.0, D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_SUBREGION_LAYOUT_CHANGE.0),
    ];
    D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAGS(
        pairs.iter().filter(|(can, _)| support & can != 0).fold(0, |flags, (_, change)| flags | change),
    )
}

fn cbr_params(bitrate: u32, fps: u32, vbv: bool) -> D3D12_VIDEO_ENCODER_RATE_CONTROL_CBR {
    let frame = (bitrate as u64 / fps.max(1) as u64).max(1000);
    D3D12_VIDEO_ENCODER_RATE_CONTROL_CBR { InitialQP: 0, MinQP: 0, MaxQP: 0, MaxFrameBitSize: 0, TargetBitRate: bitrate as u64, VBVCapacity: if vbv { frame } else { 0 }, InitialVBVFullness: if vbv { frame } else { 0 } }
}

fn rate_control(cbr: &D3D12_VIDEO_ENCODER_RATE_CONTROL_CBR, fps: u32, vbv: bool) -> D3D12_VIDEO_ENCODER_RATE_CONTROL {
    D3D12_VIDEO_ENCODER_RATE_CONTROL {
        Mode: D3D12_VIDEO_ENCODER_RATE_CONTROL_MODE_CBR,
        Flags: if vbv { D3D12_VIDEO_ENCODER_RATE_CONTROL_FLAG_ENABLE_VBV_SIZES } else { D3D12_VIDEO_ENCODER_RATE_CONTROL_FLAG_NONE },
        ConfigParams: D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS {
            DataSize: std::mem::size_of::<D3D12_VIDEO_ENCODER_RATE_CONTROL_CBR>() as u32,
            Anonymous: D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS_0 { pConfiguration_CBR: cbr },
        },
        TargetFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
    }
}

struct Pic {
    poc: u32,
    /// The frame number the pipeline gave it.
    n: u64,
    slice: usize,
}

struct Waiter {
    fence: ID3D12Fence,
    event: HANDLE,
    value: u64,
}

impl Waiter {
    fn wait(&self) -> Result<()> {
        // SAFETY: a live fence and event.
        unsafe {
            if self.fence.GetCompletedValue() < self.value {
                self.fence.SetEventOnCompletion(self.value, self.event)?;
                WaitForSingleObject(self.event, INFINITE);
            }
        }
        Ok(())
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        // SAFETY: the event is ours.
        unsafe {
            let _ = CloseHandle(self.event);
        }
    }
}

pub struct Encoder {
    note: String,
    params: Params,
    caps: Caps,
    gpu_context: ID3D11DeviceContext4,
    input11: ID3D11Texture2D,
    shared11: ID3D11Texture2D,
    fence11: ID3D11Fence,
    shared_fence: ID3D12Fence,
    queue: ID3D12CommandQueue,
    device: ID3D12Device,
    allocator: ID3D12CommandAllocator,
    list: ID3D12VideoEncodeCommandList2,
    encoder: ID3D12VideoEncoder,
    heap: ID3D12VideoEncoderHeap,
    input: ID3D12Resource,
    recon: ID3D12Resource,
    recon_state: Vec<D3D12_RESOURCE_STATES>,
    bitstream: ID3D12Resource,
    bitstream_size: u64,
    meta: ID3D12Resource,
    resolved: ID3D12Resource,
    bitstream_state: D3D12_RESOURCE_STATES,
    resolved_state: D3D12_RESOURCE_STATES,
    waiter: Waiter,
    shared_value: u64,
    fps: u32,
    bitrate: u32,
    /// A new rate not yet given to the encoder.
    rate_change: bool,
    vbv: bool,
    reconfigurable: bool,
    first: bool,
    pics: Vec<Pic>,
    free: Vec<usize>,
    poc: u32,
    last_anchor: u64,
    out: Vec<u8>,
    last_qp: u32,
    /// The time the GPU took for the last frame, in microseconds, for the tests.
    pub last_gpu_us: u64,
}

// SAFETY: the pipeline moves its encoder to its own thread and uses it there alone; the
// interfaces are free-threaded.
unsafe impl Send for Encoder {}

impl Encoder {
    /// An encoder for pictures in `input`, a P010 texture of `gpu`'s device.
    pub fn open(gpu: &Gpu, input: &ID3D11Texture2D, s: &Settings) -> Result<Self> {
        ensure!(s.mode.codec == remoteman_core::Codec::Hevc && s.mode.chroma == remoteman_core::Chroma::C420, "Direct3D 12 encoding here is HEVC 4:2:0 only");
        let (w, h, fps) = (s.width, s.height, s.fps.max(1));
        // The coded size is the picture's: the input is copied whole (a planar texture is copied
        // as a whole or not at all), so a size the smallest coding unit (8) doesn't divide is
        // left to the other encoders.
        ensure!(w % 8 == 0 && h % 8 == 0, "Direct3D 12 encoding here needs a picture size that 8 divides, not {w}x{h}");
        // SAFETY: Direct3D 12 calls as documented, with live arguments and out-parameters.
        unsafe {
            let device = d3d12_device(gpu)?;
            let video: ID3D12VideoDevice3 = device.cast().context("ID3D12VideoDevice3 (no Direct3D 12 video on this GPU or driver)")?;
            let (mw, mh) = size_multiples(&video, gpu.vendor);
            let (cw, ch) = (w.next_multiple_of(mw), h.next_multiple_of(mh));
            let chosen = choose(&video, gpu.vendor, cw, ch, fps, s.bitrate)?;
            let caps = chosen.caps.clone();
            let vbv = caps.support_flags & D3D12_VIDEO_ENCODER_SUPPORT_FLAG_RATE_CONTROL_VBV_SIZE_CONFIG_AVAILABLE.0 != 0 && env_flag("REMOTEMAN_D3D12_VBV", true);
            let reconfigurable = caps.support_flags & D3D12_VIDEO_ENCODER_SUPPORT_FLAG_RATE_CONTROL_RECONFIGURATION_AVAILABLE.0 != 0;
            let array = caps.support_flags & D3D12_VIDEO_ENCODER_SUPPORT_FLAG_RECONSTRUCTED_FRAMES_REQUIRE_TEXTURE_ARRAYS.0 != 0;

            let mut profile = D3D12_VIDEO_ENCODER_PROFILE_HEVC_MAIN10;
            let mut config = chosen.config;
            let desc = D3D12_VIDEO_ENCODER_DESC {
                NodeMask: 0,
                Flags: D3D12_VIDEO_ENCODER_FLAG_NONE,
                EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
                EncodeProfile: D3D12_VIDEO_ENCODER_PROFILE_DESC { DataSize: 4, Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 { pHEVCProfile: &mut profile } },
                InputFormat: DXGI_FORMAT_P010,
                CodecConfiguration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION { DataSize: std::mem::size_of_val(&config) as u32, Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 { pHEVCConfig: &mut config } },
                MaxMotionEstimationPrecision: D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_QUARTER_PIXEL,
            };
            let encoder: ID3D12VideoEncoder = video.CreateVideoEncoder(&desc).with_context(|| format!("CreateVideoEncoder (config {:?}, caps {:?})", config, caps))?;
            let mut level = chosen.level;
            let res = D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC { Width: cw, Height: ch };
            let hdesc = D3D12_VIDEO_ENCODER_HEAP_DESC {
                NodeMask: 0,
                Flags: D3D12_VIDEO_ENCODER_HEAP_FLAG_NONE,
                EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
                EncodeProfile: D3D12_VIDEO_ENCODER_PROFILE_DESC { DataSize: 4, Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 { pHEVCProfile: &mut profile } },
                EncodeLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING { DataSize: 8, Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 { pHEVCLevelSetting: &mut level } },
                ResolutionsListCount: 1,
                pResolutionList: &res,
            };
            let vheap: ID3D12VideoEncoderHeap = video.CreateVideoEncoderHeap(&hdesc).context("CreateVideoEncoderHeap")?;

            let queue: ID3D12CommandQueue = device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE, ..Default::default() }).context("the video encode queue")?;
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE)?;
            let device4: ID3D12Device4 = device.cast()?;
            let list: ID3D12VideoEncodeCommandList2 = device4.CreateCommandList1(0, D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE, D3D12_COMMAND_LIST_FLAG_NONE).context("the video encode command list")?;

            // The input: a texture both APIs open (made in Direct3D 11, which shares P010 more
            // readily), the converter's output copied into it.
            let sd = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC {
                Width: cw,
                Height: ch,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_P010,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: windows::Win32::Graphics::Direct3D11::D3D11_USAGE_DEFAULT,
                BindFlags: windows::Win32::Graphics::Direct3D11::D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: (windows::Win32::Graphics::Direct3D11::D3D11_RESOURCE_MISC_SHARED.0 | windows::Win32::Graphics::Direct3D11::D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
            };
            let mut shared11: Option<ID3D11Texture2D> = None;
            gpu.device.CreateTexture2D(&sd, None, Some(&mut shared11)).context("the shared input texture (Direct3D 11)")?;
            let shared11 = shared11.context("no input texture")?;
            let dxgi: windows::Win32::Graphics::Dxgi::IDXGIResource1 = shared11.cast().context("IDXGIResource1")?;
            let handle = dxgi.CreateSharedHandle(None, 0x1000_0000, None).context("sharing the input texture")?;
            let mut shared: Option<ID3D12Resource> = None;
            let opened = device.OpenSharedHandle(handle, &mut shared);
            let _ = CloseHandle(handle);
            opened.context("opening the input texture in Direct3D 12")?;
            let shared = shared.context("no input texture")?;
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_SHARED).context("a shared fence")?;
            let fh = device.CreateSharedHandle(&fence, None, 0x1000_0000, None).context("sharing the fence")?;
            let device5: ID3D11Device5 = gpu.device.cast().context("ID3D11Device5")?;
            let mut fence11_out: Option<ID3D11Fence> = None;
            let fence11 = device5.OpenSharedFence(fh, &mut fence11_out).map(|_| fence11_out);
            let _ = CloseHandle(fh);
            let fence11 = fence11.context("opening the fence in Direct3D 11")?.context("no fence")?;
            let gpu_context: ID3D11DeviceContext4 = gpu.context.cast().context("ID3D11DeviceContext4")?;

            let recon_slices = DPB + 1;
            let mut recon: Option<ID3D12Resource> = None;
            device
                .CreateCommittedResource(&heap(D3D12_HEAP_TYPE_DEFAULT), D3D12_HEAP_FLAG_NONE, &texture_desc(cw, ch, recon_slices as u16, if env_flag("REMOTEMAN_D3D12_REFONLY", true) { D3D12_RESOURCE_FLAGS(D3D12_RESOURCE_FLAG_VIDEO_ENCODE_REFERENCE_ONLY.0 | D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE.0) } else { D3D12_RESOURCE_FLAG_NONE }), D3D12_RESOURCE_STATE_COMMON, None, &mut recon)
                .context("the reconstructed pictures")?;
            let _ = array;

            // Output: the bitstream and the resolved metadata where the CPU can read them
            // (a readback heap), the encoder's own metadata in the GPU's.
            let bitstream_size = ((cw as u64 * ch as u64 * 3 / 2) * 2).max(1 << 20).next_multiple_of(1 << 16);
            let mut bitstream: Option<ID3D12Resource> = None;
            let mut bitstream_state = D3D12_RESOURCE_STATE_COMMON;
            device
                .CreateCommittedResource(&readable_heap(), D3D12_HEAP_FLAG_NONE, &buffer_desc(bitstream_size), bitstream_state, None, &mut bitstream)
                .context("the bitstream buffer")?;
            let mut meta: Option<ID3D12Resource> = None;
            device
                .CreateCommittedResource(&heap(D3D12_HEAP_TYPE_DEFAULT), D3D12_HEAP_FLAG_NONE, &buffer_desc(caps.meta_size.max(256) as u64), D3D12_RESOURCE_STATE_COMMON, None, &mut meta)
                .context("the metadata buffer")?;
            let mut resolved: Option<ID3D12Resource> = None;
            let resolved_state = D3D12_RESOURCE_STATE_COMMON;
            device
                .CreateCommittedResource(&readable_heap(), D3D12_HEAP_FLAG_NONE, &buffer_desc(4096), resolved_state, None, &mut resolved)
                .context("the resolved metadata buffer")?;
            bitstream_state = bitstream_state;

            let d12fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let event = CreateEventW(None, false, false, None)?;
            let _ = HMODULE::default();

            let params = Params {
                width: w,
                height: h,
                coded_w: cw,
                coded_h: ch,
                level: caps.level,
                min_cb_log2: 3 + config.MinLumaCodingUnitSize.0 as u32,
                max_cb_log2: 3 + config.MaxLumaCodingUnitSize.0 as u32,
                min_tb_log2: 2 + config.MinLumaTransformUnitSize.0 as u32,
                max_tb_log2: 2 + config.MaxLumaTransformUnitSize.0 as u32,
                depth_inter: std::env::var("REMOTEMAN_D3D12_SPSDEPTH").ok().and_then(|v| v.parse().ok()).unwrap_or(config.max_transform_hierarchy_depth_inter as u32),
                depth_intra: std::env::var("REMOTEMAN_D3D12_SPSDEPTH").ok().and_then(|v| v.parse().ok()).unwrap_or(config.max_transform_hierarchy_depth_intra as u32),
                amp: config.ConfigurationFlags.0 & D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_FLAG_USE_ASYMETRIC_MOTION_PARTITION.0 != 0,
                sao: false,
                hdr: s.hdr,
                light: s.light,
                cu_qp_delta: env_flag("REMOTEMAN_D3D12_CUQP", true),
                cabac_init_present: env_flag("REMOTEMAN_D3D12_CABACINIT", true),
                strong_intra: env_flag("REMOTEMAN_D3D12_STRONG", false),
                temporal_mvp: env_flag("REMOTEMAN_D3D12_TMVP", false),
                transform_skip: env_flag("REMOTEMAN_D3D12_TSKIP", false),
            };
            let note = format!("Direct3D 12 HEVC Main10 level idx {} on {}: {} refs/P, DPB {}, flags {:#x}, vbv {vbv}, reconfig {reconfigurable}", caps.level, gpu.adapter, caps.max_l0_refs, caps.max_dpb, caps.support_flags);
            if std::env::var_os("REMOTEMAN_D3D12_DEBUG").is_some() {
                eprintln!("{note}
{caps:?}
{config:?}");
            }
            Ok(Self {
                note,
                params,
                caps,
                gpu_context,
                input11: input.clone(),
                shared11,
                fence11,
                shared_fence: fence,
                queue,
                device: device.clone(),
                allocator,
                list,
                encoder,
                heap: vheap,
                input: shared,
                recon: recon.context("no reconstruction texture")?,
                recon_state: vec![D3D12_RESOURCE_STATE_COMMON; recon_slices],
                bitstream: bitstream.context("no bitstream buffer")?,
                bitstream_size,
                meta: meta.context("no metadata buffer")?,
                resolved: resolved.context("no resolved buffer")?,
                bitstream_state,
                resolved_state,
                waiter: Waiter { fence: d12fence, event, value: 0 },
                shared_value: 0,
                fps,
                bitrate: s.bitrate,
                rate_change: false,
                vbv,
                reconfigurable,
                first: true,
                pics: Vec::new(),
                free: (0..recon_slices).collect(),
                poc: 0,
                last_anchor: 0,
                out: Vec::new(),
                last_qp: 0,
                last_gpu_us: 0,
            })
        }
    }

    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    /// The parameter sets this encoder puts before a keyframe.
    pub fn parameter_sets(&self) -> Vec<u8> {
        self.params.sets()
    }

    fn encode_frame(&mut self, n: u64, idr: bool) -> Result<bool> {
        let started = std::time::Instant::now();
        let p = &self.params;
        // Which picture to build it from: the newest the client has.
        let key = idr || self.first || self.pics.is_empty();
        if key {
            for pic in self.pics.drain(..) {
                self.free.push(pic.slice);
            }
            self.poc = 0;
        } else {
            self.poc += 1;
        }
        let work = self.free.pop().context("no free picture")?;

        // Copy the converter's picture into the shared texture and hand it over.
        // SAFETY: live interfaces of the device the converter works on and of the encoder's.
        unsafe {
            if (self.params.coded_w, self.params.coded_h) == (self.params.width, self.params.height) {
                self.gpu_context.CopyResource(&self.shared11, &self.input11);
            } else {
                // The picture is smaller than the size coded (rows or columns added for the
                // encoder's alignment, cropped away again by the stream's parameter sets).
                let area = D3D11_BOX { left: 0, top: 0, front: 0, right: self.params.width, bottom: self.params.height, back: 1 };
                self.gpu_context.CopySubresourceRegion(&self.shared11, 0, 0, 0, 0, &self.input11, 0, Some(&area));
            }
            self.shared_value += 1;
            self.gpu_context.Signal(&self.fence11, self.shared_value)?;
        }

        // Newest first, so that each reference's distance grows along the list: the short-term
        // reference picture set in a slice header is written in this order and has to be sorted
        // so (AMD's driver writes it as given, and a decoder finds the wrong pictures otherwise).
        let refs: Vec<usize> = if key { Vec::new() } else { (0..self.pics.len()).rev().collect() };
        let ref_i = self.pics.len().wrapping_sub(1); // the newest
        let mut textures: Vec<Option<ID3D12Resource>> = refs.iter().map(|_| Some(self.recon.clone())).collect();
        let mut subs: Vec<u32> = refs.iter().map(|&i| self.pics[i].slice as u32).collect();
        let mut descs: Vec<D3D12_VIDEO_ENCODER_REFERENCE_PICTURE_DESCRIPTOR_HEVC> = refs
            .iter()
            .enumerate()
            .map(|(at, &i)| D3D12_VIDEO_ENCODER_REFERENCE_PICTURE_DESCRIPTOR_HEVC {
                ReconstructedPictureResourceIndex: at as u32,
                IsRefUsedByCurrentPic: BOOL::from(i == ref_i),
                IsLongTermReference: BOOL::from(false),
                PictureOrderCountNumber: self.pics[i].poc,
                TemporalLayerIndex: 0,
            })
            .collect();
        let mut list0 = [0u32]; // the newest, first in the arrays above
        let mut data = D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC {
            Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC_FLAG_NONE,
            FrameType: if key { D3D12_VIDEO_ENCODER_FRAME_TYPE_HEVC_IDR_FRAME } else { D3D12_VIDEO_ENCODER_FRAME_TYPE_HEVC_P_FRAME },
            slice_pic_parameter_set_id: 0,
            PictureOrderCountNumber: self.poc,
            TemporalLayerIndex: 0,
            List0ReferenceFramesCount: if key { 0 } else { 1 },
            pList0ReferenceFrames: if key { std::ptr::null_mut() } else { list0.as_mut_ptr() },
            List1ReferenceFramesCount: 0,
            pList1ReferenceFrames: std::ptr::null_mut(),
            ReferenceFramesReconPictureDescriptorsCount: descs.len() as u32,
            pReferenceFramesReconPictureDescriptors: if key { std::ptr::null_mut() } else { descs.as_mut_ptr() },
            List0RefPicModificationsCount: 0,
            pList0RefPicModifications: std::ptr::null_mut(),
            List1RefPicModificationsCount: 0,
            pList1RefPicModifications: std::ptr::null_mut(),
            QPMapValuesCount: 0,
            pRateControlQPMap: std::ptr::null_mut(),
        };
        let mut gop = D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC { GOPLength: 0, PPicturePeriod: 0, log2_max_pic_order_cnt_lsb_minus4: POC_BITS - 4 };
        let cbr = cbr_params(self.bitrate, self.fps, self.vbv);
        let mut flags = D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_NONE;
        if self.first {
            flags = first_frame_flags(self.caps.support_flags);
        } else if self.rate_change {
            flags = D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_RATE_CONTROL_CHANGE;
        }
        let mut profile = D3D12_VIDEO_ENCODER_PROFILE_HEVC_MAIN10;
        let res = D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC { Width: p.coded_w, Height: p.coded_h };

        // SAFETY: the command list is recorded and run with arguments that live until the
        // wait below; the resources are ours.
        let (written, qp) = unsafe {
            self.allocator.Reset()?;
            self.list.Reset(&self.allocator)?;
            let mut before = Vec::new();
            before.push(transition(&self.input, 0, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ));
            before.push(transition(&self.input, 1, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ));
            let mut set = |slice: usize, to: D3D12_RESOURCE_STATES, b: &mut Vec<D3D12_RESOURCE_BARRIER>, states: &mut Vec<D3D12_RESOURCE_STATES>| {
                if states[slice] != to {
                    let n = DPB as u32 + 1;
                    b.push(transition(&self.recon, slice as u32, states[slice], to));
                    b.push(transition(&self.recon, slice as u32 + n, states[slice], to));
                    states[slice] = to;
                }
            };
            for &i in &refs {
                set(self.pics[i].slice, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ, &mut before, &mut self.recon_state);
            }
            set(work, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE, &mut before, &mut self.recon_state);
            before.push(transition(&self.meta, 0, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE));
            before.push(transition(&self.bitstream, 0, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE));
            before.push(transition(&self.resolved, 0, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE));
            self.list.ResourceBarrier(&before);

            let input_args = D3D12_VIDEO_ENCODER_ENCODEFRAME_INPUT_ARGUMENTS {
                SequenceControlDesc: D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_DESC {
                    Flags: flags,
                    IntraRefreshConfig: D3D12_VIDEO_ENCODER_INTRA_REFRESH { Mode: D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE, IntraRefreshDuration: 0 },
                    RateControl: rate_control(&cbr, self.fps, self.vbv),
                    PictureTargetResolution: res,
                    SelectedLayoutMode: D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME,
                    FrameSubregionsLayoutData: Default::default(),
                    CodecGopSequence: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE { DataSize: std::mem::size_of_val(&gop) as u32, Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 { pHEVCGroupOfPictures: &mut gop } },
                },
                PictureControlDesc: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC {
                    IntraRefreshFrameIndex: 0,
                    Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_FLAG_USED_AS_REFERENCE_PICTURE,
                    PictureControlCodecData: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA { DataSize: std::mem::size_of_val(&data) as u32, Anonymous: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_0 { pHEVCPicData: &mut data } },
                    ReferenceFrames: D3D12_VIDEO_ENCODE_REFERENCE_FRAMES {
                        NumTexture2Ds: textures.len() as u32,
                        ppTexture2Ds: if key { std::ptr::null_mut() } else { textures.as_mut_ptr() },
                        pSubresources: if key { std::ptr::null_mut() } else { subs.as_mut_ptr() },
                    },
                },
                pInputFrame: ManuallyDrop::new(Some(self.input.clone())),
                InputFrameSubresource: 0,
                CurrentFrameBitstreamMetadataSize: 0,
            };
            let output_args = D3D12_VIDEO_ENCODER_ENCODEFRAME_OUTPUT_ARGUMENTS {
                Bitstream: D3D12_VIDEO_ENCODER_COMPRESSED_BITSTREAM { pBuffer: ManuallyDrop::new(Some(self.bitstream.clone())), FrameStartOffset: 0 },
                ReconstructedPicture: D3D12_VIDEO_ENCODER_RECONSTRUCTED_PICTURE { pReconstructedPicture: ManuallyDrop::new(Some(self.recon.clone())), ReconstructedPictureSubresource: work as u32 },
                EncoderOutputMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER { pBuffer: ManuallyDrop::new(Some(self.meta.clone())), Offset: 0 },
            };
            self.list.EncodeFrame(&self.encoder, &self.heap, &input_args, &output_args);

            let mut after = vec![transition(&self.meta, 0, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ)]; 
            after.push(transition(&self.input, 0, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ, D3D12_RESOURCE_STATE_COMMON));
            after.push(transition(&self.input, 1, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ, D3D12_RESOURCE_STATE_COMMON));
            self.list.ResourceBarrier(&after);
            let resolve_in = D3D12_VIDEO_ENCODER_RESOLVE_METADATA_INPUT_ARGUMENTS {
                EncoderCodec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
                EncoderProfile: D3D12_VIDEO_ENCODER_PROFILE_DESC { DataSize: 4, Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 { pHEVCProfile: &mut profile } },
                EncoderInputFormat: DXGI_FORMAT_P010,
                EncodedPictureEffectiveResolution: res,
                HWLayoutMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER { pBuffer: ManuallyDrop::new(Some(self.meta.clone())), Offset: 0 },
            };
            let resolve_out = D3D12_VIDEO_ENCODER_RESOLVE_METADATA_OUTPUT_ARGUMENTS {
                ResolvedLayoutMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER { pBuffer: ManuallyDrop::new(Some(self.resolved.clone())), Offset: 0 },
            };
            self.list.ResolveEncoderOutputMetadata(&resolve_in, &resolve_out);
            let last = [
                transition(&self.meta, 0, D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ, D3D12_RESOURCE_STATE_COMMON),
                transition(&self.bitstream, 0, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE, D3D12_RESOURCE_STATE_COMMON),
                transition(&self.resolved, 0, D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE, D3D12_RESOURCE_STATE_COMMON),
            ];
            self.list.ResourceBarrier(&last);
            if let Err(e) = self.list.Close() {
                if let Ok(q) = self.device.cast::<ID3D12InfoQueue>() {
                    for i in 0..q.GetNumStoredMessages() {
                        let mut len = 0;
                        let _ = q.GetMessage(i, None, &mut len);
                        let mut buf = vec![0u8; len];
                        let m = buf.as_mut_ptr() as *mut D3D12_MESSAGE;
                        if q.GetMessage(i, Some(m), &mut len).is_ok() {
                            eprintln!("D3D12: {}", std::ffi::CStr::from_ptr((*m).pDescription as *const _).to_string_lossy());
                        }
                    }
                }
                return Err(anyhow::Error::from(e).context("closing the encode command list (the driver refused the EncodeFrame arguments)"));
            }

            // Wait for the converter's copy, run, and wait for the end.
            let shared_fence = self.shared_fence.clone();
            self.queue.Wait(&shared_fence, self.shared_value)?;
            self.queue.ExecuteCommandLists(&[Some(self.list.cast()?)]);
            self.waiter.value += 1;
            self.queue.Signal(&self.waiter.fence, self.waiter.value)?;
            self.waiter.wait()?;

            for b in before.into_iter().chain(after).chain(last) {
                drop_barrier(b);
            }
            let mut a = input_args;
            ManuallyDrop::drop(&mut a.pInputFrame);
            let mut o = output_args;
            ManuallyDrop::drop(&mut o.Bitstream.pBuffer);
            ManuallyDrop::drop(&mut o.ReconstructedPicture.pReconstructedPicture);
            ManuallyDrop::drop(&mut o.EncoderOutputMetadata.pBuffer);
            let mut ri = resolve_in;
            ManuallyDrop::drop(&mut ri.HWLayoutMetadata.pBuffer);
            let mut ro = resolve_out;
            ManuallyDrop::drop(&mut ro.ResolvedLayoutMetadata.pBuffer);

            let mut mapped = std::ptr::null_mut();
            self.resolved.Map(0, None, Some(&mut mapped))?;
            let md = *(mapped as *const D3D12_VIDEO_ENCODER_OUTPUT_METADATA);
            self.resolved.Unmap(0, None);
            ensure!(md.EncodeErrorFlags == 0, "the encoder reports error flags {:#x}", md.EncodeErrorFlags);
            (md.EncodedBitstreamWrittenBytesCount, md.EncodeStats.AverageQP as u32)
        };
        self.bitstream_state = D3D12_RESOURCE_STATE_COMMON;
        self.resolved_state = D3D12_RESOURCE_STATE_COMMON;
        ensure!(written > 0 && written <= self.bitstream_size, "the encoder wrote {written} bytes");
        self.last_qp = qp;

        self.out.clear();
        if key {
            self.out.extend(self.params.sets());
        }
        // SAFETY: the buffer's first `written` bytes hold the frame.
        unsafe {
            let mut mapped = std::ptr::null_mut();
            let range = D3D12_RANGE { Begin: 0, End: written as usize };
            self.bitstream.Map(0, Some(&range), Some(&mut mapped))?;
            self.out.extend_from_slice(std::slice::from_raw_parts(mapped as *const u8, written as usize));
            self.bitstream.Unmap(0, None);
        }
        self.first = false;
        self.rate_change = false;
        self.last_gpu_us = started.elapsed().as_micros() as u64;

        // The new picture joins the others; the oldest or an aged one goes.
        self.pics.push(Pic { poc: self.poc, n, slice: work });
        if self.pics.len() > DPB {
            let aged = self.pics.len() - 1 - RECENT;
            let every = (self.fps as u64 / 4).max(8);
            let gone = if key || self.pics[aged].n.saturating_sub(self.last_anchor) >= every {
                self.last_anchor = self.pics[aged].n;
                0
            } else {
                aged
            };
            let pic = self.pics.remove(gone);
            self.free.push(pic.slice);
        }
        Ok(key)
    }
}

fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) => v == "1",
        Err(_) => default,
    }
}

impl FrameEncoder for Encoder {
    fn note(&self) -> &str {
        &self.note
    }

    fn short_term(&self) -> u32 {
        // Any loss that leaves a picture older than it is answered without a keyframe (the
        // anchors reach back about half a second); `invalidate` says when none is left.
        1 << 20
    }

    fn long_term(&self) -> u32 {
        0
    }

    fn encode_how(&mut self, idx: usize, timestamp: u64, how: How) -> Result<Made<'_>> {
        ensure!(idx == 0, "this encoder has the one input");
        let key = self.encode_frame(timestamp, how.idr)?;
        Ok(Made { data: &self.out, key, marked: None, referred: 0, qp: self.last_qp })
    }

    fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        ensure!(self.reconfigurable, "this encoder's rate can't change without a keyframe");
        self.bitrate = bitrate;
        self.rate_change = true;
        Ok(())
    }

    fn invalidate(&mut self, timestamp: u64) -> Result<()> {
        if let Some(i) = self.pics.iter().position(|p| p.n == timestamp) {
            let pic = self.pics.remove(i);
            self.free.push(pic.slice);
        }
        ensure!(!self.pics.is_empty(), "no picture left to refer to");
        Ok(())
    }
}

#[allow(dead_code)]
fn unused() -> Result<()> {
    bail!("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_frame_asks_only_for_changes_the_driver_can_make() {
        // Intel's Arc B580 (in a guest with its card shared): it can change the resolution, and not
        // the rate control, the GOP or the frame's layout.
        let arc = first_frame_flags(0x174d).0;
        assert_eq!(arc, D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_RESOLUTION_CHANGE.0);
        // AMD's Radeon: it can change all four, and is asked to as before.
        let radeon = first_frame_flags(0x1daf).0;
        assert_eq!(
            radeon,
            D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_RESOLUTION_CHANGE.0
                | D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_RATE_CONTROL_CHANGE.0
                | D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_GOP_SEQUENCE_CHANGE.0
                | D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_SUBREGION_LAYOUT_CHANGE.0
        );
    }
}
