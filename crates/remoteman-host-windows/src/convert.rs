//! Convert: the driver's RGB texture to the encoder's input in one go on the GPU: 4:2:0 into a
//! P010 texture (two draws, one for each plane), or 4:4:4 into an R16 texture with the three
//! planes stacked (one draw); SDR as BT.709, or HDR as BT.2020 PQ. See `convert.hlsl` for the
//! numbers.

use std::ffi::c_void;

use anyhow::{Context, Result, bail};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BUFFER_DESC, D3D11_FORMAT_SUPPORT_RENDER_TARGET, D3D11_FORMAT_SUPPORT_TEXTURE2D, D3D11_RENDER_TARGET_VIEW_DESC,
    D3D11_RENDER_TARGET_VIEW_DESC_0, D3D11_RTV_DIMENSION_TEXTURE2D, D3D11_SUBRESOURCE_DATA, D3D11_TEX2D_RTV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT, ID3D11Buffer, ID3D11PixelShader,
    ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_P010, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_FORMAT_R16_UINT, DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
    DXGI_SAMPLE_DESC,
};
use windows::core::{PCSTR, s};

use crate::capture::Gpu;

const SOURCE: &str = include_str!("convert.hlsl");

/// How the driver's texture holds the picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    /// As painted, gamma-encoded: an 8- or 10-bit SDR desktop surface.
    Encoded,
    /// Linear light where 1.0 is 80 nits (a 16-bit float scRGB surface); SDR white is this
    /// many times that.
    Linear { white: f32 },
    /// HDR10: PQ with BT.2020 primaries; SDR white is at this many nits.
    Pq { white: f32 },
}

/// What comes out: SDR (BT.709) or HDR (BT.2020 PQ, an SDR desktop's white put at this many
/// nits).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Output {
    Sdr,
    Pq { sdr_white: f32 },
}

/// What kind of picture a surface of this format holds, and what to say about it in the log.
/// `white`: SDR white on an HDR desktop, in nits (Windows' "SDR content brightness").
pub fn input_for(format: u32, is_hdr: bool, white: f32) -> Result<(Input, Option<&'static str>)> {
    let format = DXGI_FORMAT(format as i32);
    Ok(if format == DXGI_FORMAT_B8G8R8A8_UNORM || format == DXGI_FORMAT_R8G8B8A8_UNORM {
        (Input::Encoded, None)
    } else if format == DXGI_FORMAT_R10G10B10A2_UNORM {
        if is_hdr { (Input::Pq { white }, None) } else { (Input::Encoded, None) }
    } else if format == DXGI_FORMAT_R16G16B16A16_FLOAT {
        if is_hdr {
            (Input::Linear { white: white / 80.0 }, None)
        } else {
            (Input::Linear { white: 1.0 }, Some("the desktop's surface is 16-bit float in SDR: taken to be linear light with white at 1.0"))
        }
    } else {
        bail!("the desktop's surface is in DXGI format {}, which this host doesn't convert", format.0)
    })
}

#[repr(C)]
struct Params {
    input_kind: u32,
    white: f32,
    pq_out: u32,
    sdr_white_nits: f32,
    height: u32,
    pointer_on: u32,
    pointer_at: [i32; 2],
    pointer_size: [u32; 2],
    unused: [u32; 2],
}

/// The draws that fill the encoder's input.
enum Layout {
    /// P010: its two planes.
    Semi { luma: ID3D11PixelShader, chroma: ID3D11PixelShader, luma_plane: ID3D11RenderTargetView, chroma_plane: ID3D11RenderTargetView },
    /// R16, three planes stacked.
    Planar { shader: ID3D11PixelShader, target: ID3D11RenderTargetView },
}

pub struct Converter {
    corner: ID3D11VertexShader,
    layout: Layout,
    params: ID3D11Buffer,
    /// The encoder's input.
    pub output: ID3D11Texture2D,
    width: u32,
    height: u32,
    /// The parameters as the shader has them (the pointer's part changes as it moves).
    p: Params,
    /// The pointer's image, while one is drawn in, and which of its images it is.
    pointer: Option<ID3D11ShaderResourceView>,
    pointer_serial: u64,
}

