//! The draw thread: a flip-model swap chain on the window, and on it, at each refresh of the
//! display, the newest decoded picture with the interface over it.
//!
//! The window has no surface of Windows' own. What is on it is two layers put together by the
//! desktop's compositor (DirectComposition): below, the picture, in a swap chain of 10 bits a
//! channel that is only there while a session is, and only where the picture goes; above it,
//! the interface, which Direct2D draws straight into a second swap chain of 8 bits a channel
//! with transparency (Direct2D doesn't draw into a 10-bit buffer). Where neither has anything,
//! the window's backdrop shows: Mica. The picture's own way to the screen is as it was: the
//! decoder's texture sampled straight into the 10-bit buffer. All of this, Direct2D included,
//! happens on this thread, so the interface never holds the device's lock from another.
//!
//! The swap chain holds one picture in waiting at most and says, through a handle to wait on,
//! when it has room for the next: about once a refresh of the display. This thread waits for
//! that and then draws whatever is newest; what the decoder finished meanwhile and was replaced
//! is never seen. The waiting is done on the handle and not inside `Present`, which would hold
//! the device's lock all the while and keep the decoder out: decoding would then go at the
//! display's rate instead of the stream's (seen: 60 a second decoded of a 120 Hz stream).
//! The picture is two planes (Y, and CbCr interleaved) of one slice of the decoder's texture,
//! or, for a picture decoded on the CPU, three textures of its own (Y, Cb, Cr); a pixel shader
//! turns limited-range BT.709 into RGB, or BT.2020 PQ into PQ (an HDR screen) or into SDR.
//! Presenting is always at the display's refresh: variable refresh was measured not to work
//! for the swap chains made here (see `chain_flags`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{DXGI_STATUS_OCCLUDED, HANDLE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_SRV_DIMENSION_TEXTURE2DARRAY, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::core::{Interface, PCSTR};

use crate::decode::Picture;
use crate::gpu::Gpu;
use crate::stream::{Scene, unpack};
use crate::ui::paint::Painter;

/// A triangle that covers the viewport, made from the vertex number alone, and the conversion.
/// The planes are sampled as array textures because that is what the decoder's texture is.
const SHADER: &str = r"
struct Corner { float4 pos : SV_Position; float2 uv : TEXCOORD0; };

Corner vs(uint n : SV_VertexID) {
    Corner c;
    c.uv = float2((n << 1) & 2, n & 2);
    c.pos = float4(c.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return c;
}

Texture2DArray<float> luma : register(t0);
Texture2DArray<float2> chroma : register(t1);
// A picture decoded on the CPU has Cb and Cr apart: Cb in `chroma`'s first channel, Cr here.
Texture2DArray<float> cr : register(t2);
SamplerState smooth : register(s0);

// `visible`: the part of the texture that is picture (it is the coded size, a little larger),
// and `last`, the last place to sample so that nothing beyond the edge is blended in.
// `scale` turns a normalised sample into a code value at the picture's bit depth; `black`,
// `yrange`, `mid` and `crange` are the code values of the range's ends.
// `mode`: 0 BT.709 (SDR, as before); 1 BT.2020 with PQ, passed through to a swap chain that is
// itself PQ BT.2020 (an HDR screen); 2 the same tone-mapped to SDR (a screen that isn't HDR).
// `white` and `peak`: where the stream's SDR white and brightest sit, in nits. `planar`: Cr
// is in a texture of its own.
cbuffer Picture : register(b0) {
    float2 visible;
    float2 last;
    float scale, black, yrange, mid;
    float crange;
    float mode, white, peak;
    float planar, unused1, unused2, unused3;
};

float3 pq_nits(float3 e) {
    float3 p = pow(saturate(e), 1.0 / 78.84375);
    return 10000.0 * pow(max(p - 0.8359375, 0) / (18.8515625 - 18.6875 * p), 1.0 / 0.1593017578125);
}

float4 ps(Corner c) : SV_Target {
    float3 at = float3(min(c.uv * visible, last), 0);
    float y = (luma.Sample(smooth, at) * scale - black) / yrange;
    float2 raw = planar > 0.5 ? float2(chroma.Sample(smooth, at).x, cr.Sample(smooth, at)) : chroma.Sample(smooth, at);
    float2 cc = (raw * scale - mid) / crange;
    if (mode < 0.5) {
        // Limited-range BT.709, the same numbers as the Linux viewer.
        return float4(y + 1.5748 * cc.y, y - 0.1873 * cc.x - 0.4681 * cc.y, y + 1.8556 * cc.x, 1);
    }
    // The BT.2020 (non-constant luminance) matrix: R'G'B' in PQ.
    float3 e = saturate(float3(y + 1.4746 * cc.y, y - 0.16455 * cc.x - 0.57135 * cc.y, y + 1.8814 * cc.x));
    if (mode < 1.5) return float4(e, 1);
    // To SDR: linear light with SDR white at 1, BT.2020 to BT.709 primaries, the highlights
    // brought down so that the stream's peak lands at white (extended Reinhard on the largest
    // channel, which keeps hues), then the screen's gamma.
    float3 lin = pq_nits(e) / white;
    lin = max(mul(float3x3(1.6605, -0.5876, -0.0728, -0.1246, 1.1329, -0.0083, -0.0182, -0.1006, 1.1187), lin), 0);
    float m = max(max(lin.r, lin.g), max(lin.b, 1e-6));
    float w = max(peak / white, 1.0);
    float k = (m * (1 + m / (w * w)) / (1 + m)) / m;
    return float4(pow(saturate(lin * k), 1.0 / 2.2), 1);
}
";

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>> {
    let (mut code, mut errors): (Option<ID3DBlob>, Option<ID3DBlob>) = (None, None);
    // SAFETY: the source and names live through the call; blobs are read within their size.
    unsafe {
        let done = D3DCompile(SHADER.as_ptr() as *const _, SHADER.len(), None, None, None, entry, target, 0, 0, &mut code, Some(&mut errors));
        if let Err(e) = done {
            let said = errors.map(|b| String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).into_owned()).unwrap_or_default();
            bail!("compile the shader: {e}: {said}");
        }
        let code = code.context("no shader code")?;
        Ok(std::slice::from_raw_parts(code.GetBufferPointer() as *const u8, code.GetBufferSize()).to_vec())
    }
}

/// The two planes of one slice, as the shader reads them; or three, for a picture decoded on
/// the CPU (Cb in `chroma`, Cr in `cr`).
#[derive(Clone)]
struct Planes {
    luma: ID3D11ShaderResourceView,
    chroma: ID3D11ShaderResourceView,
    cr: Option<ID3D11ShaderResourceView>,
}

