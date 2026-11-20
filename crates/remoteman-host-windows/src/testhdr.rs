//! `remoteman-host --test-hdr-window SECONDS`: a window for testing the HDR picture, with no
//! program of anyone else's needed. It covers the monitor that isn't the main one (the
//! device's own, where there is one) with a 16-bit float scRGB swap chain: a dark ground
//! (8 nits), a square at Windows' usual SDR white (200 nits), and a bar at 1000 nits that
//! moves across every frame, so the picture changes 60 times a second. On an HDR monitor the
//! bar is five times as bright as the square; on an SDR one both are clipped to white.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11DeviceContext1, ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory2, IDXGISwapChain1, IDXGISwapChain3};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SW_SHOW, ShowWindow, TranslateMessage, WNDCLASSW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, Interface, w};

unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: passing on what was given.
    unsafe { DefWindowProcW(window, message, w, l) }
}

unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, found: LPARAM) -> BOOL {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: a live monitor and a structure that says its size; `found` is the caller's RECTs.
    unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            (*(found.0 as *mut Vec<(RECT, bool)>)).push((info.rcMonitor, info.dwFlags & 1 != 0));
        }
    }
    true.into()
}

pub fn run(seconds: u64) -> Result<()> {
    let mut monitors: Vec<(RECT, bool)> = Vec::new();
    // SAFETY: the callback writes into the vector, which outlives the call.
    unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut monitors as *mut _ as isize)) }.ok().context("listing the monitors")?;
    let at = monitors.iter().find(|m| !m.1).or(monitors.first()).map(|m| m.0).context("no monitor")?;
    let (width, height) = ((at.right - at.left) as u32, (at.bottom - at.top) as u32);
    tracing::info!(left = at.left, top = at.top, width, height, "the HDR test window");
    // SAFETY: Win32 and Direct3D calls as documented, on objects of this thread.
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(procedure), hInstance: instance, lpszClassName: w!("remoteman-hdr-test"), ..Default::default() });
        let window = CreateWindowExW(WS_EX_TOPMOST, w!("remoteman-hdr-test"), w!("HDR test"), WS_POPUP, at.left, at.top, width as i32, height as i32, None, None, Some(instance), None)?;
        let _ = ShowWindow(window, SW_SHOW);

        let (mut device, mut context) = (None::<ID3D11Device>, None::<ID3D11DeviceContext>);
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, Default::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, Some(&[D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context))?;
        let (device, context) = (device.context("no device")?, context.context("no context")?.cast::<ID3D11DeviceContext1>()?);
        let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            ..Default::default()
        };
        let chain: IDXGISwapChain1 = factory.CreateSwapChainForHwnd(&device, window, &desc, None, None)?;
        // scRGB: linear, BT.709 primaries, 1.0 = 80 nits, beyond 1.0 for HDR.
        chain.cast::<IDXGISwapChain3>()?.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709).context("the scRGB colour space")?;
        let buffer: ID3D11Texture2D = chain.GetBuffer(0)?;
        let mut target = None::<ID3D11RenderTargetView>;
        device.CreateRenderTargetView(&buffer, None, Some(&mut target))?;
        let target = target.context("no view")?;

        let began = Instant::now();
        let mut frame = 0u32;
        let mut message = MSG::default();
        while began.elapsed() < Duration::from_secs(seconds) {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let (w, h) = (width as i32, height as i32);
            context.ClearView(&target, &[0.1, 0.1, 0.1, 1.0], None);
            // SDR white (200 nits) in a square, left of middle.
            context.ClearView(&target, &[2.5, 2.5, 2.5, 1.0], Some(&[RECT { left: w / 8, top: h / 4, right: w / 8 + h / 2, bottom: h * 3 / 4 }]));
            // 1000 nits, a bar moving across.
            let x = ((frame * 8) % w.max(1) as u32) as i32;
            context.ClearView(&target, &[12.5, 12.5, 12.5, 1.0], Some(&[RECT { left: x, top: h / 3, right: (x + w / 10).min(w), bottom: h * 2 / 3 }]));
            chain.Present(1, DXGI_PRESENT(0)).ok()?;
            frame += 1;
        }
        tracing::info!(frames = frame, "the HDR test window is done");
    }
    Ok(())
}
