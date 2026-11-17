//! Zero-copy conversion: the compositor's DMA-BUF is imported into Vulkan as an image, and one
//! compute pass writes 10-bit Y'CbCr straight into a buffer whose memory CUDA and NVENC share.
//! No copy on the way and nothing touches CPU memory.

use std::collections::HashMap;
use std::os::fd::{BorrowedFd, IntoRawFd};

use anyhow::{Context, Result, anyhow, bail};
use ash::vk;

/// Layout of the encoder input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chroma {
    /// P010: a Y plane, then interleaved CbCr at half size.
    C420,
    /// Three full-size planes.
    C444,
}

/// One encoder input buffer, written by Vulkan and read by NVENC.
pub struct Slot {
    buffer: vk::Buffer,
    #[allow(dead_code)]
    memory: vk::DeviceMemory,
    /// The memory's size, for importing it into CUDA.
    pub size: u64,
    /// The exported handle; CUDA takes it over. `-1` once taken.
    pub fd: i32,
}

struct Imported {
    image: vk::Image,
    view: vk::ImageView,
    memory: vk::DeviceMemory,
    /// False until its first use, when the layout is still undefined.
    used: bool,
}

pub struct Converter {
    _entry: ash::Entry,
    instance: ash::Instance,
    pdev: vk::PhysicalDevice,
    device: ash::Device,
    mem_fd: ash::khr::external_memory_fd::Device,
    queue: vk::Queue,
    family: u32,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    sampler: vk::Sampler,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    desc_pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
    pub slots: Vec<Slot>,
    images: HashMap<i32, Imported>,
    pub width: u32,
    pub height: u32,
    /// Bytes per row of every plane in a slot.
    pub pitch: u32,
    chroma: Chroma,
}

// SAFETY: used by one thread at a time (built on the main thread, then owned by the capture
// thread); Vulkan handles are plain values.
unsafe impl Send for Converter {}

const FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;

