//! Capture: a Direct3D 11 device on the adapter the display driver renders on, the driver's
//! shared textures opened on it, and the newest frame handed over while its texture is held.
//! Nothing is copied here; the conversion reads the driver's texture directly.

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use windows::Win32::Foundation::{HMODULE, LUID, S_OK};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Multithread, ID3D11ShaderResourceView,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory4, IDXGIKeyedMutex};
use windows::core::Interface;

use crate::vdd::{Channel, Metadata};

/// The Direct3D device everything of the picture happens on: the driver's textures are read,
/// converted and encoded on it without leaving the GPU.
pub struct Gpu {
    pub device: ID3D11Device,
    pub device1: ID3D11Device1,
    pub context: ID3D11DeviceContext,
    /// The adapter's name, for the log.
    pub adapter: String,
    pub luid: (u32, i32),
    /// The adapter maker's PCI id (0x10de NVIDIA, 0x1002 AMD, 0x8086 Intel).
    pub vendor: u32,
}

impl Gpu {
    /// A device on the adapter with this LUID: the one the display driver renders on, since
    /// shared textures open only on the adapter that made them.
    pub fn on_adapter(luid: (u32, i32)) -> Result<Self> {
        // SAFETY: the calls as documented; out-parameters are live for each call.
        unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory1().context("CreateDXGIFactory1")?;
            let adapter: IDXGIAdapter1 = factory
                .EnumAdapterByLuid(LUID { LowPart: luid.0, HighPart: luid.1 })
                .with_context(|| format!("no display adapter with the LUID {:08x}:{:08x} the driver renders on", luid.1, luid.0))?;
            let desc = adapter.GetDesc1().context("the adapter's description")?;
            let name = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_owned();
            let (mut device, mut context, mut level) = (None, None, D3D_FEATURE_LEVEL::default());
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut context),
            )
            .with_context(|| format!("a Direct3D 11 device on {name}"))?;
            let (device, context): (ID3D11Device, ID3D11DeviceContext) = (device.context("no device")?, context.context("no device context")?);
            // The encoder works on this device from threads of its own.
            let _ = context.cast::<ID3D11Multithread>().context("ID3D11Multithread")?.SetMultithreadProtected(true);
            let device1 = device.cast::<ID3D11Device1>().context("ID3D11Device1 (Direct3D 11.1), which opening the driver's textures needs")?;
            tracing::info!(adapter = %name, vendor = format!("{:04x}", desc.VendorId), feature_level = format!("{:x}", level.0), "Direct3D device");
            Ok(Self { device, device1, context, adapter: name, luid, vendor: desc.VendorId })
        }
    }
}

/// The frame channel with its textures opened on a device.
pub struct Source {
    channel: Channel,
    textures: Vec<ID3D11Texture2D>,
    mutexes: Vec<IDXGIKeyedMutex>,
    views: Vec<ID3D11ShaderResourceView>,
    /// What the channel held when it was opened: the picture's size and format for as long as
    /// this source is good.
    pub meta: Metadata,
    /// The counter of the last frame taken.
    last: Option<u64>,
    /// Frames that were announced but whose texture couldn't be had, in a row.
    missed: u32,
}

/// What [`Source::take`] found.
pub enum Take<R> {
    Frame(R),
    /// No frame newer than the last one taken.
    Nothing,
    /// The driver made its textures anew (another size or format): this source shows a
    /// picture that no longer changes. Open the channel again.
    Stale,
}

impl Source {
    pub fn open(gpu: &Gpu, channel: Channel) -> Result<Self> {
        let meta = channel.snapshot().context("the frame channel's metadata isn't readable (never whole, or not this driver's)")?;
        ensure!((meta.luid_low, meta.luid_high) == gpu.luid, "the driver now renders on another adapter ({:08x}:{:08x})", meta.luid_high, meta.luid_low);
        let (mut textures, mut mutexes, mut views) = (Vec::new(), Vec::new(), Vec::new());
        for (i, handle) in channel.slots.iter().enumerate() {
            // SAFETY: a handle to a shared texture, which the driver gave this process.
            unsafe {
                let texture: ID3D11Texture2D = gpu.device1.OpenSharedResource1(*handle).with_context(|| format!("opening the driver's texture {i} (OpenSharedResource1)"))?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                texture.GetDesc(&mut desc);
                ensure!((desc.Width, desc.Height) == (meta.width, meta.height), "the driver's texture {i} is {}x{}, its metadata says {}x{}", desc.Width, desc.Height, meta.width, meta.height);
                let mut view = None;
                gpu.device.CreateShaderResourceView(&texture, None, Some(&mut view)).with_context(|| format!("a view of the driver's texture {i} (format {})", desc.Format.0))?;
                mutexes.push(texture.cast::<IDXGIKeyedMutex>().with_context(|| format!("the keyed mutex of the driver's texture {i}"))?);
                views.push(view.context("no view")?);
                textures.push(texture);
            }
        }
        Ok(Self { channel, textures, mutexes, views, meta, last: None, missed: 0 })
    }

