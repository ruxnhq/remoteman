//! The CUDA driver (`nvcuda.dll`, loaded when wanted), only as far as moving a picture that
//! NVDEC decoded (FFmpeg's CUDA hardware device) into Direct3D 11 textures on the same card, with
//! no trip through memory: the planes are copied device to array from the decoder's pointers
//! into textures registered with CUDA once. Where the DLL, the card or a call isn't there,
//! the caller decodes on the CPU instead.

use std::ffi::c_void;

use anyhow::{Context, Result, bail};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{Interface, PCSTR, w};

use crate::gpu::Gpu;

type Result_ = i32;
pub type Context_ = *mut c_void;
pub type Resource = *mut c_void;
type Stream = *mut c_void;

const MEMORY_DEVICE: u32 = 2;
const MEMORY_ARRAY: u32 = 3;

/// `CUDA_MEMCPY2D` (the v2 layout).
#[repr(C)]
struct Memcpy2D {
    src_x_in_bytes: usize,
    src_y: usize,
    src_memory_type: u32,
    src_host: *const c_void,
    src_device: u64,
    src_array: *mut c_void,
    src_pitch: usize,
    dst_x_in_bytes: usize,
    dst_y: usize,
    dst_memory_type: u32,
    dst_host: *mut c_void,
    dst_device: u64,
    dst_array: *mut c_void,
    dst_pitch: usize,
    width_in_bytes: usize,
    height: usize,
}

/// FFmpeg's `AVCUDADeviceContext`: the context it made and the stream it decodes on.
#[repr(C)]
pub struct AvCudaDevice {
    pub cuda_ctx: Context_,
    pub stream: Stream,
}

/// A plane in the decoder's memory: its device pointer, pitch, width in bytes and rows.
pub struct Plane {
    pub ptr: u64,
    pub pitch: usize,
    pub width: usize,
    pub height: usize,
}

#[derive(Clone, Copy)]
struct Api {
    init: unsafe extern "system" fn(u32) -> Result_,
    d3d11_device: unsafe extern "system" fn(*mut i32, *mut c_void) -> Result_,
    push: unsafe extern "system" fn(Context_) -> Result_,
    pop: unsafe extern "system" fn(*mut Context_) -> Result_,
    register: unsafe extern "system" fn(*mut Resource, *mut c_void, u32) -> Result_,
    unregister: unsafe extern "system" fn(Resource) -> Result_,
    map: unsafe extern "system" fn(u32, *mut Resource, Stream) -> Result_,
    unmap: unsafe extern "system" fn(u32, *mut Resource, Stream) -> Result_,
    mapped_array: unsafe extern "system" fn(*mut *mut c_void, Resource, u32, u32) -> Result_,
    copy: unsafe extern "system" fn(*const Memcpy2D, Stream) -> Result_,
    sync: unsafe extern "system" fn(Stream) -> Result_,
}

fn check(what: &str, r: Result_) -> Result<()> {
    anyhow::ensure!(r == 0, "{what}: CUDA error {r}");
    Ok(())
}

impl Api {
    fn load() -> Result<Self> {
        // SAFETY: loading a system DLL by name; each export is cast to the signature the CUDA
        // driver API documents for it.
        unsafe {
            let lib = LoadLibraryW(w!("nvcuda.dll")).context("no nvcuda.dll (no NVIDIA driver)")?;
            macro_rules! f {
                ($name:literal) => {
                    std::mem::transmute(GetProcAddress(lib, PCSTR(concat!($name, "\0").as_ptr())).with_context(|| format!("nvcuda.dll has no {}", $name))?)
                };
            }
            Ok(Self {
                init: f!("cuInit"),
                d3d11_device: f!("cuD3D11GetDevice"),
                push: f!("cuCtxPushCurrent_v2"),
                pop: f!("cuCtxPopCurrent_v2"),
                register: f!("cuGraphicsD3D11RegisterResource"),
                unregister: f!("cuGraphicsUnregisterResource"),
                map: f!("cuGraphicsMapResources"),
                unmap: f!("cuGraphicsUnmapResources"),
                mapped_array: f!("cuGraphicsSubResourceGetMappedArray"),
                copy: f!("cuMemcpy2DAsync_v2"),
                sync: f!("cuStreamSynchronize"),
            })
        }
    }
}

/// The CUDA ordinal of the card `gpu` is on (what FFmpeg's CUDA device takes as its name).
pub fn ordinal(gpu: &Gpu) -> Result<String> {
    let api = Api::load()?;
    // SAFETY: calls on interfaces we hold and the driver's documented calls.
    unsafe {
        check("cuInit", (api.init)(0))?;
        let adapter: IDXGIAdapter = gpu.device.cast::<IDXGIDevice>()?.GetAdapter()?;
        let mut dev = -1;
        check("cuD3D11GetDevice", (api.d3d11_device)(&mut dev, adapter.as_raw()))?;
        Ok(dev.to_string())
    }
}

