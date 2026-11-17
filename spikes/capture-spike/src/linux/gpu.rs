//! Zero-copy capture: importing the compositor's DMA-BUF frames into CUDA for NVENC without
//! touching CPU memory.
//!
//! The DMA-BUF is mapped into Vulkan (FFmpeg's DRM → Vulkan import, which also waits for the
//! compositor's rendering), copied on the GPU into one of FFmpeg's own Vulkan frames (imported
//! frames can't be shared with CUDA, pool frames can), and from there into a pooled CUDA frame.
//! Both copies stay on the GPU; once they're done the buffer goes straight back to the compositor.

use std::ffi::{CString, c_void};
use std::ptr;

use anyhow::{Context, Result, bail};
use ash::vk;
use ash::vk::Handle;
use ffmpeg_next::ffi::*;

use super::capture::PixelFormat;

const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// DRM fourcc and FFmpeg software format for each capture layout.
fn drm_format(f: PixelFormat) -> (u32, AVPixelFormat) {
    let fourcc = |s: &[u8; 4]| u32::from_le_bytes(*s);
    match f {
        PixelFormat::Bgrx => (fourcc(b"XR24"), AVPixelFormat::AV_PIX_FMT_BGR0),
        PixelFormat::Bgra => (fourcc(b"AR24"), AVPixelFormat::AV_PIX_FMT_BGRA),
        PixelFormat::Rgbx => (fourcc(b"XB24"), AVPixelFormat::AV_PIX_FMT_RGB0),
        PixelFormat::Rgba => (fourcc(b"AB24"), AVPixelFormat::AV_PIX_FMT_RGBA),
    }
}

fn vk_format(f: PixelFormat) -> vk::Format {
    match f {
        PixelFormat::Bgrx | PixelFormat::Bgra => vk::Format::B8G8R8A8_UNORM,
        PixelFormat::Rgbx | PixelFormat::Rgba => vk::Format::R8G8B8A8_UNORM,
    }
}

fn err(e: i32, what: &str) -> anyhow::Error {
    anyhow::anyhow!("{what}: {}", ffmpeg_next::Error::from(e))
}

/// A reference-counted FFmpeg buffer (device or frames context).
struct Buf(*mut AVBufferRef);

impl Drop for Buf {
    fn drop(&mut self) {
        // SAFETY: we own this reference.
        unsafe { av_buffer_unref(&mut self.0) };
    }
}

/// A frame in CUDA memory, ready for NVENC.
pub struct GpuFrame(*mut AVFrame);

// SAFETY: the frame and its CUDA buffer are reference counted and usable from any thread.
unsafe impl Send for GpuFrame {}

impl GpuFrame {
    pub fn as_ptr(&self) -> *const AVFrame {
        self.0
    }

    /// Copy the picture to CPU memory as tightly packed 4-byte pixels.
    pub fn download(&self) -> Result<Vec<u8>> {
        // SAFETY: a fresh CPU frame receives a copy of our CUDA frame.
        unsafe {
            let mut sw = av_frame_alloc();
            let e = av_hwframe_transfer_data(sw, self.0, 0);
            if e < 0 {
                av_frame_free(&mut sw);
                return Err(err(e, "download from GPU"));
            }
            let (w, h, stride) = ((*sw).width as usize, (*sw).height as usize, (*sw).linesize[0] as usize);
            let mut out = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                out.extend_from_slice(std::slice::from_raw_parts((*sw).data[0].add(y * stride), w * 4));
            }
            av_frame_free(&mut sw);
            Ok(out)
        }
    }
}

impl Drop for GpuFrame {
    fn drop(&mut self) {
        // SAFETY: we own this frame.
        unsafe { av_frame_free(&mut self.0) };
    }
}

/// The CUDA calls needed to wait for the GPU copy (FFmpeg's own CUDA loader is private).
struct Cuda {
    _lib: libloading::Library,
    push: unsafe extern "C" fn(*mut c_void) -> i32,
    pop: unsafe extern "C" fn(*mut *mut c_void) -> i32,
    sync: unsafe extern "C" fn(*mut c_void) -> i32,
}

impl Cuda {
    fn load() -> Result<Self> {
        // SAFETY: libcuda's exported functions have these C signatures.
        unsafe {
            let lib = libloading::Library::new("libcuda.so.1").context("load libcuda (NVIDIA driver)")?;
            let push = *lib.get(b"cuCtxPushCurrent_v2\0")?;
            let pop = *lib.get(b"cuCtxPopCurrent_v2\0")?;
            let sync = *lib.get(b"cuStreamSynchronize\0")?;
            Ok(Self { _lib: lib, push, pop, sync })
        }
    }
}