/// Compile one function of `convert.hlsl`. On failure the compiler's own words are the error.
fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>> {
    let (mut code, mut errors) = (None::<ID3DBlob>, None::<ID3DBlob>);
    // SAFETY: the source is live for the call; the blobs are read within their own size.
    unsafe {
        let bytes = |b: &ID3DBlob| std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec();
        let result = D3DCompile(SOURCE.as_ptr() as *const c_void, SOURCE.len(), s!("convert.hlsl"), None, None, entry, target, D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &mut code, Some(&mut errors));
        if let Err(e) = result {
            let said = errors.map(|b| String::from_utf8_lossy(&bytes(&b)).into_owned()).unwrap_or_default();
            bail!("compiling the conversion shader ({}): {e}: {}", entry.to_string().unwrap_or_default(), said.trim());
        }
        Ok(bytes(&code.context("the shader compiler returned nothing")?))
    }
}

impl Converter {
    /// A converter for pictures of this size (even both ways, as 4:2:0 needs), into 4:4:4 if
    /// `full_chroma` is given: in an R16 texture, UNORM or (`Some(true)`) UINT.
    pub fn new(gpu: &Gpu, width: u32, height: u32, input: Input, output: Output, full_chroma: Option<bool>) -> Result<Self> {
        let planar_uint = full_chroma == Some(true);
        let full_chroma = full_chroma.is_some();
        // SAFETY: Direct3D calls as documented, with live arguments and out-parameters.
        unsafe {
            let format = if planar_uint { DXGI_FORMAT_R16_UINT } else if full_chroma { DXGI_FORMAT_R16_UNORM } else { DXGI_FORMAT_P010 };
            let support = gpu.device.CheckFormatSupport(format).with_context(|| format!("this GPU has no textures of DXGI format {}", format.0))?;
            let needed = (D3D11_FORMAT_SUPPORT_TEXTURE2D.0 | D3D11_FORMAT_SUPPORT_RENDER_TARGET.0) as u32;
            if support & needed != needed {
                bail!("this GPU can't draw into textures of DXGI format {} (format support {support:#x})", format.0);
            }
            let mut corner = None;
            gpu.device.CreateVertexShader(&compile(s!("corner"), s!("vs_5_0"))?, None, Some(&mut corner)).context("the vertex shader")?;
            let pixel = |entry: PCSTR| -> Result<ID3D11PixelShader> {
                let mut shader = None;
                gpu.device.CreatePixelShader(&compile(entry, s!("ps_5_0"))?, None, Some(&mut shader)).with_context(|| format!("the {} shader", entry.to_string().unwrap_or_default()))?;
                shader.context("no shader")
            };

            let (input_kind, white) = match input {
                Input::Encoded => (0, 1.0),
                Input::Linear { white } => (1, white),
                Input::Pq { white } => (2, white),
            };
            let (pq_out, sdr_white_nits) = match output {
                Output::Sdr => (0, 0.0),
                Output::Pq { sdr_white } => (1, sdr_white),
            };
            let p = Params { input_kind, white, pq_out, sdr_white_nits, height, pointer_on: 0, pointer_at: [0; 2], pointer_size: [0; 2], unused: [0; 2] };
            let desc = D3D11_BUFFER_DESC { ByteWidth: size_of::<Params>() as u32, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
            let mut params = None;
            gpu.device.CreateBuffer(&desc, Some(&D3D11_SUBRESOURCE_DATA { pSysMem: &p as *const Params as *const c_void, ..Default::default() }), Some(&mut params)).context("the shader's parameters")?;

            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: if full_chroma { height * 3 } else { height },
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                ..Default::default()
            };
            let mut texture = None;
            gpu.device.CreateTexture2D(&desc, None, Some(&mut texture)).with_context(|| format!("a {width}x{} texture of DXGI format {}", desc.Height, format.0))?;
            let texture: ID3D11Texture2D = texture.context("no texture")?;
            // A view of a P010 texture shows one plane, picked by the view's format.
            let view = |format: DXGI_FORMAT| {
                let desc = D3D11_RENDER_TARGET_VIEW_DESC { Format: format, ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D, Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 } } };
                let mut view = None;
                gpu.device.CreateRenderTargetView(&texture, Some(&desc), Some(&mut view)).with_context(|| format!("a view of the encoder's input (format {})", format.0))?;
                view.context("no view")
            };
            let layout = if full_chroma {
                Layout::Planar { shader: pixel(if planar_uint { s!("planar_uint") } else { s!("planar") })?, target: view(format)? }
            } else {
                Layout::Semi { luma: pixel(s!("luma"))?, chroma: pixel(s!("chroma"))?, luma_plane: view(DXGI_FORMAT_R16_UNORM)?, chroma_plane: view(DXGI_FORMAT_R16G16_UNORM)? }
            };
            Ok(Self { corner: corner.context("no shader")?, layout, params: params.context("no buffer")?, output: texture, width, height, p, pointer: None, pointer_serial: 0 })
        }
    }

    /// Draw the pointer into the pictures converted from now on, as `now` has it.
    pub fn set_pointer(&mut self, gpu: &Gpu, now: &crate::cursor::Pointer) {
        if now.shape_serial != self.pointer_serial {
            self.pointer_serial = now.shape_serial;
            self.pointer = now.shape.as_ref().filter(|s| s.width > 0 && s.height > 0 && s.rgba.len() as u32 >= s.width * s.height * 4).and_then(|s| {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: s.width,
                    Height: s.height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    ..Default::default()
                };
                let data = D3D11_SUBRESOURCE_DATA { pSysMem: s.rgba.as_ptr() as *const c_void, SysMemPitch: s.width * 4, SysMemSlicePitch: 0 };
                // SAFETY: the pixels live through the call; out-parameters are live.
                unsafe {
                    let mut texture = None;
                    gpu.device.CreateTexture2D(&desc, Some(&data), Some(&mut texture)).ok()?;
                    let mut view = None;
                    gpu.device.CreateShaderResourceView(&texture?, None, Some(&mut view)).ok()?;
                    view
                }
            });
            self.p.pointer_size = now.shape.as_ref().map_or([0; 2], |s| [s.width, s.height]);
        }
        self.p.pointer_on = (now.visible && self.pointer.is_some()) as u32;
        self.p.pointer_at = [now.x, now.y];
        // SAFETY: the buffer is ours and as large as the parameters.
        unsafe { gpu.context.UpdateSubresource(&self.params, 0, None, &self.p as *const Params as *const c_void, 0, 0) };
    }

    /// Convert `source` (the driver's texture, held by the caller) into the output.
    pub fn convert(&self, gpu: &Gpu, source: &ID3D11ShaderResourceView) {
        let c = &gpu.context;
        // SAFETY: plain state-setting and draw calls on live objects.
        unsafe {
            c.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            c.IASetInputLayout(None);
            c.VSSetShader(&self.corner, None);
            c.PSSetShaderResources(0, Some(&[Some(source.clone()), self.pointer.clone()]));
            c.PSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            let draw = |shader: &ID3D11PixelShader, plane: &ID3D11RenderTargetView, width: u32, height: u32| {
                c.PSSetShader(shader, None);
                c.OMSetRenderTargets(Some(&[Some(plane.clone())]), None);
                c.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: width as f32, Height: height as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
                c.Draw(3, 0);
            };
            match &self.layout {
                Layout::Semi { luma, chroma, luma_plane, chroma_plane } => {
                    draw(luma, luma_plane, self.width, self.height);
                    draw(chroma, chroma_plane, self.width / 2, self.height / 2);
                }
                Layout::Planar { shader, target } => draw(shader, target, self.width, self.height * 3),
            }
            // Let go of both: the driver's texture goes back to the driver, and the output is
            // the encoder's to read.
            c.PSSetShaderResources(0, Some(&[None, None]));
            c.OMSetRenderTargets(None, None);
        }
    }
}
