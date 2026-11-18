//! The draw thread: a flip-model swap chain on the window, and the newest decoded picture put
//! on it at each refresh of the display.
//!
//! The swap chain holds one picture in waiting at most and says, through a handle to wait on,
//! when it has room for the next: about once a refresh of the display. This thread waits for
//! that and then draws whatever is newest; what the decoder finished meanwhile and was replaced
//! is never seen. The waiting is done on the handle and not inside `Present`, which would hold
//! the device's lock all the while and keep the decoder out: decoding would then go at the
//! display's rate instead of the stream's (seen: 60 a second decoded of a 120 Hz stream).
//! The picture is two planes (Y, and CbCr interleaved) of one slice of the decoder's texture;
//! a pixel shader turns limited-range BT.709 into RGB.

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
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::core::{Interface, PCSTR};

use crate::decode::Picture;
use crate::gpu::Gpu;
use crate::stream::{Shared, unpack};

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
SamplerState smooth : register(s0);

// `visible`: the part of the texture that is picture (it is the coded size, a little larger),
// and `last`, the last place to sample so that nothing beyond the edge is blended in.
// `scale` turns a normalised sample into a code value at the picture's bit depth; `black`,
// `yrange`, `mid` and `crange` are the code values of the range's ends.
cbuffer Picture : register(b0) {
    float2 visible;
    float2 last;
    float scale, black, yrange, mid;
    float crange;
    float3 unused;
};