/// Where the picture goes: a window of our own, with the interface drawn in a layer over it,
/// or the WinUI 3 app's `SwapChainPanel`, which has been given the swap chain already and
/// draws everything else itself.
pub enum Surface {
    Window(HWND),
    #[allow(dead_code)]
    Panel(IDXGISwapChain1),
    /// The engine's full-screen window (`full.rs`): the picture in an HWND flip swap chain
    /// that may tear, so a monitor that varies its refresh can follow the frames, and the
    /// interface in a layer over it only while there is something of it to show.
    #[allow(dead_code)]
    Full(HWND),
}

/// Full screen's presenting: the window's swap chain is on a device of its own, which does
/// nothing but present (measured: on the device that decodes, or that also presents into the
/// panel's swap chain, the desktop shows it as an overlay at the monitor's fixed rate, and the
/// monitor doesn't vary its refresh). The picture is drawn on the engine's device into `stage`,
/// which both devices open; each holds its keyed mutex while it uses it (key 0: the engine
/// draws, key 1: it is copied into the window's back buffer).
struct Presenter {
    gpu: Gpu,
    stage: ID3D11Texture2D,
    stage_lock: IDXGIKeyedMutex,
    shared: ID3D11Texture2D,
    shared_lock: IDXGIKeyedMutex,
}

impl Presenter {
    /// The stage, `width`×`height` in `format`, made on the engine's device and opened on the
    /// presenting one.
    fn stage(engine: &Gpu, presenter: Gpu, width: u32, height: u32, format: DXGI_FORMAT) -> Result<Self> {
        // SAFETY: Direct3D calls on interfaces held here, with a description that lives through
        // each call.
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32,
            };
            let mut stage = None;
            engine.device.CreateTexture2D(&desc, None, Some(&mut stage)).context("the picture's stage")?;
            let stage: ID3D11Texture2D = stage.context("no stage")?;
            let handle = stage.cast::<IDXGIResource>()?.GetSharedHandle()?;
            let mut shared: Option<ID3D11Texture2D> = None;
            presenter.device.OpenSharedResource(handle, &mut shared).context("open the stage on the presenting device")?;
            let shared = shared.context("no stage on the presenting device")?;
            Ok(Self { stage_lock: stage.cast()?, shared_lock: shared.cast()?, stage, shared, gpu: presenter })
        }
    }

    /// Copy the stage into the swap chain's back buffer, on the presenting device.
    fn copy(&self, swap: &IDXGISwapChain1) -> Result<()> {
        // SAFETY: calls on interfaces held here; the keyed mutex is taken and given back.
        unsafe {
            self.shared_lock.AcquireSync(1, 1000).context("the stage's turn")?;
            let back: ID3D11Texture2D = swap.GetBuffer(0)?;
            self.gpu.context.CopyResource(&back, &self.shared);
            self.shared_lock.ReleaseSync(0)?;
        }
        Ok(())
    }
}
/// The window's layers above the picture, and the compositor's side (none for a panel).
struct Layers {
    painter: Painter,
    /// The interface's layer, and what Direct2D draws into it through (made anew when the
    /// window changes size).
    ui_swap: IDXGISwapChain1,
    ui_target: Option<windows::Win32::Graphics::Direct2D::ID2D1Bitmap1>,
    /// The device, the window's target (kept alive), and the picture's layer (none in full
    /// screen, where the picture is the window's own swap chain).
    dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    video_visual: Option<IDCompositionVisual>,
    /// The interface's layer and what it hangs from; in full screen it hangs there only while
    /// it shows something (`ui_on`).
    root: IDCompositionVisual,
    ui_visual: IDCompositionVisual,
    ui_on: bool,
}

struct Drawer {
    gpu: Gpu,
    swap: IDXGISwapChain1,
    /// Signalled when the swap chain has room for another picture.
    room: HANDLE,
    target: Option<ID3D11RenderTargetView>,
    /// The size the swap chain's buffers are now.
    size: (u32, u32),
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    constants: ID3D11Buffer,
    /// Views on the decoder's slices, made once each: the decoder goes round a small pool.
    views: HashMap<(usize, u32), Planes>,
    /// Set once the driver has refused a view on a decoder texture: from then on each picture
    /// is copied (on the GPU) into `own`, a texture made for sampling.
    copying: bool,
    own: Option<(ID3D11Texture2D, Planes, (u32, u32, DXGI_FORMAT))>,
    /// The picture on screen, kept for drawing again when the window or the interface changes.
    shown: Option<Picture>,
    layers: Option<Layers>,
    /// The part of the window the picture's layer is cut to (`None`: not shown at all).
    clip: Option<Option<(i32, i32, i32, i32)>>,
    /// A panel's scale (pixels to one of the app's units) the swap chain is set to undo.
    scale: (f32, f32),
    /// The swap chain is in PQ BT.2020 now (else sRGB).
    pq: bool,
    /// What the picture's swap chain was made with, and so is resized with; whether that
    /// lets it tear.
    flags: DXGI_SWAP_CHAIN_FLAG,
    tears: bool,
    cadence: Cadence,
    /// Full screen's own presenting device and the stage the picture is drawn into.
    presenter: Option<Presenter>,
}

/// Whether DXGI lets this device's swap chains present with tearing allowed, which variable
/// refresh needs: a monitor that varies its refresh can then show a picture the moment it is
/// presented instead of at its next refresh.
pub fn tearing_supported(gpu: &Gpu) -> bool {
    // SAFETY: a DXGI query into a value that lives through the call.
    unsafe {
        let Ok(factory) = gpu.device.cast::<IDXGIDevice>().and_then(|d| d.GetAdapter()).and_then(|a| a.GetParent::<IDXGIFactory5>()) else { return false };
        let mut allowed = windows::core::BOOL(0);
        factory.CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING, &mut allowed as *mut _ as *mut _, 4).is_ok() && allowed.as_bool()
    }
}

/// The flags the picture's swap chain is made with, and resized with (they must stay the same).
/// Not allowed to tear: a swap chain for composition (the panel's, the old window's layer) was
/// measured not to make a monitor vary its refresh even in full screen with tearing allowed
/// (an RTX 5090 and an ASUS XG32UCWMG at 240 Hz: its vertical blanks stayed 4.17 ms apart for
/// pictures presented 100 a second, where an HWND swap chain's followed them, 10 ms apart).
/// Full screen's HWND swap chain (`Surface::Full`) is made on its own, and may tear.
fn chain_flags(_gpu: &Gpu) -> DXGI_SWAP_CHAIN_FLAG {
    WAITABLE
}

/// The picture's swap chain as it is made for a window's layer or for a panel.
fn picture_chain(gpu: &Gpu, width: u32, height: u32) -> Result<IDXGISwapChain1> {
    // SAFETY: DXGI calls on interfaces held here, with a description that lives through each.
    unsafe {
        let factory: IDXGIFactory2 = gpu.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
        let mut desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width.max(8),
            Height: height.max(8),
            Format: DXGI_FORMAT_R10G10B10A2_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: chain_flags(gpu).0 as u32,
            ..Default::default()
        };
        // The stream is 10-bit, so the picture's buffers are too where that is allowed.
        Ok(match factory.CreateSwapChainForComposition(&gpu.device, &desc, None) {
            Ok(swap) => swap,
            Err(e) => {
                say!("no 10-bit swap chain ({e}): 8-bit");
                desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
                factory.CreateSwapChainForComposition(&gpu.device, &desc, None).context("create the picture's swap chain")?
            }
        })
    }
}

