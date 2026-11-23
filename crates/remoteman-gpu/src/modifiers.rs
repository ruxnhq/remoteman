//! The DMA-BUF layouts any GPU (found by its DRM render node) can read BGRx from, to offer the
//! compositor when something other than the Vulkan converter (VA-API) imports the buffers.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

use anyhow::{Context, Result};
use ash::vk;

/// The single-plane layouts (modifiers) the GPU behind `node` samples BGRx from, as Vulkan
/// reports them for that device.
pub fn modifiers_on(node: &Path) -> Result<Vec<u64>> {
    let rdev = std::fs::metadata(node).with_context(|| format!("{}", node.display()))?.rdev();
    let (major, minor) = (libc::major(rdev) as i64, libc::minor(rdev) as i64);
    // SAFETY: plain Vulkan queries on the program's instance.
    unsafe {
        let (_, instance) = crate::vulkan()?;
        let found = (|| {
            for p in instance.enumerate_physical_devices()? {
                let exts = instance.enumerate_device_extension_properties(p)?;
                let has = |name: &std::ffi::CStr| exts.iter().any(|e| e.extension_name_as_c_str() == Ok(name));
                if !has(ash::ext::physical_device_drm::NAME) || !has(ash::ext::image_drm_format_modifier::NAME) {
                    continue;
                }
                let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
                let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
                instance.get_physical_device_properties2(p, &mut props);
                if drm.has_render == vk::FALSE || (drm.render_major, drm.render_minor) != (major, minor) {
                    continue;
                }
                let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
                let mut fp = vk::FormatProperties2::default().push_next(&mut list);
                instance.get_physical_device_format_properties2(p, vk::Format::B8G8R8A8_UNORM, &mut fp);
                let mut items = vec![vk::DrmFormatModifierPropertiesEXT::default(); list.drm_format_modifier_count as usize];
                let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut items);
                let mut fp = vk::FormatProperties2::default().push_next(&mut list);
                instance.get_physical_device_format_properties2(p, vk::Format::B8G8R8A8_UNORM, &mut fp);
                return Ok(Some(
                    items
                        .iter()
                        .filter(|m| m.drm_format_modifier_plane_count == 1 && m.drm_format_modifier_tiling_features.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE))
                        .map(|m| m.drm_format_modifier)
                        .collect(),
                ));
            }
            anyhow::Ok(None)
        })();
        found?.with_context(|| format!("no Vulkan device for {}", node.display()))
    }
}