/// Records and submits the Vulkan copy from an imported DMA-BUF into a pool frame, on FFmpeg's
/// Vulkan device.
struct VkCopier {
    device: ash::Device,
    queue: vk::Queue,
    family: u32,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
}

impl VkCopier {
    /// # Safety
    /// `dev` must be a live Vulkan device context.
    unsafe fn new(dev: *mut AVBufferRef) -> Result<Self> {
        // SAFETY: the handles come from FFmpeg's live device context.
        unsafe {
            let ctx = (*dev).data as *const AVHWDeviceContext;
            let vkctx = &*((*ctx).hwctx as *const AVVulkanDeviceContext);
            let gipa: vk::PFN_vkGetInstanceProcAddr =
                std::mem::transmute(vkctx.get_proc_addr.context("no vkGetInstanceProcAddr")?);
            let entry = ash::Entry::from_static_fn(ash::StaticFn { get_instance_proc_addr: gipa });
            let instance = ash::Instance::load(entry.static_fn(), vk::Instance::from_raw(vkctx.inst as u64));
            let device = ash::Device::load(instance.fp_v1_0(), vk::Device::from_raw(vkctx.act_dev as u64));
            // Any family FFmpeg opened that can copy images.
            let wanted = vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER;
            let family = vkctx.qf[..vkctx.nb_qf as usize]
                .iter()
                .find(|q| vk::QueueFlags::from_raw(q.flags as u32).intersects(wanted) && q.num > 0)
                .context("FFmpeg's Vulkan device has no queue that can copy")?
                .idx as u32;
            let queue = device.get_device_queue(family, 0);
            let pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?;
            let cmd = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default().command_pool(pool).command_buffer_count(1),
            )?[0];
            let fence = device.create_fence(&vk::FenceCreateInfo::default(), None)?;
            Ok(Self { device, queue, family, pool, cmd, fence })
        }
    }

    /// Copy `src` into `dst` (same size), waiting until the GPU has finished.
    ///
    /// # Safety
    /// Both frames must be live Vulkan frames of `fc`'s / `dev`'s device.
    unsafe fn copy(
        &self,
        dev: *mut AVBufferRef,
        src_fc: *mut AVBufferRef,
        src: *mut AVFrame,
        dst_fc: *mut AVBufferRef,
        dst: *mut AVFrame,
    ) -> Result<()> {
        // SAFETY: as documented; FFmpeg's frame and queue locks serialise access to the
        // frames' state and the queue.
        unsafe {
            let devctx = (*dev).data as *mut AVHWDeviceContext;
            let vkctx = &*((*devctx).hwctx as *const AVVulkanDeviceContext);
            let (sfc, dfc) = ((*src_fc).data as *mut AVHWFramesContext, (*dst_fc).data as *mut AVHWFramesContext);
            let (svk, dvk) = (&*((*sfc).hwctx as *const AVVulkanFramesContext), &*((*dfc).hwctx as *const AVVulkanFramesContext));
            let (s, d) = ((*src).data[0] as *mut AVVkFrame, (*dst).data[0] as *mut AVVkFrame);
            let (w, h) = ((*src).width as u32, (*src).height as u32);

            if let Some(lock) = svk.lock_frame {
                lock(sfc, s);
            }
            if let Some(lock) = dvk.lock_frame {
                lock(dfc, d);
            }
            let result = (|| -> Result<()> {
                let dev = &self.device;
                let img = |f: *mut AVVkFrame| vk::Image::from_raw((*f).img[0] as u64);
                let sem = |f: *mut AVVkFrame| vk::Semaphore::from_raw((*f).sem[0] as u64);
                let range = vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1);
                // The images must be usable on our queue: shared (concurrent) or owned by our family.
                for f in [s, d] {
                    let q = (*f).queue_family[0];
                    if q != vk::QUEUE_FAMILY_IGNORED && q != self.family {
                        bail!("frame is owned by queue family {q}, not {}", self.family);
                    }
                }
                let barriers = [
                    vk::ImageMemoryBarrier2::default()
                        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                        .src_access_mask(vk::AccessFlags2::from_raw((*s).access[0] as u64))
                        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                        .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                        .old_layout(vk::ImageLayout::from_raw((*s).layout[0] as i32))
                        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(img(s))
                        .subresource_range(range),
                    vk::ImageMemoryBarrier2::default()
                        .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                        .src_access_mask(vk::AccessFlags2::from_raw((*d).access[0] as u64))
                        .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                        .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                        .old_layout(vk::ImageLayout::from_raw((*d).layout[0] as i32))
                        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(img(d))
                        .subresource_range(range),
                ];
                dev.reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())?;
                dev.begin_command_buffer(
                    self.cmd,
                    &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )?;
                dev.cmd_pipeline_barrier2(self.cmd, &vk::DependencyInfo::default().image_memory_barriers(&barriers));
                let layers = vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1);
                dev.cmd_copy_image(
                    self.cmd,
                    img(s),
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    img(d),
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[vk::ImageCopy::default()
                        .src_subresource(layers)
                        .dst_subresource(layers)
                        .extent(vk::Extent3D { width: w, height: h, depth: 1 })],
                );
                dev.end_command_buffer(self.cmd)?;

                let waits = [
                    vk::SemaphoreSubmitInfo::default()
                        .semaphore(sem(s))
                        .value((*s).sem_value[0])
                        .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
                    vk::SemaphoreSubmitInfo::default()
                        .semaphore(sem(d))
                        .value((*d).sem_value[0])
                        .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
                ];
                let signals = [
                    vk::SemaphoreSubmitInfo::default()
                        .semaphore(sem(s))
                        .value((*s).sem_value[0] + 1)
                        .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
                    vk::SemaphoreSubmitInfo::default()
                        .semaphore(sem(d))
                        .value((*d).sem_value[0] + 1)
                        .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
                ];
                let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(self.cmd)];
                let submit =
                    vk::SubmitInfo2::default().wait_semaphore_infos(&waits).command_buffer_infos(&cmds).signal_semaphore_infos(&signals);
                if let Some(lock) = vkctx.lock_queue {
                    lock(devctx, self.family, 0);
                }
                let r = dev.queue_submit2(self.queue, &[submit], self.fence);
                if let Some(unlock) = vkctx.unlock_queue {
                    unlock(devctx, self.family, 0);
                }
                r?;
                // Tell FFmpeg where the frames are now.
                for (f, layout, access) in [
                    (s, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::AccessFlags2::TRANSFER_READ),
                    (d, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::AccessFlags2::TRANSFER_WRITE),
                ] {
                    (*f).layout[0] = std::mem::transmute::<i32, _>(layout.as_raw());
                    (*f).access[0] = std::mem::transmute::<u64, _>(access.as_raw());
                    (*f).sem_value[0] += 1;
                }
                dev.wait_for_fences(&[self.fence], true, 1_000_000_000)?;
                dev.reset_fences(&[self.fence])?;
                Ok(())
            })();
            if let Some(unlock) = dvk.unlock_frame {
                unlock(dfc, d);
            }
            if let Some(unlock) = svk.unlock_frame {
                unlock(sfc, s);
            }
            result
        }
    }
}