    pub fn channel(&self) -> &Channel {
        &self.channel
    }

    /// Take a texture's keyed mutex if it is released with `key` now or within `wait`.
    fn acquire(&self, slot: usize, key: u64, wait: Duration) -> windows::core::HRESULT {
        let m = &self.mutexes[slot];
        // Called through the table because a wait that ran out is a success code (WAIT_TIMEOUT),
        // which the wrapped call would report as having the mutex.
        // SAFETY: a live interface and its own method.
        unsafe { (Interface::vtable(m).AcquireSync)(Interface::as_raw(m), key, wait.as_millis() as u32) }
    }

    /// Hand the newest frame to `read`, if there is one the caller hasn't had: `read` gets a
    /// view of the driver's texture, which is held for as long as it runs and given back to the
    /// driver after the GPU has been told everything `read` asked of it.
    ///
    /// `again`: take the newest frame even if it was taken before (a client that has just
    /// connected to a still desktop needs a first picture).
    pub fn take<R>(&mut self, gpu: &Gpu, again: bool, read: impl FnOnce(&ID3D11ShaderResourceView, &Metadata) -> R) -> Result<Take<R>> {
        let Some(meta) = self.channel.snapshot() else { return Ok(Take::Nothing) };
        if meta.generation() != self.meta.generation() || (meta.width, meta.height, meta.format) != (self.meta.width, self.meta.height, self.meta.format) {
            return Ok(Take::Stale);
        }
        if meta.frame_counter == 0 || (!again && self.last == Some(meta.frame_counter)) {
            return Ok(Take::Nothing);
        }
        let slot = meta.slot as usize;
        // Key 1: a frame the driver has put there and nobody has read. For a frame read before
        // (key 0, the driver's own key) only when asked to take it again.
        let mut hr = self.acquire(slot, 1, Duration::from_millis(if again { 0 } else { 4 }));
        if hr != S_OK && again {
            hr = self.acquire(slot, 0, Duration::ZERO);
        }
        if hr != S_OK {
            // The driver is writing the next frame into it, and will say so when it is done.
            // If this goes on with frames coming all the while, the keys aren't what this
            // code takes them for.
            self.missed += 1;
            if self.missed == 30 {
                tracing::warn!(slot, result = format!("{:#x}", hr.0), counter = meta.frame_counter, "30 frames in a row were announced but their texture couldn't be taken");
            }
            return Ok(Take::Nothing);
        }
        self.missed = 0;
        let result = read(&self.views[slot], &meta);
        // SAFETY: plain calls on live interfaces; the mutex is held by this thread.
        unsafe {
            // The driver may write to the texture as soon as it has the mutex: the GPU must
            // have this side's reads queued before that.
            gpu.context.Flush();
            self.mutexes[slot].ReleaseSync(0).context("giving the driver its texture back (ReleaseSync)")?;
        }
        self.last = Some(meta.frame_counter);
        Ok(Take::Frame(result))
    }

    /// The driver's own counts of frames it replaced before they were read, and frames it
    /// dropped because a texture was held or couldn't be had, for the log.
    pub fn counts(&self) -> Option<(u64, u64, u64, u64)> {
        self.channel.snapshot().map(|m| (m.frame_counter, m.replaced_unread, m.dropped_held, m.dropped_failed))
    }

    /// The textures' format as Direct3D says it (the metadata says the same of the surface
    /// Windows handed the driver).
    pub fn format(&self) -> Result<D3D11_TEXTURE2D_DESC> {
        let Some(texture) = self.textures.first() else { bail!("the frame channel has no textures") };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a live texture.
        unsafe { texture.GetDesc(&mut desc) };
        Ok(desc)
    }
}