/// A swap chain for a panel of the WinUI 3 app; the app's thread hands it to the panel.
#[allow(dead_code)]
pub fn panel_chain(gpu: &Gpu, width: u32, height: u32) -> Result<IDXGISwapChain1> {
    say!("tearing: DXGI {} it", if tearing_supported(gpu) { "allows" } else { "doesn't allow" });
    picture_chain(gpu, width, height)
}

/// How the pictures presented lately reached the screen: whether the desktop's compositor drew
/// them or they were shown by themselves (independent flip or an overlay, which variable
/// refresh needs), and when each was shown, from the swap chain's own figures. A monitor that
/// varies its refresh shows pictures sent at an odd rate when they come; one at a fixed rate
/// only on its refreshes, so the gaps between them are whole refreshes.
#[derive(Default)]
struct Cadence {
    /// The present count and the time it was shown (QPC ticks), for each picture shown.
    shown: Vec<(u32, i64)>,
    presents: u32,
    since: Option<Instant>,
    failed: bool,
}

impl Cadence {
    /// After a present: note what the swap chain says was last shown.
    fn note(&mut self, swap: &IDXGISwapChain1) {
        self.presents += 1;
        self.since.get_or_insert_with(Instant::now);
        let mut stats = DXGI_FRAME_STATISTICS::default();
        // SAFETY: a call on the swap chain into a structure that lives through it.
        match unsafe { swap.GetFrameStatistics(&mut stats) } {
            Ok(()) if stats.SyncQPCTime != 0 => {
                if self.shown.last().is_none_or(|l| l.0 != stats.PresentCount) {
                    self.shown.push((stats.PresentCount, stats.SyncQPCTime));
                }
            }
            Ok(()) => {}
            // The figures start again (the first present, a mode change): no gap across it.
            Err(e) if e.code() == DXGI_ERROR_FRAME_STATISTICS_DISJOINT => self.shown.clear(),
            Err(e) => {
                if !self.failed {
                    say!("presenting: no frame statistics from the swap chain ({e})");
                    self.failed = true;
                }
            }
        }
    }

    /// Every few seconds: a line for the log, and start again. (The gaps are only worth
    /// anything for a picture shown by itself: for one drawn by the compositor they are the
    /// times of presenting, not of showing. `monitor::watch_vblanks` is what to go by.)
    fn report(&mut self, swap: &IDXGISwapChain1, refresh_hz: u32, tearing: bool) -> Option<String> {
        if self.since.is_none_or(|s| s.elapsed() < Duration::from_secs(5)) {
            return None;
        }
        let seconds = self.since.take().unwrap().elapsed().as_secs_f64();
        // SAFETY: plain queries.
        let (mode, frequency) = unsafe {
            let mut media = DXGI_FRAME_STATISTICS_MEDIA::default();
            let mode = match swap.cast::<IDXGISwapChainMedia>().and_then(|m| m.GetFrameStatisticsMedia(&mut media)) {
                Ok(()) => match media.CompositionMode {
                    DXGI_FRAME_PRESENTATION_MODE_COMPOSED => "drawn by the desktop's compositor".to_owned(),
                    DXGI_FRAME_PRESENTATION_MODE_OVERLAY => "shown by itself (independent flip or an overlay)".to_owned(),
                    DXGI_FRAME_PRESENTATION_MODE_NONE => "not shown".to_owned(),
                    other => format!("composition failed ({})", other.0),
                },
                Err(e) => format!("composition not known ({e})"),
            };
            let mut f = 0i64;
            let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f);
            (mode, f.max(1) as f64)
        };
        // The gaps between pictures shown one after another, and how many of them are whole
        // refreshes of the display (within half a millisecond).
        let gaps: Vec<f64> = self.shown.windows(2).filter(|w| w[1].0 == w[0].0 + 1).map(|w| (w[1].1 - w[0].1) as f64 / frequency * 1000.0).collect();
        let period = if refresh_hz > 0 { 1000.0 / refresh_hz as f64 } else { 0.0 };
        let whole = gaps.iter().filter(|g| period > 0.0 && ((**g / period).round() * period - **g).abs() < 0.5).count();
        let mean = if gaps.is_empty() { 0.0 } else { gaps.iter().sum::<f64>() / gaps.len() as f64 };
        let line = format!(
            "{} presented in {seconds:.1} s{}, {mode}; {} gaps between pictures shown, {mean:.2} ms on average, {} of them whole refreshes of {period:.2} ms",
            self.presents,
            if tearing { " with tearing allowed" } else { " at the display's refresh" },
            gaps.len(),
            whole
        );
        self.shown.clear();
        self.presents = 0;
        Some(line)
    }
}

fn plane_formats(format: DXGI_FORMAT) -> Result<(DXGI_FORMAT, DXGI_FORMAT)> {
    Ok(match format {
        DXGI_FORMAT_P010 => (DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM),
        DXGI_FORMAT_NV12 => (DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM),
        other => bail!("the decoder's pictures are format {}, which isn't drawn yet", other.0),
    })
}