impl Drop for VkCopier {
    fn drop(&mut self) {
        // SAFETY: our own objects, idle after the last fence wait.
        unsafe {
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
        }
    }
}

/// Frames contexts for one picture size and layout.
struct Pools {
    key: (u32, u32, PixelFormat),
    drm: Buf,
    vulkan: Buf,
    cuda: Buf,
}

pub struct Importer {
    copier: VkCopier,
    _drm_dev: Buf,
    vk_dev: Buf,
    cuda_dev: Buf,
    cuda: Cuda,
    pools: Option<Pools>,
    /// DRM render node in use, for logs.
    pub node: String,
}

// SAFETY: FFmpeg device and frames contexts are thread safe; the importer is used by one thread
// at a time.
unsafe impl Send for Importer {}

/// The NVIDIA render node (NVENC lives there), else the first one.
fn render_node() -> Option<String> {
    let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri")
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("renderD"))
        .collect();
    nodes.sort();
    let vendor = |n: &str| std::fs::read_to_string(format!("/sys/class/drm/{n}/device/vendor")).unwrap_or_default();
    let nvidia = nodes.iter().find(|n| vendor(n).trim() == "0x10de").cloned();
    nvidia.or_else(|| nodes.first().cloned()).map(|n| format!("/dev/dri/{n}"))
}

impl Importer {
    pub fn new() -> Result<Self> {
        let node = render_node().context("no GPU render node in /dev/dri")?;
        let path = CString::new(node.clone())?;
        // SAFETY: device creation; each success hands us a reference we own.
        unsafe {
            let mut drm = ptr::null_mut();
            let e = av_hwdevice_ctx_create(&mut drm, AVHWDeviceType::AV_HWDEVICE_TYPE_DRM, path.as_ptr(), ptr::null_mut(), 0);
            if e < 0 {
                return Err(err(e, "open DRM device"));
            }
            let drm = Buf(drm);
            let mut vk_dev = ptr::null_mut();
            let e = av_hwdevice_ctx_create_derived(&mut vk_dev, AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN, drm.0, 0);
            if e < 0 {
                return Err(err(e, "Vulkan on the capture GPU"));
            }
            let vk_dev = Buf(vk_dev);
            let mut cuda_dev = ptr::null_mut();
            let e = av_hwdevice_ctx_create_derived(&mut cuda_dev, AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA, vk_dev.0, 0);
            if e < 0 {
                return Err(err(e, "CUDA on the capture GPU"));
            }
            let copier = VkCopier::new(vk_dev.0)?;
            Ok(Self { copier, _drm_dev: drm, vk_dev, cuda_dev: Buf(cuda_dev), cuda: Cuda::load()?, pools: None, node })
        }
    }