// Limited-range BT.709, the same numbers as the Linux viewer.
float4 ps(Corner c) : SV_Target {
    float3 at = float3(min(c.uv * visible, last), 0);
    float y = (luma.Sample(smooth, at) * scale - black) / yrange;
    float2 cc = (chroma.Sample(smooth, at) * scale - mid) / crange;
    return float4(y + 1.5748 * cc.y, y - 0.1873 * cc.x - 0.4681 * cc.y, y + 1.8556 * cc.x, 1);
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

/// The two planes of one slice, as the shader reads them.
#[derive(Clone)]
struct Planes {
    luma: ID3D11ShaderResourceView,
    chroma: ID3D11ShaderResourceView,
}

struct Drawer {
    gpu: Gpu,
    swap: IDXGISwapChain1,
    /// Signalled when the swap chain has room for another picture.
    room: HANDLE,
    /// The swap chain's pixel format: 10 bits a channel where the system gives it.
    format: DXGI_FORMAT,
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
    /// The picture on screen, kept for drawing again when the window changes size.
    shown: Option<Picture>,
}

fn plane_formats(format: DXGI_FORMAT) -> Result<(DXGI_FORMAT, DXGI_FORMAT)> {
    Ok(match format {
        DXGI_FORMAT_P010 => (DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM),
        DXGI_FORMAT_NV12 => (DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM),
        other => bail!("the decoder's pictures are format {}, which isn't drawn yet", other.0),
    })
}

impl Drawer {
    fn new(gpu: Gpu, hwnd: HWND) -> Result<Self> {
        // SAFETY: Direct3D and DXGI calls on interfaces held here, with structures that live
        // through each call.
        unsafe {
            let factory: IDXGIFactory2 = gpu.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
            let mut desc = DXGI_SWAP_CHAIN_DESC1 {
                // 0: the window's size.
                Width: 0,
                Height: 0,
                Format: DXGI_FORMAT_R10G10B10A2_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: WAITABLE.0 as u32,
                ..Default::default()
            };
            // The stream is 10-bit, so the buffers are too where that is allowed.
            let swap = match factory.CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None) {
                Ok(swap) => swap,
                Err(e) => {
                    say!("no 10-bit swap chain ({e}): 8-bit");
                    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
                    factory.CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None).context("create the swap chain")?
                }
            };
            // Alt+Enter is the other computer's.
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            let now = swap.GetDesc1()?;
            let latency: IDXGISwapChain2 = swap.cast()?;
            latency.SetMaximumFrameLatency(1)?;
            let room = latency.GetFrameLatencyWaitableObject();
            anyhow::ensure!(!room.is_invalid(), "the swap chain has nothing to wait on");

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
            let c = D3D11_BUFFER_DESC { ByteWidth: 48, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
            gpu.device.CreateBuffer(&c, None, Some(&mut constants))?;
            say!("swap chain: {}x{}, {}, flip model", now.Width, now.Height, if now.Format == DXGI_FORMAT_R10G10B10A2_UNORM { "10 bits a channel" } else { "8 bits a channel" });
            Ok(Self {
                gpu,
                swap,
                room,
                format: now.Format,
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
        Ok(Planes { luma: view(luma)?, chroma: view(chroma)? })
    }

    /// What the shader should read for this picture: the decoder's own slice, or a copy of it.
    fn planes(&mut self, picture: &Picture) -> Result<(Planes, D3D11_TEXTURE2D_DESC)> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: the out pointer is valid.
        unsafe { picture.texture.GetDesc(&mut desc) };
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
        // SAFETY: calls on interfaces we hold.
        unsafe {
            self.gpu.context.OMSetRenderTargets(None, None);
            self.swap.ResizeBuffers(0, to.0, to.1, DXGI_FORMAT_UNKNOWN, WAITABLE).context("resize the swap chain")?;
        }
        self.size = to;
        Ok(true)
    }

    /// Draw the picture into the back buffer, as large as fits with its shape kept.
    fn draw(&mut self, picture: &Picture) -> Result<()> {
        let (planes, desc) = self.planes(picture)?;
        // SAFETY: Direct3D calls on interfaces held here; everything bound outlives the draw.
        unsafe {
            if self.target.is_none() {
                let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
                let mut target = None;
                self.gpu.device.CreateRenderTargetView(&back, None, Some(&mut target))?;
                self.target = target;
            }
            let target = self.target.clone().context("no render target")?;
            let ctx = &self.gpu.context;
            let ten = desc.Format == DXGI_FORMAT_P010;
            // P010 keeps its 10 bits at the top of 16: a normalised sample times 65535/64 is
            // the 10-bit code.
            let (scale, black, yrange, mid, crange) = if ten { (65535.0 / 64.0, 64.0, 876.0, 512.0, 896.0) } else { (255.0, 16.0, 219.0, 128.0, 224.0) };
            let (tw, th) = (desc.Width as f32, desc.Height as f32);
            let (vw, vh) = (picture.width as f32, picture.height as f32);
            let constants: [f32; 12] = [vw / tw, vh / th, (vw - 0.5) / tw, (vh - 0.5) / th, scale, black, yrange, mid, crange, 0.0, 0.0, 0.0];
            ctx.UpdateSubresource(&self.constants, 0, None, constants.as_ptr() as *const _, 0, 0);

            let (ww, wh) = (self.size.0 as f32, self.size.1 as f32);
            let fit = (ww / vw).min(wh / vh);
            let (dw, dh) = (vw * fit, vh * fit);
            let viewport = D3D11_VIEWPORT { TopLeftX: ((ww - dw) / 2.0).floor(), TopLeftY: ((wh - dh) / 2.0).floor(), Width: dw, Height: dh, MinDepth: 0.0, MaxDepth: 1.0 };

            // The flip model unbinds the back buffer at every present.
            ctx.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            ctx.ClearRenderTargetView(&target, &[0.0, 0.0, 0.0, 1.0]);
            ctx.RSSetViewports(Some(&[viewport]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.PSSetShaderResources(0, Some(&[Some(planes.luma), Some(planes.chroma)]));
            ctx.Draw(3, 0);
            // Let go of the slice, so the decoder may write to it again when its turn comes.
            ctx.PSSetShaderResources(0, Some(&[None, None]));
        }
        Ok(())
    }

    /// The back buffer as it is about to be presented, copied down and saved.
    fn shot(&self, path: &std::path::Path) -> Result<()> {
        // SAFETY: the staging texture is mapped for reading and unmapped again; rows are read
        // within the mapped pitch and height.
        unsafe {
            let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
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
            let mut rgb = Vec::with_capacity(w * h * 3);
            for row in 0..h {
                let line = std::slice::from_raw_parts((map.pData as *const u8).add(row * map.RowPitch as usize), w * 4);
                for px in line.chunks_exact(4) {
                    if self.format == DXGI_FORMAT_R10G10B10A2_UNORM {
                        // Red in the low 10 bits, then green, then blue; the PNG gets the top 8.
                        let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                        rgb.extend_from_slice(&[(v >> 2) as u8, (v >> 12) as u8, (v >> 22) as u8]);
                    } else {
                        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
                    }
                }
            }
            self.gpu.context.Unmap(&staging, 0);
            crate::shot::save(path, desc.Width, desc.Height, &rgb)?;
            say!("shot: {} ({}x{}, the back buffer before it was presented)", path.display(), desc.Width, desc.Height);
        }
        Ok(())
    }
}

const WAITABLE: DXGI_SWAP_CHAIN_FLAG = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;

/// How long after the first picture `--shot` is taken: long enough for the host's content to
/// be up and moving.
const SHOT_AFTER: Duration = Duration::from_secs(4);

pub fn start(gpu: Gpu, shared: Arc<Shared>, hwnd: HWND, shot: Option<PathBuf>) -> Result<JoinHandle<()>> {
    let hwnd = hwnd.0 as isize;
    Ok(std::thread::Builder::new().name("draw".into()).spawn(move || {
        if let Err(e) = run(gpu, &shared, HWND(hwnd as *mut _), shot) {
            say!("drawing stopped: {e:#}");
            *shared.closed.lock().unwrap() = Some(format!("{e:#}"));
        }
    })?)
}

fn run(gpu: Gpu, shared: &Shared, hwnd: HWND, mut shot: Option<PathBuf>) -> Result<()> {
    let mut drawer = Drawer::new(gpu, hwnd)?;
    let mut first = None::<Instant>;
    let mut occluded = false;
    // Whether the swap chain has said it has room and nothing has been presented since.
    let mut room = false;
    while !shared.quit.load(Ordering::Relaxed) {
        if !room {
            // Not for ever: a window that is covered may never be given room.
            // SAFETY: the handle is the swap chain's and lives as long as it.
            let _ = unsafe { WaitForSingleObject(drawer.room, 100) };
            room = true;
        }
        // Wait for a picture, but not for ever: the window may have changed size meanwhile.
        let fresh = {
            let mut waiting = shared.picture.lock().unwrap();
            if waiting.is_none() {
                waiting = shared.fresh.wait_timeout(waiting, Duration::from_millis(50)).unwrap().0;
            }
            waiting.take()
        };
        let resized = drawer.resize(unpack(shared.client_size.load(Ordering::Relaxed)))?;
        let new = fresh.is_some();
        if let Some(picture) = fresh {
            drawer.shown = Some(picture);
        }
        if !new && !resized {
            continue;
        }
        let Some(picture) = drawer.shown.take() else { continue };
        drawer.draw(&picture)?;
        if new && shot.is_some() && first.is_some_and(|t| t.elapsed() >= SHOT_AFTER) {
            if let Err(e) = drawer.shot(&shot.take().unwrap()) {
                say!("shot failed: {e:#}");
            }
        }
        // For the next refresh of the display. There is room, so this doesn't wait.
        // SAFETY: a call on the swap chain.
        let presented = unsafe { drawer.swap.Present(1, DXGI_PRESENT(0)) };
        room = false;
        presented.ok().context("present")?;
        if (presented == DXGI_STATUS_OCCLUDED) != occluded {
            occluded = !occluded;
            say!("the window is {}", if occluded { "covered or has no desktop to be on: presenting isn't paced by a display" } else { "visible again" });
        }
        if new {
            first.get_or_insert_with(Instant::now);
            shared.presented.fetch_add(1, Ordering::Relaxed);
            shared.presented_total.fetch_add(1, Ordering::Relaxed);
            if let Some(age) = picture.age_us {
                shared.age_presented_us.store(age as u64 + picture.decoded.elapsed().as_micros() as u64, Ordering::Relaxed);
            }
        }
        drawer.shown = Some(picture);
    }
    Ok(())
}
