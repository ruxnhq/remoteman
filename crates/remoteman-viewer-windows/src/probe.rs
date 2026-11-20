//! `remoteman-windows --vrr-probe [SECONDS]`: the control for variable refresh. A borderless
//! window over the whole primary monitor with a flip-model swap chain of its own (an HWND one,
//! as Microsoft documents variable refresh for), presenting a dark, nearly still picture 100
//! times a second with tearing allowed, while the monitor's vertical blanks are logged
//! (`monitor::watch_vblanks`). If the monitor varies its refresh for this window, it blanks
//! about every 10 ms; at a fixed rate, once a refresh. The app's own picture (in a
//! `SwapChainPanel`) is measured the same way with `--watch-vblank`.
//!
//! Variations (`REMOTEMAN_PROBE`, comma-separated), to find what keeps a monitor from varying
//! its refresh and what the full-screen session window may have:
//! - `noactivate`, `tool`, `owned`: the window not activated, a tool window, owned by another;
//! - `10bit`, `pq`, `fp16`: the swap chain in 10 bits (sRGB, or PQ BT.2020 with `pq`), or in
//!   half floats (scRGB), instead of 8-bit sRGB;
//! - `layer`, `layer-empty`: a DirectComposition layer over the window, as the session's bar
//!   would be drawn: a bar along the top (the rest transparent), or nothing but transparency;
//! - `exclusive`: DXGI's exclusive full screen (`SetFullscreenState`), presenting with a sync
//!   interval of 0 and no tearing flag (which exclusive mode refuses).
//!
//! With `REMOTEMAN_VBLANK_SECONDS=1` the vertical blanks come as a timeline, a line a second;
//! the presentation modes the swap chain reports are logged a line a second too.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D11::{ID3D11DeviceContext1, ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, w};

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: passing the message on as it came.
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

/// What a layer over the window keeps alive.
struct Layer {
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _swap: IDXGISwapChain1,
}

/// A DirectComposition layer over the window: a bar 44 pixels high along the top, nearly
/// opaque, the rest transparent (or all of it transparent).
fn layer(gpu: &crate::gpu::Gpu, hwnd: HWND, w: u32, h: u32, bar: bool) -> Result<Layer> {
    // SAFETY: DXGI, Direct3D and DirectComposition calls on interfaces held here, with
    // structures that live through each call.
    unsafe {
        let factory: IDXGIFactory2 = gpu.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            ..Default::default()
        };
        let swap = factory.CreateSwapChainForComposition(&gpu.device, &desc, None).context("the layer's swap chain")?;
        let back: ID3D11Texture2D = swap.GetBuffer(0)?;
        let mut view = None;
        gpu.device.CreateRenderTargetView(&back, None, Some(&mut view))?;
        let view: ID3D11RenderTargetView = view.context("no view")?;
        gpu.context.ClearRenderTargetView(&view, &[0.0, 0.0, 0.0, 0.0]);
        if bar {
            let ctx: ID3D11DeviceContext1 = gpu.context.cast()?;
            // Premultiplied: a dark grey at 85 %.
            ctx.ClearView(&view, &[0.1 * 0.85, 0.1 * 0.85, 0.1 * 0.85, 0.85], Some(&[RECT { left: 0, top: 0, right: w as i32, bottom: 44 }]));
        }
        swap.Present(0, DXGI_PRESENT(0)).ok()?;
        let dcomp: IDCompositionDevice = DCompositionCreateDevice(&gpu.device.cast::<IDXGIDevice>()?).context("no DirectComposition")?;
        let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
        let visual = dcomp.CreateVisual()?;
        visual.SetContent(&swap)?;
        target.SetRoot(&visual)?;
        dcomp.Commit()?;
        Ok(Layer { _dcomp: dcomp, _target: target, _swap: swap })
    }
}

