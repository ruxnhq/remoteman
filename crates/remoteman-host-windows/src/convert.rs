//! Convert: the driver's RGB texture to the encoder's P010 input in one go on the GPU (two
//! draws, one for each plane of the P010 texture). See `convert.hlsl` for the numbers.

use std::ffi::c_void;

use anyhow::{Context, Result, bail};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BUFFER_DESC, D3D11_FORMAT_SUPPORT_RENDER_TARGET, D3D11_FORMAT_SUPPORT_TEXTURE2D, D3D11_RENDER_TARGET_VIEW_DESC,
    D3D11_RENDER_TARGET_VIEW_DESC_0, D3D11_RTV_DIMENSION_TEXTURE2D, D3D11_SUBRESOURCE_DATA, D3D11_TEX2D_RTV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT, ID3D11Buffer, ID3D11PixelShader,
    ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_P010, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
    DXGI_SAMPLE_DESC,
};
use windows::core::{PCSTR, s};

use crate::capture::Gpu;

const SOURCE: &str = include_str!("convert.hlsl");

/// How the driver's texture holds the picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    /// As painted, gamma-encoded: an 8- or 10-bit desktop surface.
    Encoded,
    /// Linear light where 1.0 is 80 nits (a 16-bit float surface); the client's white is this
    /// many times that.
    Linear { white: f32 },
}

/// What kind of picture a surface of this format holds, and what to say about it in the log.
/// HDR is not built: an HDR desktop is squeezed into SDR so that there is a picture at all.
pub fn input_for(format: u32, is_hdr: bool) -> Result<(Input, Option<&'static str>)> {
    let format = DXGI_FORMAT(format as i32);
    Ok(if format == DXGI_FORMAT_B8G8R8A8_UNORM || format == DXGI_FORMAT_R8G8B8A8_UNORM {
        (Input::Encoded, None)
    } else if format == DXGI_FORMAT_R10G10B10A2_UNORM {
        (Input::Encoded, is_hdr.then_some("the desktop is HDR10, which isn't converted yet: its colours will be wrong"))
    } else if format == DXGI_FORMAT_R16G16B16A16_FLOAT {
        if is_hdr {
            // Windows' usual white for SDR content on an HDR desktop is about 200 nits; what
            // the user set isn't asked for yet.
            (Input::Linear { white: 2.5 }, Some("the desktop is HDR, which isn't streamed as HDR yet: it is clipped to SDR, with white taken to be 200 nits"))
        } else {
            (Input::Linear { white: 1.0 }, Some("the desktop's surface is 16-bit float in SDR: taken to be linear light with white at 1.0"))
        }
    } else {
        bail!("the desktop's surface is in DXGI format {}, which this host doesn't convert", format.0)
    })
}

#[repr(C)]
struct Params {
    linear_in: u32,
    white: f32,
    unused: [u32; 2],
}

pub struct Converter {
    corner: ID3D11VertexShader,
    luma: ID3D11PixelShader,
    chroma: ID3D11PixelShader,
    params: ID3D11Buffer,
    /// The encoder's input.
    pub output: ID3D11Texture2D,
    luma_plane: ID3D11RenderTargetView,
    chroma_plane: ID3D11RenderTargetView,
    width: u32,
    height: u32,
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
    /// A converter for pictures of this size (even both ways, as 4:2:0 needs).
    pub fn new(gpu: &Gpu, width: u32, height: u32, input: Input) -> Result<Self> {
        // SAFETY: Direct3D calls as documented, with live arguments and out-parameters.
        unsafe {
            let support = gpu.device.CheckFormatSupport(DXGI_FORMAT_P010).context("this GPU has no P010 textures")?;
            let needed = (D3D11_FORMAT_SUPPORT_TEXTURE2D.0 | D3D11_FORMAT_SUPPORT_RENDER_TARGET.0) as u32;
            if support & needed != needed {
                bail!("this GPU can't draw into P010 textures (format support {support:#x})");
            }
            let (mut corner, mut luma, mut chroma) = (None, None, None);
            gpu.device.CreateVertexShader(&compile(s!("corner"), s!("vs_5_0"))?, None, Some(&mut corner)).context("the vertex shader")?;
            gpu.device.CreatePixelShader(&compile(s!("luma"), s!("ps_5_0"))?, None, Some(&mut luma)).context("the luma shader")?;
            gpu.device.CreatePixelShader(&compile(s!("chroma"), s!("ps_5_0"))?, None, Some(&mut chroma)).context("the chroma shader")?;

            let p = match input {
                Input::Encoded => Params { linear_in: 0, white: 1.0, unused: [0; 2] },
                Input::Linear { white } => Params { linear_in: 1, white, unused: [0; 2] },
            };
            let desc = D3D11_BUFFER_DESC { ByteWidth: size_of::<Params>() as u32, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
            let mut params = None;
            gpu.device.CreateBuffer(&desc, Some(&D3D11_SUBRESOURCE_DATA { pSysMem: &p as *const Params as *const c_void, ..Default::default() }), Some(&mut params)).context("the shader's parameters")?;

            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_P010,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                ..Default::default()
            };
            let mut output = None;
            gpu.device.CreateTexture2D(&desc, None, Some(&mut output)).with_context(|| format!("a {width}x{height} P010 texture"))?;
            let output: ID3D11Texture2D = output.context("no texture")?;
            // A view of a P010 texture shows one plane, picked by the view's format.
            let plane = |format: DXGI_FORMAT| {
                let desc = D3D11_RENDER_TARGET_VIEW_DESC { Format: format, ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D, Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 } } };
                let mut view = None;
                gpu.device.CreateRenderTargetView(&output, Some(&desc), Some(&mut view)).with_context(|| format!("a view of the P010 texture's plane (format {})", format.0))?;
                view.context("no view")
            };
            Ok(Self {
                corner: corner.context("no shader")?,
                luma: luma.context("no shader")?,
                chroma: chroma.context("no shader")?,
                params: params.context("no buffer")?,
                luma_plane: plane(DXGI_FORMAT_R16_UNORM)?,
                chroma_plane: plane(DXGI_FORMAT_R16G16_UNORM)?,
                output,
                width,
                height,
            })
        }
    }

    /// Convert `source` (the driver's texture, held by the caller) into the output.
    pub fn convert(&self, gpu: &Gpu, source: &ID3D11ShaderResourceView) {
        let c = &gpu.context;
        // SAFETY: plain state-setting and draw calls on live objects.
        unsafe {
            c.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            c.IASetInputLayout(None);
            c.VSSetShader(&self.corner, None);
            c.PSSetShaderResources(0, Some(&[Some(source.clone())]));
            c.PSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            let draw = |shader: &ID3D11PixelShader, plane: &ID3D11RenderTargetView, width: u32, height: u32| {
                c.PSSetShader(shader, None);
                c.OMSetRenderTargets(Some(&[Some(plane.clone())]), None);
                c.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: width as f32, Height: height as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
                c.Draw(3, 0);
            };
            draw(&self.luma, &self.luma_plane, self.width, self.height);
            draw(&self.chroma, &self.chroma_plane, self.width / 2, self.height / 2);
            // Let go of both: the driver's texture goes back to the driver, and the output is
            // the encoder's to read.
            c.PSSetShaderResources(0, Some(&[None]));
            c.OMSetRenderTargets(None, None);
        }
    }
}