impl Drawer {
    fn new(gpu: Gpu, surface: Surface) -> Result<Self> {
        // SAFETY: Direct3D and DXGI calls on interfaces held here, with structures that live
        // through each call.
        unsafe {
            let mut full = None::<Presenter>;
            let (swap, layers) = match surface {
                Surface::Panel(swap) => (swap, None),
                Surface::Window(hwnd) => {
                    let factory: IDXGIFactory2 = gpu.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
                    let mut client = windows::Win32::Foundation::RECT::default();
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
                    let (width, height) = (client.right.max(8) as u32, client.bottom.max(8) as u32);
                    let swap = picture_chain(&gpu, width, height)?;
                    // The interface's: Direct2D's own format, transparent where nothing is drawn.
                    let ui_desc = DXGI_SWAP_CHAIN_DESC1 {
                        Width: width,
                        Height: height,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 2,
                        Scaling: DXGI_SCALING_STRETCH,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                        Flags: 0,
                        ..Default::default()
                    };
                    let ui_swap = factory.CreateSwapChainForComposition(&gpu.device, &ui_desc, None).context("create the interface's swap chain")?;
                    let dcomp: IDCompositionDevice = DCompositionCreateDevice(&gpu.device.cast::<IDXGIDevice>()?).context("no DirectComposition")?;
                    let target = dcomp.CreateTargetForHwnd(hwnd, true).context("a composition target for the window")?;
                    let (root, video_visual, ui_visual) = (dcomp.CreateVisual()?, dcomp.CreateVisual()?, dcomp.CreateVisual()?);
                    video_visual.SetContent(&swap)?;
                    ui_visual.SetContent(&ui_swap)?;
                    root.AddVisual(&video_visual, false, None)?;
                    root.AddVisual(&ui_visual, true, &video_visual)?;
                    target.SetRoot(&root)?;
                    dcomp.Commit()?;
                    let painter = Painter::new(&gpu.device)?;
                    say!("interface: Direct2D on the same device, in a layer of its own over the picture's; text in {}", painter.family());
                    (swap, Some(Layers { painter, ui_swap, ui_target: None, dcomp, _target: target, video_visual: Some(video_visual), root, ui_visual, ui_on: true }))
                }
                Surface::Full(hwnd) => {
                    let presenting = Gpu::presenter()?;
                    let factory: IDXGIFactory2 = presenting.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
                    let mut client = windows::Win32::Foundation::RECT::default();
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
                    let (width, height) = (client.right.max(8) as u32, client.bottom.max(8) as u32);
                    let tears = tearing_supported(&gpu);
                    // 10-bit, as the stream is: measured to vary the refresh as 8-bit does, in sRGB
                    // and in PQ, while the window has the keyboard. (REMOTEMAN_FULL_FORMAT=8: 8-bit,
                    // for measuring.)
                    let eight = std::env::var("REMOTEMAN_FULL_FORMAT").as_deref() == Ok("8");
                    let mut desc = DXGI_SWAP_CHAIN_DESC1 {
                        Width: width,
                        Height: height,
                        Format: if eight { DXGI_FORMAT_B8G8R8A8_UNORM } else { DXGI_FORMAT_R10G10B10A2_UNORM },
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 2,
                        Scaling: DXGI_SCALING_STRETCH,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                        Flags: (WAITABLE.0 | if tears { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 } else { 0 }) as u32,
                        ..Default::default()
                    };
                    let swap = match factory.CreateSwapChainForHwnd(&presenting.device, hwnd, &desc, None, None) {
                        Ok(swap) => swap,
                        Err(e) => {
                            say!("no 10-bit swap chain for the window ({e}): 8-bit");
                            desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
                            factory.CreateSwapChainForHwnd(&presenting.device, hwnd, &desc, None, None).context("create the full-screen swap chain")?
                        }
                    };
                    // Alt+Enter is no business of DXGI's here.
                    let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
                    full = Some(Presenter::stage(&gpu, presenting, width, height, desc.Format)?);
                    let factory: IDXGIFactory2 = gpu.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
                    let ui_desc = DXGI_SWAP_CHAIN_DESC1 {
                        Width: width,
                        Height: height,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 2,
                        Scaling: DXGI_SCALING_STRETCH,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                        Flags: 0,
                        ..Default::default()
                    };
                    let ui_swap = factory.CreateSwapChainForComposition(&gpu.device, &ui_desc, None).context("create the interface's swap chain")?;
                    let dcomp: IDCompositionDevice = DCompositionCreateDevice(&gpu.device.cast::<IDXGIDevice>()?).context("no DirectComposition")?;
                    let target = dcomp.CreateTargetForHwnd(hwnd, true).context("a composition target for the window")?;
                    let (root, ui_visual) = (dcomp.CreateVisual()?, dcomp.CreateVisual()?);
                    ui_visual.SetContent(&ui_swap)?;
                    // The interface's layer is hung on once there is something to show.
                    target.SetRoot(&root)?;
                    dcomp.Commit()?;
                    let painter = Painter::new(&gpu.device)?;
                    say!("full screen: the picture in the window's own swap chain (HWND, flip model{}), the interface in a layer over it only while it shows something", if tears { ", tearing allowed" } else { ", DXGI doesn't allow tearing" });
                    (swap, Some(Layers { painter, ui_swap, ui_target: None, dcomp, _target: target, video_visual: None, root, ui_visual, ui_on: false }))
                }
            };
            let now = swap.GetDesc1()?;
            let room = if now.Flags & WAITABLE.0 as u32 != 0 {
                let latency: IDXGISwapChain2 = swap.cast()?;
                latency.SetMaximumFrameLatency(1)?;
                let room = latency.GetFrameLatencyWaitableObject();
                anyhow::ensure!(!room.is_invalid(), "the swap chain has nothing to wait on");
                room
            } else {
                // (A swap chain made without the waitable object, for measuring: presenting
                // waits instead.)
                HANDLE::default()
            };

            let (vs_code, ps_code) = (compile(windows::core::s!("vs"), windows::core::s!("vs_5_0"))?, compile(windows::core::s!("ps"), windows::core::s!("ps_5_0"))?);
            let (mut vs, mut ps, mut sampler, mut constants) = (None, None, None, None);
            gpu.device.CreateVertexShader(&vs_code, None, Some(&mut vs))?;
            gpu.device.CreatePixelShader(&ps_code, None, Some(&mut ps))?;
            let s = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            gpu.device.CreateSamplerState(&s, Some(&mut sampler))?;
            let c = D3D11_BUFFER_DESC { ByteWidth: 64, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
            gpu.device.CreateBuffer(&c, None, Some(&mut constants))?;
            let flags = DXGI_SWAP_CHAIN_FLAG(now.Flags as i32);
            let tears = flags.0 & DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 != 0;
            say!(
                "swap chain: {}x{}, {}, flip model{}{}",
                now.Width,
                now.Height,
                if now.Format == DXGI_FORMAT_R10G10B10A2_UNORM { "10 bits a channel" } else { "8 bits a channel" },
                if layers.is_none() { ", in the app's panel" } else { "" },
                if tears { ", may tear" } else { "" }
            );
            Ok(Self {
                gpu,
                swap,
                room,
                target: None,
                size: (now.Width, now.Height),
                vs: vs.context("no vertex shader")?,
                ps: ps.context("no pixel shader")?,
                sampler: sampler.context("no sampler")?,
                constants: constants.context("no constant buffer")?,
                views: HashMap::new(),
                copying: false,
                own: None,
                shown: None,
                layers,
                clip: None,
                scale: (1.0, 1.0),
                pq: false,
                flags,
                tears,
                cadence: Cadence::default(),
                presenter: full,
            })
        }
    }

    /// Views on one slice of a P010 or NV12 array texture.
    fn planes_of(&self, texture: &ID3D11Texture2D, slice: u32, format: DXGI_FORMAT) -> Result<Planes> {
        let (luma, chroma) = plane_formats(format)?;
        let view = |format| -> Result<ID3D11ShaderResourceView> {
            let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: format,
                ViewDimension: D3D_SRV_DIMENSION_TEXTURE2DARRAY,
                Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2DArray: D3D11_TEX2D_ARRAY_SRV { MostDetailedMip: 0, MipLevels: 1, FirstArraySlice: slice, ArraySize: 1 } },
            };
            let mut view = None;
            // SAFETY: the description lives through the call.
            unsafe { self.gpu.device.CreateShaderResourceView(texture, Some(&desc), Some(&mut view))? };
            view.context("no view")
        };
        Ok(Planes { luma: view(luma)?, chroma: view(chroma)?, cr: None })
    }

    /// What the shader should read for this picture: the decoder's own slice, or a copy of it.
    fn planes(&mut self, picture: &Picture) -> Result<(Planes, D3D11_TEXTURE2D_DESC)> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: the out pointer is valid.
        unsafe { picture.texture.GetDesc(&mut desc) };
        if let Some((cb, cr)) = &picture.chroma {
            // Decoded on the CPU: two plain textures, made for sampling.
            let key = (picture.texture.as_raw() as usize, 0);
            if let Some(planes) = self.views.get(&key) {
                return Ok((planes.clone(), desc));
            }
            let view = |texture: &ID3D11Texture2D, format| -> Result<ID3D11ShaderResourceView> {
                let d = D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: format,
                    ViewDimension: D3D_SRV_DIMENSION_TEXTURE2DARRAY,
                    Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2DArray: D3D11_TEX2D_ARRAY_SRV { MostDetailedMip: 0, MipLevels: 1, FirstArraySlice: 0, ArraySize: 1 } },
                };
                let mut view = None;
                // SAFETY: the description lives through the call.
                unsafe { self.gpu.device.CreateShaderResourceView(texture, Some(&d), Some(&mut view))? };
                view.context("no view")
            };
            // Three plain textures, of one format (R16 or R8).
            let planes = Planes { luma: view(&picture.texture, desc.Format)?, chroma: view(cb, desc.Format)?, cr: Some(view(cr, desc.Format)?) };
            if self.views.len() > 16 {
                self.views.clear();
            }
            self.views.insert(key, planes.clone());
            return Ok((planes, desc));
        }
        if !self.copying {
            let key = (picture.texture.as_raw() as usize, picture.slice);
            if let Some(planes) = self.views.get(&key) {
                return Ok((planes.clone(), desc));
            }
            let made = if desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0 { self.planes_of(&picture.texture, picture.slice, desc.Format) } else { Err(anyhow::anyhow!("not made for sampling")) };
            match made {
                Ok(planes) => {
                    if self.views.is_empty() {
                        say!("drawing: the decoder's own textures are sampled (an array of {}, {}x{}), no copy", desc.ArraySize, desc.Width, desc.Height);
                    }
                    // A new stream brings a new texture; the old one's views are no use then.
                    if self.views.keys().any(|(texture, _)| *texture != key.0) {
                        self.views.clear();
                    }
                    self.views.insert(key, planes.clone());
                    return Ok((planes, desc));
                }
                Err(e) => {
                    say!("drawing: the decoder's textures can't be sampled ({e:#}): one copy on the GPU a picture");
                    self.copying = true;
                    self.views.clear();
                }
            }
        }
        let want = (desc.Width, desc.Height, desc.Format);
        if self.own.as_ref().is_none_or(|(_, _, have)| *have != want) {
            let d = D3D11_TEXTURE2D_DESC { ArraySize: 1, MipLevels: 1, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32, CPUAccessFlags: 0, MiscFlags: 0, ..desc };
            let mut texture = None;
            // SAFETY: the description lives through the call.
            unsafe { self.gpu.device.CreateTexture2D(&d, None, Some(&mut texture)).context("a texture to copy pictures into")? };
            let texture = texture.context("no texture")?;
            let planes = self.planes_of(&texture, 0, desc.Format)?;
            self.own = Some((texture, planes, want));
        }
        let (texture, planes, _) = self.own.as_ref().unwrap();
        // The whole slice, both planes, GPU to GPU.
        // SAFETY: both textures are alive and of one size and format.
        unsafe { self.gpu.context.CopySubresourceRegion(texture, 0, 0, 0, 0, &picture.texture, picture.slice, None) };
        Ok((planes.clone(), desc))
    }

    /// Follow the window's size.
    fn resize(&mut self, to: (u32, u32)) -> Result<bool> {
        if to == self.size || to.0 == 0 || to.1 == 0 {
            return Ok(false);
        }
        // Nothing may hold the old buffers while they are replaced.
        self.target = None;
        if let Some(layers) = &mut self.layers {
            layers.ui_target = None;
        }
        // SAFETY: calls on interfaces we hold.
        unsafe {
            self.gpu.context.OMSetRenderTargets(None, None);
            self.swap.ResizeBuffers(0, to.0, to.1, DXGI_FORMAT_UNKNOWN, self.flags).context("resize the swap chain")?;
            if let Some(p) = self.presenter.take() {
                let format = self.swap.GetDesc1()?.Format;
                self.presenter = Some(Presenter::stage(&self.gpu, p.gpu, to.0, to.1, format)?);
            }
            if let Some(layers) = &self.layers {
                layers.ui_swap.ResizeBuffers(0, to.0, to.1, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0)).context("resize the interface's swap chain")?;
            }
        }
        self.size = to;
        Ok(true)
    }

    /// A panel's swap chain is in the app's units: undo its scale, so a pixel is a pixel.
    fn set_scale(&mut self, scale: (f32, f32)) -> Result<()> {
        if self.layers.is_some() || scale == self.scale || scale.0 <= 0.0 || scale.1 <= 0.0 {
            return Ok(());
        }
        self.scale = scale;
        let m = DXGI_MATRIX_3X2_F { _11: 1.0 / scale.0, _22: 1.0 / scale.1, ..Default::default() };
        // SAFETY: a call on the swap chain with a matrix that lives through it.
        unsafe { self.swap.cast::<IDXGISwapChain2>()?.SetMatrixTransform(&m).context("set the panel's scale")? };
        Ok(())
    }

    /// The swap chain's colour space: PQ BT.2020 for an HDR stream on an HDR screen, sRGB else.
    fn set_pq(&mut self, pq: bool) {
        if pq == self.pq {
            return;
        }
        use windows::Win32::Graphics::Dxgi::Common::{DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020};
        let space = if pq { DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 } else { DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 };
        // SAFETY: calls on the swap chain.
        let done = unsafe {
            self.swap.cast::<IDXGISwapChain3>().and_then(|s3| {
                let support = s3.CheckColorSpaceSupport(space)?;
                if support & DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT.0 as u32 == 0 {
                    return Err(windows::core::Error::new(windows::core::HRESULT(-1), "not supported for presenting"));
                }
                s3.SetColorSpace1(space)
            })
        };
        let ok = done.is_ok();
        match done {
            Ok(()) => say!("swap chain: {}", if pq { "PQ BT.2020 (HDR10), passed through" } else { "sRGB" }),
            Err(e) => say!("swap chain: no {} colour space ({e}); tone-mapped to SDR instead", if pq { "PQ" } else { "sRGB" }),
        }
        self.pq = pq && ok;
    }

    /// Draw the interface into its layer's back buffer.
    fn paint(&mut self, frame: &crate::ui::Frame) -> Result<()> {
        let Some(layers) = &mut self.layers else { return Ok(()) };
        if layers.ui_target.is_none() {
            // SAFETY: a call on the swap chain; with the flip model, buffer 0 is always the
            // one being drawn, so what is made on it lasts until the size changes.
            let back: IDXGISurface = unsafe { layers.ui_swap.GetBuffer(0)? };
            layers.ui_target = Some(layers.painter.target(&back)?);
        }
        let bitmap = layers.ui_target.clone().unwrap();
        layers.painter.paint(&bitmap, frame)
    }

    /// Show the picture's layer in this part of the window only (in pixels), or not at all.
    fn place_video(&mut self, area: Option<(i32, i32, i32, i32)>) -> Result<()> {
        if self.clip != Some(area) {
            self.clip = Some(area);
            // A panel is the picture's place, all of it; in full screen the picture is drawn
            // where it goes, in the window's own swap chain.
            let Some(layers) = &self.layers else { return Ok(()) };
            let Some(video_visual) = &layers.video_visual else { return Ok(()) };
            let (x, y, w, h) = area.unwrap_or((0, 0, 0, 0));
            let r = windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: x as f32, top: y as f32, right: (x + w) as f32, bottom: (y + h) as f32 };
            // SAFETY: calls on interfaces held here; the rectangle lives through the call.
            unsafe {
                video_visual.SetClip2(&r)?;
                layers.dcomp.Commit()?;
            }
        }
        Ok(())
    }

    /// In full screen: hang the interface's layer over the picture while it shows something,
    /// and take it away while it shows nothing (the picture can then be shown by itself).
    fn show_interface(&mut self, on: bool) -> Result<()> {
        let Some(layers) = &mut self.layers else { return Ok(()) };
        if layers.video_visual.is_some() || layers.ui_on == on {
            return Ok(());
        }
        layers.ui_on = on;
        say!("full screen: the interface's layer {}", if on { "hung over the picture" } else { "taken away" });
        // SAFETY: calls on interfaces held here.
        unsafe {
            if on {
                layers.root.AddVisual(&layers.ui_visual, true, None)?;
            } else {
                layers.root.RemoveVisual(&layers.ui_visual)?;
            }
            layers.dcomp.Commit()?;
        }
        Ok(())
    }

    /// Draw the picture's back buffer: black, and the picture (if there is one and somewhere
    /// for it to go) as large as fits `video` with its shape kept.
    fn draw(&mut self, picture: Option<&Picture>, video: Option<(i32, i32, i32, i32)>, colour: (bool, f32, f32)) -> Result<()> {
        let planes = match (picture, video) {
            (Some(picture), Some(_)) => Some((self.planes(picture)?, picture)),
            _ => None,
        };
        // SAFETY: Direct3D calls on interfaces held here; everything bound outlives the draw.
        unsafe {
            if self.target.is_none() {
                // Full screen's picture is drawn into its stage; anyone else's into the back buffer.
                let back: ID3D11Texture2D = match &self.presenter {
                    Some(p) => p.stage.clone(),
                    None => self.swap.GetBuffer(0)?,
                };
                let mut target = None;
                self.gpu.device.CreateRenderTargetView(&back, None, Some(&mut target))?;
                self.target = target;
            }
            if let Some(p) = &self.presenter {
                p.stage_lock.AcquireSync(0, 1000).context("the stage's turn to be drawn")?;
            }
            let target = self.target.clone().context("no render target")?;
            let ctx = &self.gpu.context;
            // Everything is set each time: the flip model unbinds the back buffer at every
            // present, and Direct2D leaves the context as it pleases.
            ctx.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            ctx.OMSetDepthStencilState(None, 0);
            ctx.OMSetBlendState(None, None, u32::MAX);
            ctx.RSSetState(None);
            ctx.ClearRenderTargetView(&target, &[0.0, 0.0, 0.0, 1.0]);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.GSSetShader(None, None);
            ctx.HSSetShader(None, None);
            ctx.DSSetShader(None, None);
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            if let (Some(((planes, desc), picture)), Some(area)) = (planes, video) {
                // P010 keeps its 10 bits at the top of 16: a normalised sample times 65535/64 is
                // the 10-bit code. A picture decoded on the CPU has them at the bottom (R16), or
                // 8 bits (R8).
                let (scale, black, yrange, mid, crange) = match desc.Format {
                    DXGI_FORMAT_P010 => (65535.0 / 64.0, 64.0, 876.0, 512.0, 896.0),
                    DXGI_FORMAT_R16_UNORM => (65535.0, 64.0, 876.0, 512.0, 896.0),
                    _ => (255.0, 16.0, 219.0, 128.0, 224.0),
                };
                let (tw, th) = (desc.Width as f32, desc.Height as f32);
                let (vw, vh) = (picture.width as f32, picture.height as f32);
                let (hdr, white, peak) = colour;
                let mode = if !hdr { 0.0 } else if self.pq { 1.0 } else { 2.0 };
                let planar = if planes.cr.is_some() { 1.0 } else { 0.0 };
                let constants: [f32; 16] = [vw / tw, vh / th, (vw - 0.5) / tw, (vh - 0.5) / th, scale, black, yrange, mid, crange, mode, white, peak, planar, 0.0, 0.0, 0.0];
                ctx.UpdateSubresource(&self.constants, 0, None, constants.as_ptr() as *const _, 0, 0);
                let (ax, ay, aw, ah) = (area.0 as f32, area.1 as f32, area.2 as f32, area.3 as f32);
                let fit = (aw / vw).min(ah / vh);
                let (dw, dh) = (vw * fit, vh * fit);
                let viewport = D3D11_VIEWPORT { TopLeftX: (ax + (aw - dw) / 2.0).floor(), TopLeftY: (ay + (ah - dh) / 2.0).floor(), Width: dw, Height: dh, MinDepth: 0.0, MaxDepth: 1.0 };
                ctx.RSSetViewports(Some(&[viewport]));
                ctx.PSSetShader(&self.ps, None);
                ctx.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
                ctx.PSSetShaderResources(0, Some(&[Some(planes.luma), Some(planes.chroma), planes.cr]));
                ctx.Draw(3, 0);
                // Let go of the slice, so the decoder may write to it again when its turn comes.
                ctx.PSSetShaderResources(0, Some(&[None, None, None]));
            }
            // Direct2D draws with the same context: leave nothing of ours bound to it.
            ctx.OMSetRenderTargets(None, None);
            if let Some(p) = &self.presenter {
                p.stage_lock.ReleaseSync(1)?;
            }
        }
        Ok(())
    }

    /// A swap chain's back buffer copied down: its size and its pixels as they lie in memory.
    fn read(&self, swap: &IDXGISwapChain1) -> Result<(usize, usize, DXGI_FORMAT, Vec<u8>)> {
        // SAFETY: a call on the swap chain.
        self.read_texture(&unsafe { swap.GetBuffer::<ID3D11Texture2D>(0) }?)
    }

    /// The picture as it is about to be shown: the stage in full screen (it is the copied turn's
    /// until it has been copied), the back buffer otherwise.
    fn read_picture(&self) -> Result<(usize, usize, DXGI_FORMAT, Vec<u8>)> {
        match &self.presenter {
            Some(p) => {
                // SAFETY: the keyed mutex is taken and given back with the key it had.
                unsafe { p.stage_lock.AcquireSync(1, 1000)? };
                let read = self.read_texture(&p.stage);
                unsafe { p.stage_lock.ReleaseSync(1)? };
                read
            }
            None => self.read(&self.swap),
        }
    }

    fn read_texture(&self, back: &ID3D11Texture2D) -> Result<(usize, usize, DXGI_FORMAT, Vec<u8>)> {
        // SAFETY: the staging texture is mapped for reading and unmapped again; rows are read
        // within the mapped pitch and height.
        unsafe {
            let back = back.clone();
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            back.GetDesc(&mut desc);
            let d = D3D11_TEXTURE2D_DESC { Usage: D3D11_USAGE_STAGING, BindFlags: 0, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, MiscFlags: 0, ..desc };
            let mut staging = None;
            self.gpu.device.CreateTexture2D(&d, None, Some(&mut staging))?;
            let staging = staging.context("no staging texture")?;
            self.gpu.context.CopyResource(&staging, &back);
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            self.gpu.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
            let (w, h) = (desc.Width as usize, desc.Height as usize);
            let mut out = Vec::with_capacity(w * h * 4);
            for row in 0..h {
                out.extend_from_slice(std::slice::from_raw_parts((map.pData as *const u8).add(row * map.RowPitch as usize), w * 4));
            }
            self.gpu.context.Unmap(&staging, 0);
            Ok((w, h, desc.Format, out))
        }
    }

    /// The window as it is about to be shown, put together here as the compositor will: the
    /// picture where its layer shows (if `picture`), `backdrop` elsewhere, the interface over
    /// both. The only place pixels come to CPU memory.
    fn shot(&self, path: &std::path::Path, backdrop: crate::ui::Color, picture: bool) -> Result<()> {
        let under = if picture || self.layers.is_none() { Some(self.read_picture()?) } else { None };
        let (w, h, _, ui) = match &self.layers {
            Some(layers) => self.read(&layers.ui_swap)?,
            // A panel has no interface of ours over it: nothing on top.
            None => {
                let (w, h) = under.as_ref().map_or((1, 1), |u| (u.0, u.1));
                (w, h, DXGI_FORMAT_B8G8R8A8_UNORM, vec![0; w * h * 4])
            }
        };
        let clip = self.clip.flatten().unwrap_or((0, 0, 0, 0));
        let back = [(backdrop.0 * 255.0) as u8, (backdrop.1 * 255.0) as u8, (backdrop.2 * 255.0) as u8];
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let inside = (x as i32) >= clip.0 && (y as i32) >= clip.1 && (x as i32) < clip.0 + clip.2 && (y as i32) < clip.1 + clip.3;
                let below = match &under {
                    Some((vw, vh, format, px)) if inside && x < *vw && y < *vh => {
                        let j = (y * vw + x) * 4;
                        if *format == DXGI_FORMAT_R10G10B10A2_UNORM {
                            // Red in the low 10 bits, then green, then blue; the PNG gets the top 8.
                            let v = u32::from_le_bytes([px[j], px[j + 1], px[j + 2], px[j + 3]]);
                            [(v >> 2) as u8, (v >> 12) as u8, (v >> 22) as u8]
                        } else {
                            [px[j + 2], px[j + 1], px[j]]
                        }
                    }
                    _ => back,
                };
                // The interface's colours are already multiplied by its alpha.
                let keep = 255 - ui[i + 3] as u32;
                for (c, over) in [ui[i + 2], ui[i + 1], ui[i]].into_iter().enumerate() {
                    rgb.push((over as u32 + (below[c] as u32 * keep + 127) / 255).min(255) as u8);
                }
            }
        }
        crate::shot::save(path, w as u32, h as u32, &rgb)?;
        say!("shot: {} ({w}x{h}, the window's layers as they are about to be shown)", path.display());
        Ok(())
    }
}