impl Converter {
    pub fn new(width: u32, height: u32, chroma: Chroma, slots: usize) -> Result<Self> {
        anyhow::ensure!(width % 2 == 0 && height % 2 == 0, "odd sizes aren't handled");
        // SAFETY: plain Vulkan setup; every handle created here is owned by the returned value.
        unsafe {
            let entry = ash::Entry::load().context("load Vulkan")?;
            let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_3);
            let instance = entry.create_instance(&vk::InstanceCreateInfo::default().application_info(&app), None)?;
            let pdev = instance
                .enumerate_physical_devices()?
                .into_iter()
                .find(|&p| instance.get_physical_device_properties(p).vendor_id == 0x10de)
                .context("no NVIDIA GPU in Vulkan")?;
            let family = instance
                .get_physical_device_queue_family_properties(pdev)
                .iter()
                .position(|q| q.queue_flags.contains(vk::QueueFlags::COMPUTE))
                .context("no compute queue")? as u32;
            let exts = [
                ash::khr::external_memory_fd::NAME.as_ptr(),
                ash::ext::external_memory_dma_buf::NAME.as_ptr(),
                ash::ext::image_drm_format_modifier::NAME.as_ptr(),
                ash::ext::queue_family_foreign::NAME.as_ptr(),
            ];
            let prio = [1.0f32];
            let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&prio)];
            let device = instance.create_device(
                pdev,
                &vk::DeviceCreateInfo::default().queue_create_infos(&queues).enabled_extension_names(&exts),
                None,
            )?;
            let mem_fd = ash::khr::external_memory_fd::Device::new(&instance, &device);
            let queue = device.get_device_queue(family, 0);
            let pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default().queue_family_index(family).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?;
            let cmd = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default().command_pool(pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1),
            )?[0];
            let fence = device.create_fence(&vk::FenceCreateInfo::default(), None)?;
            let sampler = device.create_sampler(&vk::SamplerCreateInfo::default(), None)?;

            let bindings = [
                vk::DescriptorSetLayoutBinding::default()
                    .binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
                vk::DescriptorSetLayoutBinding::default()
                    .binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
            ];
            let set_layout = device.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)?;
            let set_layouts = [set_layout];
            let ranges = [vk::PushConstantRange::default().stage_flags(vk::ShaderStageFlags::COMPUTE).size(16)];
            let layout = device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts).push_constant_ranges(&ranges),
                None,
            )?;
            let spirv = ash::util::read_spv(&mut std::io::Cursor::new(&include_bytes!(concat!(env!("OUT_DIR"), "/convert.spv"))[..]))?;
            let module = device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&spirv), None)?;
            let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE).module(module).name(c"main");
            let pipeline = device
                .create_compute_pipelines(vk::PipelineCache::null(), &[vk::ComputePipelineCreateInfo::default().stage(stage).layout(layout)], None)
                .map_err(|(_, e)| anyhow!("compute pipeline: {e}"))?[0];
            device.destroy_shader_module(module, None);
            let sizes = [
                vk::DescriptorPoolSize::default().ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).descriptor_count(1),
                vk::DescriptorPoolSize::default().ty(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(1),
            ];
            let desc_pool = device.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes), None)?;
            let set = device.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(desc_pool).set_layouts(&set_layouts))?[0];

            let pitch = width * 2;
            let bytes = pitch as u64 * height as u64 * if chroma == Chroma::C444 { 6 } else { 3 } / 2;
            let mut this = Self {
                _entry: entry,
                instance,
                pdev,
                device,
                mem_fd,
                queue,
                family,
                pool,
                cmd,
                fence,
                sampler,
                set_layout,
                layout,
                pipeline,
                desc_pool,
                set,
                slots: Vec::new(),
                images: HashMap::new(),
                width,
                height,
                pitch,
                chroma,
            };
            for _ in 0..slots {
                let slot = this.make_slot(bytes)?;
                this.slots.push(slot);
            }
            Ok(this)
        }
    }

    /// A storage buffer whose memory can be handed to CUDA.
    unsafe fn make_slot(&self, bytes: u64) -> Result<Slot> {
        unsafe {
            let handle = vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD;
            let mut ext = vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle);
            let buffer = self.device.create_buffer(
                &vk::BufferCreateInfo::default().size(bytes).usage(vk::BufferUsageFlags::STORAGE_BUFFER).push_next(&mut ext),
                None,
            )?;
            let req = self.device.get_buffer_memory_requirements(buffer);
            let props = self.instance.get_physical_device_memory_properties(self.pdev);
            let ty = (0..props.memory_type_count)
                .find(|&i| req.memory_type_bits & (1 << i) != 0 && props.memory_types[i as usize].property_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL))
                .context("no device memory type for the encoder input")?;
            let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(handle);
            let memory = self
                .device
                .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty).push_next(&mut export), None)?;
            self.device.bind_buffer_memory(buffer, memory, 0)?;
            let fd = self.mem_fd.get_memory_fd(&vk::MemoryGetFdInfoKHR::default().memory(memory).handle_type(handle))?;
            Ok(Slot { buffer, memory, size: req.size, fd })
        }
    }

    /// The DMA-BUF layouts (modifiers) this GPU can sample BGRx from, to offer the compositor.
    pub fn modifiers(&self) -> Result<Vec<u64>> {
        // SAFETY: two-call enumeration into a vector of the reported length.
        unsafe {
            let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
            let mut props = vk::FormatProperties2::default().push_next(&mut list);
            self.instance.get_physical_device_format_properties2(self.pdev, FORMAT, &mut props);
            let n = list.drm_format_modifier_count as usize;
            let mut items = vec![vk::DrmFormatModifierPropertiesEXT::default(); n];
            let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut items);
            let mut props = vk::FormatProperties2::default().push_next(&mut list);
            self.instance.get_physical_device_format_properties2(self.pdev, FORMAT, &mut props);
            Ok(items
                .iter()
                .filter(|m| m.drm_format_modifier_plane_count == 1 && m.drm_format_modifier_tiling_features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE))
                .map(|m| m.drm_format_modifier)
                .collect())
        }
    }

    /// Forget imported images (the stream renegotiated, so its buffers are new).
    pub fn reset_images(&mut self) {
        // SAFETY: nothing is in flight: every convert waits for its own work.
        unsafe {
            for (_, i) in self.images.drain() {
                self.device.destroy_image_view(i.view, None);
                self.device.destroy_image(i.image, None);
                self.device.free_memory(i.memory, None);
            }
        }
    }

    unsafe fn import(&self, fd: i32, modifier: u64, offset: u32, stride: u32) -> Result<Imported> {
        unsafe {
            let planes = [vk::SubresourceLayout { offset: offset as u64, size: 0, row_pitch: stride as u64, array_pitch: 0, depth_pitch: 0 }];
            let mut drm = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default().drm_format_modifier(modifier).plane_layouts(&planes);
            let handle = vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;
            let mut ext = vk::ExternalMemoryImageCreateInfo::default().handle_types(handle);
            let image = self.device.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(FORMAT)
                    .extent(vk::Extent3D { width: self.width, height: self.height, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                    .usage(vk::ImageUsageFlags::SAMPLED)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED)
                    .push_next(&mut ext)
                    .push_next(&mut drm),
                None,
            )?;
            let req = self.device.get_image_memory_requirements(image);
            let mut fd_props = vk::MemoryFdPropertiesKHR::default();
            self.mem_fd.get_memory_fd_properties(handle, fd, &mut fd_props)?;
            let bits = req.memory_type_bits & fd_props.memory_type_bits;
            anyhow::ensure!(bits != 0, "the DMA-BUF's memory can't back this image");
            // Vulkan takes over the descriptor it is given, so hand it a copy.
            let dup = BorrowedFd::borrow_raw(fd).try_clone_to_owned()?.into_raw_fd();
            let mut import = vk::ImportMemoryFdInfoKHR::default().handle_type(handle).fd(dup);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let memory = match self.device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(bits.trailing_zeros())
                    .push_next(&mut import)
                    .push_next(&mut dedicated),
                None,
            ) {
                Ok(m) => m,
                Err(e) => {
                    libc::close(dup);
                    self.device.destroy_image(image, None);
                    bail!("import DMA-BUF memory: {e}");
                }
            };
            self.device.bind_image_memory(image, memory, 0)?;
            let view = self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(FORMAT)
                    .subresource_range(vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 }),
                None,
            )?;
            Ok(Imported { image, view, memory, used: false })
        }
    }

    /// Convert the DMA-BUF `fd` into slot `slot`, returning once the GPU has finished.
    pub fn convert(&mut self, fd: i32, modifier: u64, offset: u32, stride: u32, slot: usize) -> Result<()> {
        // SAFETY: handles are ours and live; the fence wait at the end means nothing recorded
        // here outlives the call.
        unsafe {
            if !self.images.contains_key(&fd) {
                let img = self.import(fd, modifier, offset, stride)?;
                self.images.insert(fd, img);
            }
            let img = self.images.get_mut(&fd).unwrap();
            let (image, view, first) = (img.image, img.view, !img.used);
            img.used = true;
            let d = &self.device;
            let image_info = [vk::DescriptorImageInfo::default().sampler(self.sampler).image_view(view).image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let buffer_info = [vk::DescriptorBufferInfo::default().buffer(self.slots[slot].buffer).range(vk::WHOLE_SIZE)];
            d.update_descriptor_sets(
                &[
                    vk::WriteDescriptorSet::default().dst_set(self.set).dst_binding(0).descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).image_info(&image_info),
                    vk::WriteDescriptorSet::default().dst_set(self.set).dst_binding(1).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&buffer_info),
                ],
                &[],
            );
            d.begin_command_buffer(self.cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
            let range = vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 };
            // Take the image over from the compositor (the "foreign" queue family), keeping its
            // contents.
            let acquire = vk::ImageMemoryBarrier::default()
                .image(image)
                .subresource_range(range)
                .old_layout(if first { vk::ImageLayout::UNDEFINED } else { vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL })
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .dst_queue_family_index(self.family)
                .dst_access_mask(vk::AccessFlags::SHADER_READ);
            d.cmd_pipeline_barrier(self.cmd, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::COMPUTE_SHADER, vk::DependencyFlags::empty(), &[], &[], &[acquire]);
            d.cmd_bind_pipeline(self.cmd, vk::PipelineBindPoint::COMPUTE, self.pipeline);
            d.cmd_bind_descriptor_sets(self.cmd, vk::PipelineBindPoint::COMPUTE, self.layout, 0, &[self.set], &[]);
            let pc = [self.width, self.height, self.pitch / 4, if self.chroma == Chroma::C444 { 1 } else { 0 }];
            d.cmd_push_constants(self.cmd, self.layout, vk::ShaderStageFlags::COMPUTE, 0, std::slice::from_raw_parts(pc.as_ptr() as *const u8, 16));
            d.cmd_dispatch(self.cmd, (self.width / 2).div_ceil(8), (self.height / 2).div_ceil(8), 1);
            let release = vk::ImageMemoryBarrier::default()
                .image(image)
                .subresource_range(range)
                .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(self.family)
                .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .src_access_mask(vk::AccessFlags::SHADER_READ);
            d.cmd_pipeline_barrier(self.cmd, vk::PipelineStageFlags::COMPUTE_SHADER, vk::PipelineStageFlags::BOTTOM_OF_PIPE, vk::DependencyFlags::empty(), &[], &[], &[release]);
            d.end_command_buffer(self.cmd)?;
            let cmds = [self.cmd];
            d.queue_submit(self.queue, &[vk::SubmitInfo::default().command_buffers(&cmds)], self.fence)?;
            d.wait_for_fences(&[self.fence], true, 1_000_000_000).context("conversion didn't finish")?;
            d.reset_fences(&[self.fence])?;
            Ok(())
        }
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: our own objects, idle.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.reset_images();
            for s in &self.slots {
                self.device.destroy_buffer(s.buffer, None);
                self.device.free_memory(s.memory, None);
            }
            self.device.destroy_pipeline(self.pipeline, None);
            self.device.destroy_pipeline_layout(self.layout, None);
            self.device.destroy_descriptor_pool(self.desc_pool, None);
            self.device.destroy_descriptor_set_layout(self.set_layout, None);
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}
