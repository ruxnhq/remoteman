//! Capture of one of the PC's own screens, as it is, through DXGI Desktop Duplication: for a
//! device that asks to `mirror` on a PC that has screens. Nothing is added to the desktop or
//! moved on it.
//!
//! Each new desktop image is copied into a texture of this module's, which the conversion reads
//! like the driver's texture: Desktop Duplication hands a frame over only until it is released,
//! and the same picture is wanted again for a keyframe or a new client. A screen that is still
//! gives no frames (the copy is converted again when asked for). Desktop Duplication shows
//! the pointer apart, as the driver does, so the pointer is drawn into the picture or reported
//! to the client the way it is for the driver's monitor.
//!
//! The duplication is lost when the desktop changes (the lock screen, a permission prompt, a
//! mode or HDR switch): it is made again, on the desktop that takes input now (the host's
//! service runs as SYSTEM, which may open them all; any other process is refused on the secure
//! one and keeps trying). If the screen is another size or format after that, [`Take::Stale`]
//! says so and the picture starts afresh, as it does for the driver's channel.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11ShaderResourceView, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_DESC, DXGI_OUTDUPL_FRAME_INFO, IDXGIFactory1, IDXGIOutput5, IDXGIOutput6, IDXGIOutputDuplication,
    IDXGIResource,
};
use windows::Win32::System::Performance::QueryPerformanceFrequency;
use windows::core::Interface;

use crate::capture::{Gpu, Take};
use crate::vdd::Metadata;

/// The adapter's LUID and the output of Windows' display device `device` (`\\.\DISPLAY1`).
fn locate(device: &str) -> Result<((u32, i32), IDXGIOutput6)> {
    // SAFETY: plain enumeration calls; the out-parameters are live for each call.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().context("CreateDXGIFactory1")?;
        for i in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(i) else { break };
            for j in 0.. {
                let Ok(output) = adapter.EnumOutputs(j) else { break };
                let desc = output.GetDesc().context("an output's description")?;
                let name = String::from_utf16_lossy(&desc.DeviceName).trim_end_matches('\0').to_owned();
                if name.eq_ignore_ascii_case(device) {
                    let luid = adapter.GetDesc1().context("the adapter's description")?.AdapterLuid;
                    let output6 = output.cast::<IDXGIOutput6>().context("IDXGIOutput6 (Windows 10 1803 or later)")?;
                    return Ok(((luid.LowPart, luid.HighPart), output6));
                }
            }
        }
    }
    bail!("DXGI lists no output {device} (is it part of the desktop?)")
}

/// The adapter the screen `device` is driven by: the device its picture is converted and
/// encoded on has to be there, as Desktop Duplication needs it.
pub fn adapter_of(device: &str) -> Result<(u32, i32)> {
    locate(device).map(|(luid, _)| luid)
}

/// The screen as Desktop Duplication has it.
pub struct Duplication {
    device: String,
    dup: Option<IDXGIOutputDuplication>,
    copy: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
    /// What the screen was when this was opened.
    pub meta: Metadata,
    /// The copy holds a picture.
    have: bool,
    /// The next frame acquired counts as a new picture whatever it says (the first after the
    /// duplication was made shows what was on the screen already).
    first: bool,
    counter: u64,
    retry_at: Instant,
    lost_in_a_row: u32,
    timeouts: u64,
    qpc_frequency: u64,
}

/// What is asked of Windows for the frames: HDR as 16-bit float scRGB if the screen is in HDR,
/// else 8-bit.
fn duplicate(gpu: &Gpu, output: &IDXGIOutput6, hdr_on: bool) -> Result<(IDXGIOutputDuplication, DXGI_OUTDUPL_DESC)> {
    let formats: &[DXGI_FORMAT] = if hdr_on { &[DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM] } else { &[DXGI_FORMAT_B8G8R8A8_UNORM] };
    // SAFETY: plain calls on live interfaces; the description is the out-parameter.
    unsafe {
        let dup = output.cast::<IDXGIOutput5>().context("IDXGIOutput5")?.DuplicateOutput1(&gpu.device, 0, formats).context("DuplicateOutput1")?;
        let desc = dup.GetDesc();
        Ok((dup, desc))
    }
}