/// Held by a draw thread while it draws. Every window's draw thread uses the one device's
/// one context, Direct2D included, and a drawing is many calls that set the context up and
/// then use it: two threads at it at once draw with each other's settings (seen as garbled
/// text in the first window as soon as a second display's window was drawing).
static DRAWING: std::sync::Mutex<()> = std::sync::Mutex::new(());

const WAITABLE: DXGI_SWAP_CHAIN_FLAG = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;

/// How long after the first picture `--shot` is taken: long enough for the host's content to
/// be up and moving.
const SHOT_AFTER: Duration = Duration::from_secs(4);

pub fn start(gpu: Gpu, scene: Arc<Scene>, hwnd: HWND, shot: Option<PathBuf>) -> Result<JoinHandle<()>> {
    let hwnd = hwnd.0 as isize;
    start_on(gpu, scene, move || Surface::Window(HWND(hwnd as *mut _)), shot)
}

/// The same for whatever surface `surface` makes on the draw thread.
pub fn start_on(gpu: Gpu, scene: Arc<Scene>, surface: impl FnOnce() -> Surface + Send + 'static, shot: Option<PathBuf>) -> Result<JoinHandle<()>> {
    Ok(std::thread::Builder::new().name("draw".into()).spawn(move || {
        if let Err(e) = run(gpu, &scene, surface(), shot) {
            // (The window going is how it ends; what fails after that is nothing.)
            if scene.quit.load(Ordering::Relaxed) || scene.stop_draw.load(Ordering::Relaxed) {
                return;
            }
            say!("drawing stopped: {e:#}");
            *scene.failed.lock().unwrap() = Some(format!("{e:#}"));
            scene.wake_window();
        }
    })?)
}