pub fn run(gpu: &crate::gpu::Gpu, seconds: u64) -> Result<()> {
    // SAFETY: Win32 and DXGI calls with structures that live through each call; the window and
    // swap chain are ours and go at the end.
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW { lpfnWndProc: Some(procedure), hInstance: instance.into(), lpszClassName: w!("remoteman-vrr-probe"), hCursor: LoadCursorW(None, IDC_ARROW)?, ..Default::default() };
        RegisterClassW(&class);
        let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let how = std::env::var("REMOTEMAN_PROBE").unwrap_or_default();
        let has = |w: &str| how.split(',').any(|x| x == w);
        let mut ex = WINDOW_EX_STYLE(0);
        if has("noactivate") {
            ex |= WS_EX_NOACTIVATE;
        }
        if has("tool") {
            ex |= WS_EX_TOOLWINDOW;
        }
        if has("layer") || has("layer-empty") {
            // What a DirectComposition target on a window wants.
            ex |= WS_EX_NOREDIRECTIONBITMAP;
        }
        let owner = if has("owned") { CreateWindowExW(WINDOW_EX_STYLE(0), w!("remoteman-vrr-probe"), w!("probe owner"), WS_OVERLAPPEDWINDOW | WS_VISIBLE, 100, 100, 800, 600, None, None, Some(instance.into()), None).ok() } else { None };
        let style = if has("noactivate") { WS_POPUP } else { WS_POPUP | WS_VISIBLE };
        let hwnd = CreateWindowExW(ex, w!("remoteman-vrr-probe"), w!("remoteman variable refresh probe"), style, 0, 0, w, h, owner, None, Some(instance.into()), None).context("the probe's window")?;
        if has("noactivate") {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        } else {
            let _ = SetForegroundWindow(hwnd);
        }
        say!("probe: variations {how:?}");
        let factory: IDXGIFactory2 = gpu.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
        let tears = crate::draw::tearing_supported(gpu);
        let exclusive = has("exclusive");
        let format = if has("fp16") {
            DXGI_FORMAT_R16G16B16A16_FLOAT
        } else if has("10bit") || has("pq") {
            DXGI_FORMAT_R10G10B10A2_UNORM
        } else {
            DXGI_FORMAT_B8G8R8A8_UNORM
        };
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w as u32,
            Height: h as u32,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: if tears { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 } else { 0 } | if exclusive { DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH.0 as u32 } else { 0 },
            ..Default::default()
        };
        let swap = factory.CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None).context("an HWND swap chain")?;
        if has("pq") {
            let s3: IDXGISwapChain3 = swap.cast()?;
            s3.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020).context("PQ")?;
        } else if has("fp16") {
            let s3: IDXGISwapChain3 = swap.cast()?;
            s3.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709).context("scRGB")?;
        }
        let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
        let _layer = if has("layer") || has("layer-empty") { Some(layer(gpu, hwnd, w as u32, h as u32, has("layer"))?) } else { None };
        // Exclusive mode is left again on every way out below, before the swap chain goes.
        let result = (|| -> Result<()> {
            if exclusive {
                swap.SetFullscreenState(true, None).context("exclusive full screen")?;
                // The buffers are resized to the mode's after a mode switch.
                swap.ResizeBuffers(0, 0, 0, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(desc.Flags as i32)).context("resize after going exclusive")?;
                say!("probe: in DXGI's exclusive full screen");
            }
            let back: ID3D11Texture2D = swap.GetBuffer(0)?;
            let mut target = None;
            gpu.device.CreateRenderTargetView(&back, None, Some(&mut target))?;
            let target = target.context("no render target")?;
            drop(back);
            let scene = Arc::new(crate::stream::Scene::default());
            let (_, _, hz, ..) = crate::window::monitor_of(hwnd);
            scene.refresh_hz.store(hz, Ordering::Relaxed);
            let tear = tears && !exclusive;
            scene.tearing.store(tear, Ordering::Relaxed);
            say!(
                "probe: {w}x{h} at {hz} Hz, an HWND flip swap chain ({}), {}; 100 presents a second for {seconds} s",
                match format {
                    DXGI_FORMAT_R16G16B16A16_FLOAT => "half floats, scRGB",
                    DXGI_FORMAT_R10G10B10A2_UNORM if has("pq") => "10 bits, PQ BT.2020",
                    DXGI_FORMAT_R10G10B10A2_UNORM => "10 bits, sRGB",
                    _ => "8 bits, sRGB",
                },
                if exclusive {
                    "exclusive, sync interval 0"
                } else if tears {
                    "presenting with tearing allowed"
                } else {
                    "DXGI doesn't allow tearing: presenting at the refresh"
                }
            );
            crate::monitor::watch_vblanks(hwnd.0 as isize, scene.clone());
            if has("thread") {
                // Presenting from another thread than the window's (as the app's draw thread
                // does), this thread only pumping the window's messages.
                struct Parts(IDXGISwapChain1, windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView, windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext);
                // SAFETY: used by the other thread alone until it ends.
                unsafe impl Send for Parts {}
                let parts = Parts(swap.clone(), target.clone(), gpu.context.clone());
                say!("probe: presenting from another thread");
                let presenter = std::thread::spawn(move || {
                    let parts = parts;
                    let started = Instant::now();
                    let mut n = 0u64;
                    while started.elapsed() < Duration::from_secs(seconds) {
                        let g = if n % 2 == 0 { 0.10 } else { 0.11 };
                        // (Calls on the parts this thread holds, in the unsafe block around.)
                        parts.2.OMSetRenderTargets(Some(&[Some(parts.1.clone())]), None);
                        parts.2.ClearRenderTargetView(&parts.1, &[g, g, g, 1.0]);
                        let _ = if tear { parts.0.Present(0, DXGI_PRESENT_ALLOW_TEARING) } else { parts.0.Present(1, DXGI_PRESENT(0)) };
                        n += 1;
                        let due = started + Duration::from_millis(10 * n);
                        std::thread::sleep(due.saturating_duration_since(Instant::now()));
                    }
                    n
                });
                while !presenter.is_finished() {
                    let mut msg = MSG::default();
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                say!("probe: {} presented from another thread", presenter.join().unwrap_or(0));
                scene.quit.store(true, Ordering::Relaxed);
                return Ok(());
            }
            let started = Instant::now();
            let mut n = 0u64;
            let mut modes = std::collections::BTreeMap::<i32, u32>::new();
            let mut second = std::collections::BTreeMap::<i32, u32>::new();
            let mut tick = Instant::now();
            while started.elapsed() < Duration::from_secs(seconds) {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                // Nearly still and dark: two greys a step apart.
                let g = if n % 2 == 0 { 0.10 } else { 0.11 };
                gpu.context.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
                gpu.context.ClearRenderTargetView(&target, &[g, g, g, 1.0]);
                let presented = if tear { swap.Present(0, DXGI_PRESENT_ALLOW_TEARING) } else if exclusive { swap.Present(0, DXGI_PRESENT(0)) } else { swap.Present(1, DXGI_PRESENT(0)) };
                presented.ok().context("present")?;
                n += 1;
                let mut media = DXGI_FRAME_STATISTICS_MEDIA::default();
                if swap.cast::<IDXGISwapChainMedia>().and_then(|m| m.GetFrameStatisticsMedia(&mut media)).is_ok() {
                    *modes.entry(media.CompositionMode.0).or_default() += 1;
                    *second.entry(media.CompositionMode.0).or_default() += 1;
                }
                if tick.elapsed() >= Duration::from_secs(1) {
                    // Which window has the keyboard: a monitor varies its refresh only for the
                    // activated one.
                    let front = GetForegroundWindow();
                    let front = if front == hwnd {
                        "this window".to_owned()
                    } else {
                        let mut class = [0u16; 128];
                        let n = GetClassNameW(front, &mut class).max(0) as usize;
                        let mut pid = 0u32;
                        GetWindowThreadProcessId(front, Some(&mut pid));
                        format!("{} of process {pid}", String::from_utf16_lossy(&class[..n]))
                    };
                    say!("probe: second {}: presentation modes {second:?}; in front: {front}", started.elapsed().as_secs());
                    second.clear();
                    tick = Instant::now();
                }
                let due = started + Duration::from_millis(10 * n);
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
            say!("probe: {n} presented; presentation modes seen (0 composed, 1 overlay or independent flip, 2 none): {modes:?}");
            scene.quit.store(true, Ordering::Relaxed);
            Ok(())
        })();
        if exclusive {
            let _ = swap.SetFullscreenState(false, None);
        }
        std::thread::sleep(Duration::from_millis(200));
        drop(_layer);
        drop(swap);
        let _ = DestroyWindow(hwnd);
        if let Some(owner) = owner {
            let _ = DestroyWindow(owner);
        }
        result
    }
}