/// FFmpeg's CUDA context, current on this thread for as long as a guard lives.
#[derive(Clone, Copy)]
pub struct Cuda {
    api: Api,
    ctx: Context_,
    stream: Stream,
}

struct Current<'a>(&'a Cuda);

impl Drop for Current<'_> {
    fn drop(&mut self) {
        let mut old = std::ptr::null_mut();
        // SAFETY: pops the context pushed in `current`.
        unsafe { (self.0.api.pop)(&mut old) };
    }
}

impl Cuda {
    /// `device` is the AVCUDADeviceContext of an FFmpeg CUDA device.
    ///
    /// SAFETY: `device` stays valid for as long as this is used.
    pub unsafe fn new(device: *const AvCudaDevice) -> Result<Self> {
        let api = Api::load()?;
        // SAFETY: as documented.
        let d = unsafe { &*device };
        anyhow::ensure!(!d.cuda_ctx.is_null(), "FFmpeg's CUDA device has no context");
        Ok(Self { api, ctx: d.cuda_ctx, stream: d.stream })
    }

    fn current(&self) -> Result<Current<'_>> {
        // SAFETY: the context is FFmpeg's, alive while the decoder is.
        check("cuCtxPushCurrent", unsafe { (self.api.push)(self.ctx) })?;
        Ok(Current(self))
    }

    /// Make `texture` a copy target. It must be a plain texture of a format CUDA knows (R8,
    /// R16 UNORM, ...).
    pub fn register(&self, texture: &ID3D11Texture2D) -> Result<Resource> {
        let _c = self.current()?;
        let mut res = std::ptr::null_mut();
        // SAFETY: the texture is alive; flags 0 is "none".
        check("cuGraphicsD3D11RegisterResource", unsafe { (self.api.register)(&mut res, texture.as_raw(), 0) })?;
        Ok(res)
    }

    pub fn unregister(&self, resources: &[Resource]) {
        if let Ok(_c) = self.current() {
            for r in resources {
                // SAFETY: registered by `register`.
                unsafe { (self.api.unregister)(*r) };
            }
        }
    }

    /// Copy the planes, from the decoder's memory into the textures behind `to`, and wait
    /// for it: when this returns the textures hold the picture.
    pub fn copy(&self, to: &[Resource], from: &[Plane]) -> Result<()> {
        let _c = self.current()?;
        let mut res = to.to_vec();
        // SAFETY: the resources are registered textures nobody else uses now (the sets are
        // used in turn); the planes are the decoder's, alive until the frame is released. The
        // mapping is undone on every path.
        unsafe {
            check("cuGraphicsMapResources", (self.api.map)(res.len() as u32, res.as_mut_ptr(), self.stream))?;
            let done = (|| -> Result<()> {
                for (r, p) in res.iter().zip(from) {
                    let mut array = std::ptr::null_mut();
                    check("cuGraphicsSubResourceGetMappedArray", (self.api.mapped_array)(&mut array, *r, 0, 0))?;
                    let c = Memcpy2D {
                        src_x_in_bytes: 0,
                        src_y: 0,
                        src_memory_type: MEMORY_DEVICE,
                        src_host: std::ptr::null(),
                        src_device: p.ptr,
                        src_array: std::ptr::null_mut(),
                        src_pitch: p.pitch,
                        dst_x_in_bytes: 0,
                        dst_y: 0,
                        dst_memory_type: MEMORY_ARRAY,
                        dst_host: std::ptr::null_mut(),
                        dst_device: 0,
                        dst_array: array,
                        dst_pitch: 0,
                        width_in_bytes: p.width,
                        height: p.height,
                    };
                    check("cuMemcpy2DAsync", (self.api.copy)(&c, self.stream))?;
                }
                check("cuStreamSynchronize", (self.api.sync)(self.stream))
            })();
            let un = check("cuGraphicsUnmapResources", (self.api.unmap)(res.len() as u32, res.as_mut_ptr(), self.stream));
            done.and(un)
        }
    }
}

/// Whether FFmpeg and this card can decode with NVDEC into our textures: nvcuda.dll loads and
/// the card the device is on is a CUDA one. (The decoder is only really tried at its first
/// frame; a failure there falls back to the CPU.)
pub fn usable(gpu: &Gpu) -> Result<()> {
    if std::env::var("REMOTEMAN_CUDA").as_deref() == Ok("0") {
        bail!("REMOTEMAN_CUDA=0");
    }
    ordinal(gpu).map(|_| ())
}

/// Silence the unused-constant lint where only one of the memory kinds is needed.
const _: (u32, u32) = (MEMORY_DEVICE, MEMORY_ARRAY);