impl Duplication {
    /// Duplicate the screen `device` on `gpu`'s device, which must be on the screen's adapter.
    pub fn open(gpu: &Gpu, device: &str) -> Result<Self> {
        crate::desk::follow();
        let (luid, output) = locate(device)?;
        ensure!(luid == gpu.luid, "{device} is driven by another adapter ({:08x}:{:08x}) than the one the picture is made on", luid.1, luid.0);
        let hdr_on = crate::hdr::state(device).is_some_and(|(_, on)| on);
        let (dup, desc) = duplicate(gpu, &output, hdr_on)?;
        let (width, height, format) = (desc.ModeDesc.Width, desc.ModeDesc.Height, desc.ModeDesc.Format);
        ensure!(desc.Rotation == DXGI_MODE_ROTATION_IDENTITY || desc.Rotation == DXGI_MODE_ROTATION_UNSPECIFIED, "{device} is rotated (rotation {}), which this host doesn't straighten", desc.Rotation.0);
        let wide = format == DXGI_FORMAT_R16G16B16A16_FLOAT;
        let texture = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let (mut copy, mut view) = (None, None);
        // SAFETY: plain calls; the out-parameters are live for each call.
        unsafe {
            gpu.device.CreateTexture2D(&texture, None, Some(&mut copy)).context("a texture for the screen's picture")?;
            let copy_ref = copy.as_ref().context("no texture")?;
            gpu.device.CreateShaderResourceView(copy_ref, None, Some(&mut view)).context("a view of the screen's picture")?;
        }
        let (copy, view) = (copy.context("no texture")?, view.context("no view")?);
        let (mut max_nits, mut min_nits, mut max_fall) = (0.0, 0.0, 0.0);
        if wide {
            // SAFETY: a live interface.
            if let Ok(d) = unsafe { output.GetDesc1() } {
                (max_nits, min_nits, max_fall) = (d.MaxLuminance, d.MinLuminance, d.MaxFullFrameLuminance);
            }
        }
        // SAFETY: all of Metadata's fields are plain numbers.
        let mut meta: Metadata = unsafe { std::mem::zeroed() };
        (meta.width, meta.height, meta.format, meta.is_hdr) = (width, height, format.0 as u32, wide as u32);
        (meta.max_nits, meta.min_nits, meta.max_fall) = (max_nits, min_nits, max_fall);
        (meta.luid_low, meta.luid_high) = luid;
        let mut frequency = 0i64;
        // SAFETY: a live out-parameter.
        let _ = unsafe { QueryPerformanceFrequency(&mut frequency) };
        meta.qpc_frequency = frequency as u64;
        tracing::info!(device, width, height, format = format.0, hdr = wide, max_nits, "screen duplicated (DXGI Desktop Duplication)");
        Ok(Self { device: device.into(), dup: Some(dup), copy, view, meta, have: false, first: true, counter: 0, retry_at: Instant::now(), lost_in_a_row: 0, timeouts: 0, qpc_frequency: frequency as u64 })
    }

    /// The number of frames taken so far.
    pub fn frames(&self) -> u64 {
        self.counter
    }

    /// Hand the newest picture to `read`, if there is one the caller hasn't had (`again`: the
    /// newest even if it was had before). Waits for a new desktop image up to `wait`.
    pub fn take<R>(&mut self, gpu: &Gpu, again: bool, wait: Duration, read: impl FnOnce(&ID3D11ShaderResourceView, &Metadata) -> R) -> Result<Take<R>> {
        if again && self.have {
            let meta = Metadata { present_qpc: 0, ..self.meta };
            return Ok(Take::Frame(read(&self.view, &meta)));
        }
        if self.dup.is_none() {
            // Lost (a new desktop, a new mode): made again when Windows allows.
            let now = Instant::now();
            if now < self.retry_at {
                std::thread::sleep(wait.min(self.retry_at - now));
                return Ok(Take::Nothing);
            }
            self.retry_at = now + Duration::from_millis(250);
            match Self::open(gpu, &self.device) {
                Ok(new) => {
                    if (new.meta.width, new.meta.height, new.meta.format, new.meta.is_hdr) != (self.meta.width, self.meta.height, self.meta.format, self.meta.is_hdr) {
                        return Ok(Take::Stale);
                    }
                    self.dup = new.dup;
                    self.first = true;
                    self.lost_in_a_row = 0;
                    tracing::info!(device = self.device, "the screen's duplication is made again");
                }
                Err(e) => {
                    if self.lost_in_a_row % 20 == 0 {
                        tracing::info!(device = self.device, "the screen can't be duplicated at the moment ({e:#}); trying again");
                    }
                    self.lost_in_a_row += 1;
                    return Ok(Take::Nothing);
                }
            }
        }
        let dup = self.dup.as_ref().context("no duplication")?;
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: live out-parameters; the frame is released below before anything else is done.
        let acquired = unsafe { dup.AcquireNextFrame(wait.as_millis() as u32, &mut info, &mut resource) };
        if let Err(e) = acquired {
            if e.code() == DXGI_ERROR_WAIT_TIMEOUT {
                self.timeouts += 1;
                if self.timeouts % 250 == 0 {
                    tracing::debug!(device = self.device, timeouts = self.timeouts, frames = self.counter, "the screen shows no change");
                }
                return Ok(Take::Nothing);
            }
            if e.code() == DXGI_ERROR_ACCESS_LOST {
                tracing::info!(device = self.device, "the screen's duplication was lost (another desktop, or a new mode)");
            } else {
                tracing::warn!(device = self.device, "the screen's duplication failed: {e}");
            }
            self.dup = None;
            self.retry_at = Instant::now();
            return Ok(Take::Nothing);
        }
        tracing::trace!(present = info.LastPresentTime, mouse = info.LastMouseUpdateTime, accumulated = info.AccumulatedFrames, metadata = info.TotalMetadataBufferSize, "screen frame");
        // A frame that only moved the pointer has no new desktop image.
        let new_image = info.LastPresentTime != 0 || std::mem::take(&mut self.first);
        let mut copied = Ok(());
        if new_image {
            copied = resource.as_ref().context("no frame").and_then(|r| r.cast::<ID3D11Texture2D>().context("the frame as a texture")).map(|texture| {
                // SAFETY: both are live textures of this device and of one size and format.
                unsafe { gpu.context.CopyResource(&self.copy, &texture) };
            });
        }
        drop(resource);
        // SAFETY: the frame is held.
        let released = unsafe { dup.ReleaseFrame() };
        copied?;
        if let Err(e) = released {
            tracing::warn!(device = self.device, "releasing the screen's frame: {e}");
            self.dup = None;
        }
        if !new_image {
            return Ok(Take::Nothing);
        }
        self.have = true;
        self.counter += 1;
        let present = if self.qpc_frequency > 0 { info.LastPresentTime as u64 } else { 0 };
        let meta = Metadata { present_qpc: present, frame_counter: self.counter, ..self.meta };
        Ok(Take::Frame(read(&self.view, &meta)))
    }
}