    /// DMA-BUF layouts (modifiers) Vulkan can import for `format`, to offer the compositor.
    pub fn modifiers(&self, format: PixelFormat) -> Result<Vec<u64>> {
        // SAFETY: the Vulkan handles come from FFmpeg's live device context; we only query.
        unsafe {
            let dev = (*self.vk_dev.0).data as *const AVHWDeviceContext;
            let vkctx = &*((*dev).hwctx as *const AVVulkanDeviceContext);
            let gipa: vk::PFN_vkGetInstanceProcAddr =
                std::mem::transmute(vkctx.get_proc_addr.context("no vkGetInstanceProcAddr")?);
            let entry = ash::Entry::from_static_fn(ash::StaticFn { get_instance_proc_addr: gipa });
            let instance = ash::Instance::load(entry.static_fn(), vk::Instance::from_raw(vkctx.inst as u64));
            let phys = vk::PhysicalDevice::from_raw(vkctx.phys_dev as u64);

            let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
            let mut props = vk::FormatProperties2::default().push_next(&mut list);
            instance.get_physical_device_format_properties2(phys, vk_format(format), &mut props);
            let n = list.drm_format_modifier_count as usize;
            let mut mods = vec![vk::DrmFormatModifierPropertiesEXT::default(); n];
            let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut mods);
            let mut props = vk::FormatProperties2::default().push_next(&mut list);
            instance.get_physical_device_format_properties2(phys, vk_format(format), &mut props);
            let need = vk::FormatFeatureFlags::TRANSFER_SRC;
            Ok(mods
                .iter()
                .filter(|m| m.drm_format_modifier_plane_count == 1 && m.drm_format_modifier_tiling_features.contains(need))
                .map(|m| m.drm_format_modifier)
                .filter(|&m| m != DRM_FORMAT_MOD_INVALID)
                .collect())
        }
    }

    fn pools(&mut self, w: u32, h: u32, format: PixelFormat) -> Result<&Pools> {
        if self.pools.as_ref().is_none_or(|p| p.key != (w, h, format)) {
            self.pools = None;
            let (_, sw) = drm_format(format);
            // SAFETY: frames contexts on our own devices.
            unsafe {
                let drm_dev = self._drm_dev.0;
                let drm = Buf(av_hwframe_ctx_alloc(drm_dev));
                let c = (*drm.0).data as *mut AVHWFramesContext;
                ((*c).format, (*c).sw_format, (*c).width, (*c).height) = (AVPixelFormat::AV_PIX_FMT_DRM_PRIME, sw, w as i32, h as i32);
                let e = av_hwframe_ctx_init(drm.0);
                if e < 0 {
                    return Err(err(e, "DRM frames"));
                }
                // A plain Vulkan frames context, not one derived from the DRM one: FFmpeg won't copy
                // out of frames whose context was derived.
                let vulkan = Buf(av_hwframe_ctx_alloc(self.vk_dev.0));
                let c = (*vulkan.0).data as *mut AVHWFramesContext;
                ((*c).format, (*c).sw_format, (*c).width, (*c).height) = (AVPixelFormat::AV_PIX_FMT_VULKAN, sw, w as i32, h as i32);
                let e = av_hwframe_ctx_init(vulkan.0);
                if e < 0 {
                    return Err(err(e, "Vulkan frames for DMA-BUF import"));
                }
                let cuda = Buf(av_hwframe_ctx_alloc(self.cuda_dev.0));
                let c = (*cuda.0).data as *mut AVHWFramesContext;
                ((*c).format, (*c).sw_format, (*c).width, (*c).height) = (AVPixelFormat::AV_PIX_FMT_CUDA, sw, w as i32, h as i32);
                let e = av_hwframe_ctx_init(cuda.0);
                if e < 0 {
                    return Err(err(e, "CUDA frames"));
                }
                self.pools = Some(Pools { key: (w, h, format), drm, vulkan, cuda });
            }
        }
        Ok(self.pools.as_ref().unwrap())
    }

    /// Copy one DMA-BUF frame into a new CUDA frame. When this returns, the GPU is done reading
    /// the DMA-BUF and it can be handed back.
    #[allow(clippy::too_many_arguments)]
    pub fn import(
        &mut self,
        fd: i32,
        size: usize,
        offset: u32,
        stride: u32,
        modifier: u64,
        w: u32,
        h: u32,
        format: PixelFormat,
    ) -> Result<GpuFrame> {
        let (fourcc, _) = drm_format(format);
        let cuda_dev = self.cuda_dev.0;
        let pools = self.pools(w, h, format)?;
        let (drm_fc, vk_fc, cuda_fc) = (pools.drm.0, pools.vulkan.0, pools.cuda.0);
        // SAFETY: the descriptor outlives the mapping (both end in this call); every frame we
        // allocate is freed on all paths.
        unsafe {
            let mut desc: AVDRMFrameDescriptor = std::mem::zeroed();
            desc.nb_objects = 1;
            desc.objects[0] = AVDRMObjectDescriptor { fd, size, format_modifier: modifier };
            desc.nb_layers = 1;
            desc.layers[0].format = fourcc;
            desc.layers[0].nb_planes = 1;
            desc.layers[0].planes[0] = AVDRMPlaneDescriptor { object_index: 0, offset: offset as isize, pitch: stride as isize };

            let mut src = av_frame_alloc();
            (*src).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            ((*src).width, (*src).height) = (w as i32, h as i32);
            (*src).data[0] = (&mut desc as *mut AVDRMFrameDescriptor).cast();
            (*src).buf[0] = av_buffer_create((*src).data[0], size_of::<AVDRMFrameDescriptor>(), Some(no_free), ptr::null_mut(), 0);
            (*src).hw_frames_ctx = av_buffer_ref(drm_fc);

            let mut mapped = av_frame_alloc();
            (*mapped).format = AVPixelFormat::AV_PIX_FMT_VULKAN as i32;
            (*mapped).hw_frames_ctx = av_buffer_ref(vk_fc);
            let e = av_hwframe_map(mapped, src, AV_HWFRAME_MAP_READ as i32);
            if e < 0 {
                av_frame_free(&mut mapped);
                av_frame_free(&mut src);
                return Err(err(e, "import DMA-BUF into Vulkan"));
            }
            // GPU copy into a pool frame, which (unlike the imported one) CUDA can read.
            let mut staged = av_frame_alloc();
            let e = av_hwframe_get_buffer(vk_fc, staged, 0);
            let copied = if e < 0 {
                Err(err(e, "Vulkan staging frame"))
            } else {
                self.copier.copy(self.vk_dev.0, vk_fc, mapped, vk_fc, staged)
            };
            // The DMA-BUF has been read; release it.
            av_frame_free(&mut mapped);
            av_frame_free(&mut src);
            if let Err(e) = copied {
                av_frame_free(&mut staged);
                return Err(e.context("copy DMA-BUF on the GPU"));
            }
            let mut out = av_frame_alloc();
            let e = av_hwframe_get_buffer(cuda_fc, out, 0);
            let e = if e < 0 { e } else { av_hwframe_transfer_data(out, staged, 0) };
            let synced = if e >= 0 { self.sync(cuda_dev) } else { Ok(()) };
            av_frame_free(&mut staged);
            if e < 0 {
                av_frame_free(&mut out);
                return Err(err(e, "copy frame Vulkan -> CUDA"));
            }
            if let Err(e) = synced {
                av_frame_free(&mut out);
                return Err(e);
            }
            Ok(GpuFrame(out))
        }
    }

    /// Wait for FFmpeg's CUDA stream, so the copy out of the DMA-BUF has finished.
    unsafe fn sync(&self, cuda_dev: *mut AVBufferRef) -> Result<()> {
        // SAFETY: the CUDA context and stream belong to our live CUDA device.
        unsafe {
            let dev = (*cuda_dev).data as *const AVHWDeviceContext;
            let cu = &*((*dev).hwctx as *const AVCUDADeviceContext);
            let r = (self.cuda.push)(cu.cuda_ctx.cast());
            if r != 0 {
                bail!("cuCtxPushCurrent failed: {r}");
            }
            let s = (self.cuda.sync)(cu.stream.cast());
            let mut old = ptr::null_mut();
            (self.cuda.pop)(&mut old);
            if s != 0 {
                bail!("cuStreamSynchronize failed: {s}");
            }
        }
        Ok(())
    }
}

unsafe extern "C" fn no_free(_opaque: *mut c_void, _data: *mut u8) {}