fn run(gpu: Gpu, scene: &Scene, surface: Surface, mut shot: Option<PathBuf>) -> Result<()> {
    let mut drawer = Drawer::new(gpu, surface)?;
    scene.tearing_allowed.store(tearing_supported(&drawer.gpu), Ordering::Relaxed);
    let mut first = None::<Instant>;
    let mut occluded = false;
    // Whether the swap chain has said it has room and nothing has been presented since.
    let mut room = false;
    let mut pokes = 0;
    // The interface that is in the sheet now, and the session whose picture is kept.
    let mut drawn = None::<Arc<crate::ui::Frame>>;
    let mut of = 0usize;
    while !scene.quit.load(Ordering::Relaxed) && !scene.stop_draw.load(Ordering::Relaxed) {
        if !room {
            // Not for ever: a window that is covered may never be given room.
            // SAFETY: the handle is the swap chain's and lives as long as it.
            let _ = unsafe { WaitForSingleObject(drawer.room, 100) };
            room = true;
        }
        // Wait for a picture or a change to the interface, but not for ever: the window may
        // have changed size meanwhile.
        scene.wait(&mut pokes, Duration::from_millis(50));
        let session = scene.session.lock().unwrap().clone();
        let now_of = session.as_ref().map_or(0, |s| Arc::as_ptr(s) as usize);
        let mut changed = false;
        if now_of != of {
            // Another session, or none: the old one's picture and its decoder's textures go.
            of = now_of;
            drawer.shown = None;
            drawer.views.clear();
            changed = true;
        }
        let fresh = session.as_ref().and_then(|s| s.picture.lock().unwrap().take());
        changed |= drawer.resize(unpack(scene.client_size.load(Ordering::Relaxed)))?;
        drawer.set_scale(crate::stream::unpack_scale(scene.panel_scale.load(Ordering::Relaxed)))?;
        let frame = scene.frame.lock().unwrap().clone();
        let mut painted = false;
        if let Some(frame) = &frame {
            let unpainted = drawer.layers.as_ref().is_some_and(|l| l.ui_target.is_none());
            if drawer.layers.is_some() && (unpainted || drawn.as_ref().is_none_or(|d| d.serial != frame.serial)) {
                let _alone = DRAWING.lock().unwrap();
                drawer.paint(frame)?;
                drawn = Some(frame.clone());
                painted = true;
            }
            drawer.show_interface(!frame.ops.is_empty())?;
        }
        // The picture's layer is there while a session is, where the interface leaves room.
        let whole = Some((0, 0, drawer.size.0 as i32, drawer.size.1 as i32));
        let video = frame.as_ref().map_or(whole, |f| f.video);
        let before = drawer.clip;
        drawer.place_video(if session.is_some() { video } else { None })?;
        changed |= drawer.clip != before;
        let new = fresh.is_some();
        if let Some(picture) = fresh {
            drawer.shown = Some(picture);
        }
        // A picture of the window that was asked for, once what it should show is drawn.
        let wanted = scene.shot.lock().unwrap().as_ref().is_some_and(|(_, serial)| drawn.as_ref().is_some_and(|d| d.serial >= *serial));
        let early = new && shot.is_some() && first.is_some_and(|t| t.elapsed() >= SHOT_AFTER);
        // The picture's layer is drawn again for a new picture, a new size or place, and for
        // a saved picture of the window (what was presented is no longer there to be read).
        let draws = session.is_some() && (new || changed || wanted);
        if !draws && !painted && !wanted {
            continue;
        }
        let picture = drawer.shown.take();
        // An HDR stream: passed through where the screen is HDR, tone-mapped where it isn't.
        let colour = session.as_ref().map_or((false, 203.0, 1000.0), |s| (s.hdr.load(Ordering::Relaxed), s.sdr_white.load(Ordering::Relaxed) as f32, s.peak.load(Ordering::Relaxed) as f32));
        drawer.set_pq(colour.0 && scene.screen_hdr.load(Ordering::Relaxed));
        scene.hdr_shown.store(if !colour.0 || session.is_none() { 0 } else if drawer.pq { 1 } else { 2 }, Ordering::Relaxed);
        if draws {
            let _alone = DRAWING.lock().unwrap();
            drawer.draw(picture.as_ref(), video, colour)?;
        }
        let backdrop = frame.as_ref().map_or(crate::ui::Color(0.0, 0.0, 0.0, 1.0), |f| f.backdrop);
        if early {
            if let Err(e) = drawer.shot(&shot.take().unwrap(), backdrop, draws) {
                say!("shot failed: {e:#}");
            }
        }
        if wanted {
            if let Some((path, _)) = scene.shot.lock().unwrap().take() {
                if let Err(e) = drawer.shot(&path, backdrop, draws) {
                    say!("shot failed: {e:#}");
                }
                scene.shots.fetch_add(1, Ordering::Relaxed);
                scene.wake_window();
            }
        }
        if painted {
            // SAFETY: a call on the swap chain. The interface is shown as soon as may be.
            if let Some(layers) = &drawer.layers {
                unsafe { layers.ui_swap.Present(0, DXGI_PRESENT(0)) }.ok().context("present the interface")?;
            }
        }
        if draws {
            // For the next refresh of the display (there is room, so this doesn't wait); or,
            // with variable refresh, at once with tearing allowed, so that a monitor that varies
            // its refresh shows the picture now. Only full screen's swap chain may tear (the
            // composition ones aren't, `chain_flags`): measured with `--watch-vblank` on a 240 Hz
            // monitor, 100 pictures a second, its vertical blanks followed them 10.00 ms apart
            // in 8-bit, 10-bit and PQ.
            let tear = drawer.tears && scene.variable_refresh.load(Ordering::Relaxed);
            if scene.tearing.swap(tear, Ordering::Relaxed) != tear {
                say!("presenting: {}", if tear { "each picture as it arrives, tearing allowed (variable refresh)" } else { "at the display's refresh" });
            }
            // SAFETY: a call on the swap chain.
            if let Some(p) = &drawer.presenter {
                p.copy(&drawer.swap)?;
            }
            let presented = unsafe { if tear { drawer.swap.Present(0, DXGI_PRESENT_ALLOW_TEARING) } else { drawer.swap.Present(1, DXGI_PRESENT(0)) } };
            room = false;
            presented.ok().context("present")?;
            // For tests: how the pictures reach the screen, in the log every few seconds.
            if new && scene.measure.load(Ordering::Relaxed) {
                drawer.cadence.note(&drawer.swap);
                if let Some(line) = drawer.cadence.report(&drawer.swap, scene.refresh_hz.load(Ordering::Relaxed), tear) {
                    say!("presenting: {line}");
                }
            }
            if (presented == DXGI_STATUS_OCCLUDED) != occluded {
                occluded = !occluded;
                say!("the window is {}", if occluded { "covered or has no desktop to be on: presenting isn't paced by a display" } else { "visible again" });
            }
        }
        if let (true, Some(s), Some(picture)) = (new, &session, &picture) {
            first.get_or_insert_with(Instant::now);
            s.presented.fetch_add(1, Ordering::Relaxed);
            s.presented_total.fetch_add(1, Ordering::Relaxed);
            if let Some(age) = picture.age_us {
                s.age_presented_us.store(age as u64 + picture.decoded.elapsed().as_micros() as u64, Ordering::Relaxed);
            }
        }
        drawer.shown = picture;
    }
    Ok(())
}
