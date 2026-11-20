//! `remoteman-windows --vrr-probe [SECONDS]`: the control for variable refresh. A borderless
//! window over the whole primary monitor with a flip-model swap chain of its own (an HWND one,
//! as Microsoft documents variable refresh for), presenting a dark, nearly still picture 100
//! times a second with tearing allowed, while the monitor's vertical blanks are logged
//! (`monitor::watch_vblanks`). If the monitor varies its refresh for this window, it blanks
//! about every 10 ms; at a fixed rate, once a refresh. The app's own picture (in a
//! `SwapChainPanel`) is measured the same way with `--watch-vblank`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, w};

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: passing the message on as it came.
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
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
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("remoteman-vrr-probe"), w!("remoteman variable refresh probe"), WS_POPUP | WS_VISIBLE, 0, 0, w, h, None, None, Some(instance.into()), None).context("the probe's window")?;
        let factory: IDXGIFactory2 = gpu.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
        let tears = crate::draw::tearing_supported(gpu);
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w as u32,
            Height: h as u32,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: if tears { DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 } else { 0 },
            ..Default::default()
        };
        let swap = factory.CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None).context("an HWND swap chain")?;
        let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
        let back: ID3D11Texture2D = swap.GetBuffer(0)?;
        let mut target = None;
        gpu.device.CreateRenderTargetView(&back, None, Some(&mut target))?;
        let target = target.context("no render target")?;
        drop(back);
        let scene = Arc::new(crate::stream::Scene::default());
        let (_, _, hz, ..) = crate::window::monitor_of(hwnd);
        scene.refresh_hz.store(hz, Ordering::Relaxed);
        scene.tearing.store(tears, Ordering::Relaxed);
        say!("probe: {w}x{h} at {hz} Hz, an HWND flip swap chain, {}; 100 presents a second for {seconds} s", if tears { "presenting with tearing allowed" } else { "DXGI doesn't allow tearing: presenting at the refresh" });
        crate::monitor::watch_vblanks(hwnd.0 as isize, scene.clone());
        let started = Instant::now();
        let mut n = 0u64;
        let mut modes = std::collections::BTreeMap::<i32, u32>::new();
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
            let presented = if tears { swap.Present(0, DXGI_PRESENT_ALLOW_TEARING) } else { swap.Present(1, DXGI_PRESENT(0)) };
            presented.ok().context("present")?;
            n += 1;
            let mut media = DXGI_FRAME_STATISTICS_MEDIA::default();
            if swap.cast::<IDXGISwapChainMedia>().and_then(|m| m.GetFrameStatisticsMedia(&mut media)).is_ok() {
                *modes.entry(media.CompositionMode.0).or_default() += 1;
            }
            let due = started + Duration::from_millis(10 * n);
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
        }
        say!("probe: {n} presented; presentation modes seen (0 composed, 1 overlay or independent flip, 2 none): {modes:?}");
        scene.quit.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(200));
        let _ = DestroyWindow(hwnd);
        Ok(())
    }
}
