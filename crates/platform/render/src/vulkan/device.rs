//! Instance, physical-device and logical-device creation for the Vulkan backend.
//!
//! This module is private to [`crate::vulkan`]. It contains only immutable
//! mechanism: [`VulkanPipeline`](super::VulkanPipeline) keeps the created
//! instance and selected physical device; the logical device and its queues
//! are created per caller in
//! [`begin`](super::VulkanPipeline::begin_with_display), because present-queue
//! selection needs the window surface.
#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char};

use ash::vk;

use api::error::RenderError;

use crate::PipelineConfig;

/// Name of the Khronos validation layer, requested when
/// [`PipelineConfig::validation`] is set and the layer is installed.
const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// Instance-level resources selected in [`create_instance`].
pub(crate) struct InstanceBundle {
    /// Loaded Vulkan entry points.
    pub(crate) entry: ash::Entry,
    /// The Vulkan instance.
    pub(crate) instance: ash::Instance,
    /// Selected physical device.
    pub(crate) physical_device: vk::PhysicalDevice,
    /// `maxImageDimension2D` reported by the selected device.
    pub(crate) max_texture_dimension: u32,
    /// Cached memory-type properties of the selected device.
    pub(crate) memory_properties: vk::PhysicalDeviceMemoryProperties,
    /// Whether validation was requested and enabled.
    pub(crate) validation: bool,
}

/// Device-level resources created in [`create_device`].
pub(crate) struct DeviceBundle {
    /// The logical device.
    pub(crate) device: ash::Device,
    /// Queue used for graphics submission.
    pub(crate) graphics_queue: vk::Queue,
    /// Queue used for presentation (the same family as graphics here).
    pub(crate) present_queue: vk::Queue,
    /// Index of the combined graphics+present queue family.
    pub(crate) queue_family_index: u32,
}

/// Returns the name of an extension property as a borrowed `CStr`.
///
/// # Safety note
///
/// Vulkan guarantees `extension_name` is a NUL-terminated UTF-8-ish string.
fn extension_name(property: &vk::ExtensionProperties) -> &CStr {
    // SAFETY: `extension_name` is a fixed NUL-terminated array owned by the
    // property value, which outlives the borrow.
    unsafe { CStr::from_ptr(property.extension_name.as_ptr()) }
}

/// Returns the name of a layer property as a borrowed `CStr`.
fn layer_name(property: &vk::LayerProperties) -> &CStr {
    // SAFETY: as `extension_name`, the layer name is NUL-terminated.
    unsafe { CStr::from_ptr(property.layer_name.as_ptr()) }
}

/// Converts the configured application name into the NUL-terminated C string
/// `VkApplicationInfo::pApplicationName` requires.
///
/// A C string cannot carry an interior NUL, so a name that does is reported as
/// [`RenderError::InvalidArgument`] rather than silently renamed.
fn application_name_cstring(name: &str) -> Result<CString, RenderError> {
    CString::new(name).map_err(|error| RenderError::InvalidArgument {
        message: format!("the Vulkan application name is not a valid C string: {error}"),
    })
}

/// Loads the Vulkan loader, creates the instance and selects a physical device.
///
/// The instance enables `VK_KHR_surface` plus every platform surface extension
/// the loader advertises (xlib, xcb, wayland, win32, android, metal). The
/// platform is only known when a window is supplied to
/// [`begin`](super::VulkanPipeline::begin_with_display), so enabling all
/// available surface extensions avoids a second instance.
pub(crate) fn create_instance(config: &PipelineConfig) -> Result<InstanceBundle, RenderError> {
    // SAFETY: `Entry::load` dlopens the platform Vulkan loader. It is the
    // standard, documented way to initialise ash and is safe to call once per
    // process; the returned `Entry` owns the loader handle.
    let entry = unsafe { ash::Entry::load() }.map_err(|error| RenderError::DeviceUnavailable {
        message: format!("the Vulkan loader is unavailable: {error}"),
    })?;

    // SAFETY: `entry` owns the loaded Vulkan loader handle; the query is
    // read-only and returns an owned `Vec`.
    let extension_properties = unsafe { entry.enumerate_instance_extension_properties(None) }
        .map_err(|error| RenderError::DeviceUnavailable {
            message: format!(
                "failed to enumerate Vulkan instance extensions: {}",
                vk_result_message(error)
            ),
        })?;
    let available: Vec<&CStr> = extension_properties.iter().map(extension_name).collect();

    let has = |name: &CStr| available.contains(&name);

    if !has(ash::khr::surface::NAME) {
        return Err(RenderError::Unsupported {
            message: "the Vulkan loader does not expose VK_KHR_surface".to_owned(),
        });
    }

    // `VK_KHR_surface` plus every platform surface extension the loader has.
    // ash-window selects the right one from the display handle at surface
    // creation time.
    const PLATFORM_SURFACE_EXTENSIONS: &[&CStr] = &[
        ash::khr::xlib_surface::NAME,
        ash::khr::xcb_surface::NAME,
        ash::khr::wayland_surface::NAME,
        ash::khr::win32_surface::NAME,
        ash::khr::android_surface::NAME,
        ash::ext::metal_surface::NAME,
    ];

    let mut enabled_extensions: Vec<*const c_char> = vec![ash::khr::surface::NAME.as_ptr()];
    for extension in PLATFORM_SURFACE_EXTENSIONS {
        if has(extension) {
            enabled_extensions.push(extension.as_ptr());
        }
    }

    // Validation is opt-in and only enabled when the layer is installed. A
    // missing layer is a warning rather than an error so a release machine
    // without the SDK still runs.
    let mut validation = false;
    let mut enabled_layers: Vec<*const c_char> = Vec::new();
    if config.validation {
        // SAFETY: as above, `entry` is the live loader; the query is read-only.
        let layers = unsafe { entry.enumerate_instance_layer_properties() }.map_err(|error| {
            RenderError::DeviceUnavailable {
                message: format!(
                    "failed to enumerate Vulkan layers: {}",
                    vk_result_message(error)
                ),
            }
        })?;
        if layers
            .iter()
            .any(|layer| layer_name(layer) == VALIDATION_LAYER)
        {
            validation = true;
            enabled_layers.push(VALIDATION_LAYER.as_ptr());
        } else {
            log::warn!(
                "Vulkan validation was requested but VK_LAYER_KHRONOS_validation is not installed"
            );
        }
    }

    let application_name = application_name_cstring(config.application_name.as_str())?;
    let engine_name = c"krkr-rs";

    let application_info = vk::ApplicationInfo::default()
        .application_name(application_name.as_c_str())
        .application_version(vk::make_api_version(0, 1, 0, 0))
        .engine_name(engine_name)
        .engine_version(vk::make_api_version(0, 1, 0, 0))
        .api_version(vk::API_VERSION_1_0);

    let create_info = vk::InstanceCreateInfo::default()
        .application_info(&application_info)
        .enabled_extension_names(&enabled_extensions)
        .enabled_layer_names(&enabled_layers);

    // SAFETY: `create_info` borrows locals that live until the call returns;
    // the instance is destroyed by the owning `InstanceGuard`.
    let instance = unsafe { entry.create_instance(&create_info, None) }.map_err(|error| {
        RenderError::DeviceUnavailable {
            message: format!(
                "failed to create the Vulkan instance: {}",
                vk_result_message(error)
            ),
        }
    })?;

    let (physical_device, max_texture_dimension, memory_properties) =
        select_physical_device(&instance)?;

    Ok(InstanceBundle {
        entry,
        instance,
        physical_device,
        max_texture_dimension,
        memory_properties,
        validation,
    })
}

/// Selects the best physical device that can present through a swapchain.
///
/// Discrete GPUs are preferred over integrated ones; any device that lacks the
/// `VK_KHR_swapchain` device extension or a graphics queue is rejected.
fn select_physical_device(
    instance: &ash::Instance,
) -> Result<(vk::PhysicalDevice, u32, vk::PhysicalDeviceMemoryProperties), RenderError> {
    // SAFETY: `instance` is the live instance created above; the query is
    // read-only.
    let devices = unsafe { instance.enumerate_physical_devices() }.map_err(|error| {
        RenderError::DeviceUnavailable {
            message: format!(
                "no usable Vulkan physical device was handed back: enumeration failed with {}",
                vk_result_message(error)
            ),
        }
    })?;

    let mut best: Option<(
        u32,
        vk::PhysicalDevice,
        u32,
        vk::PhysicalDeviceMemoryProperties,
    )> = None;
    for physical_device in devices {
        // SAFETY: `physical_device` was enumerated from this `instance`; the
        // query is read-only.
        let properties = unsafe { instance.get_physical_device_properties(physical_device) };
        let type_rank = match properties.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => 3,
            vk::PhysicalDeviceType::INTEGRATED_GPU => 2,
            vk::PhysicalDeviceType::VIRTUAL_GPU => 1,
            _ => 0,
        };

        if !device_has_extension(instance, physical_device, ash::khr::swapchain::NAME) {
            continue;
        }
        let families =
            // SAFETY: `physical_device` belongs to this `instance`; read-only.
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let has_graphics = families
            .iter()
            .any(|family| family.queue_flags.contains(vk::QueueFlags::GRAPHICS));
        if !has_graphics {
            continue;
        }

        let max_dimension = properties.limits.max_image_dimension2_d.max(1);
        let memory_properties =
            // SAFETY: `physical_device` belongs to this `instance`; read-only.
            unsafe { instance.get_physical_device_memory_properties(physical_device) };
        let candidate = (type_rank, physical_device, max_dimension, memory_properties);
        if best.as_ref().is_none_or(|current| candidate.0 > current.0) {
            best = Some(candidate);
        }
    }

    best.map(|(_, device, max_dimension, memory_properties)| {
        (device, max_dimension, memory_properties)
    })
    .ok_or_else(|| RenderError::DeviceUnavailable {
        message: "no Vulkan physical device supports VK_KHR_swapchain and a graphics queue"
            .to_owned(),
    })
}

/// Returns `true` when `physical_device` exposes the named device extension.
fn device_has_extension(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    name: &CStr,
) -> bool {
    // SAFETY: `physical_device` belongs to this `instance`; the query is
    // read-only.
    match unsafe { instance.enumerate_device_extension_properties(physical_device) } {
        Ok(properties) => properties
            .iter()
            .any(|property| extension_name(property) == name),
        Err(_) => false,
    }
}

/// Creates a logical device with a combined graphics+present queue family.
///
/// Returns [`RenderError::DeviceUnavailable`] when no queue family supports
/// both graphics and presentation for `surface`.
pub(crate) fn create_device(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    surface_loader: &ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
) -> Result<DeviceBundle, RenderError> {
    // SAFETY: `physical_device` belongs to this `instance`; read-only.
    let families = unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    let mut chosen = None;
    for (index, family) in families.iter().enumerate() {
        let graphics = family.queue_flags.contains(vk::QueueFlags::GRAPHICS);
        if !graphics || family.queue_count == 0 {
            continue;
        }
        let index = index as u32;
        // SAFETY: `physical_device` and `surface` belong to `instance`; the
        // query is read-only.
        let present = unsafe {
            surface_loader.get_physical_device_surface_support(physical_device, index, surface)
        }
        .unwrap_or(false);
        if present {
            chosen = Some(index);
            break;
        }
    }

    let queue_family_index = chosen.ok_or_else(|| RenderError::DeviceUnavailable {
        message: "no Vulkan queue family supports both graphics and presentation".to_owned(),
    })?;

    let priorities = [1.0_f32];
    let queue_info = vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family_index)
        .queue_priorities(&priorities);
    let queue_infos = [queue_info];

    let enabled_extensions = [ash::khr::swapchain::NAME.as_ptr()];
    let device_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queue_infos)
        .enabled_extension_names(&enabled_extensions);

    // SAFETY: `device_info` borrows locals valid for the call; the device is
    // destroyed in `VulkanState::drop`.
    let device = unsafe { instance.create_device(physical_device, &device_info, None) }.map_err(
        |error| RenderError::DeviceUnavailable {
            message: format!(
                "failed to create the Vulkan logical device: {}",
                vk_result_message(error)
            ),
        },
    )?;

    // SAFETY: `queue_family_index` was validated above and `queue_index` 0 is
    // below the family's queue count.
    let graphics_queue = unsafe { device.get_device_queue(queue_family_index, 0) };
    let present_queue = graphics_queue;

    Ok(DeviceBundle {
        device,
        graphics_queue,
        present_queue,
        queue_family_index,
    })
}

/// Formats a Vulkan result as its canonical name and the loader's description.
///
/// The name (`VK_ERROR_INITIALIZATION_FAILED`) is what a user can search for;
/// the description is the text the loader supplies. An unrecognised result
/// falls back to its raw numeric value.
pub(crate) fn vk_result_message(result: vk::Result) -> String {
    format!("VK_{result:?} ({result})")
}

/// Maps a Vulkan result into an owned render error.
pub(crate) fn result_error(context: &str, result: vk::Result) -> RenderError {
    match result {
        vk::Result::ERROR_DEVICE_LOST => RenderError::DeviceLost {
            message: context.to_owned(),
        },
        vk::Result::ERROR_SURFACE_LOST_KHR => RenderError::SurfaceLost,
        vk::Result::ERROR_OUT_OF_DEVICE_MEMORY | vk::Result::ERROR_OUT_OF_HOST_MEMORY => {
            RenderError::OutOfMemory {
                message: context.to_owned(),
            }
        }
        other => RenderError::DeviceUnavailable {
            message: format!("{context}: {}", vk_result_message(other)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The reference raises a failed device creation distinctly from an
    // unsupported technique and names the underlying code before raising it
    // (reference/cpp/core/visual/impl/BasicDrawDevice.cpp:113-123,
    // reference/cpp/core/visual/impl/BasicDrawDevice.cpp:368-375,
    // reference/cpp/core/base/impl/MsgImpl.h:131-133,
    // reference/cpp/core/base/impl/MsgImpl.h:268-306). The reference is the
    // oracle for behaviour, not for error strings; this only checks that the
    // result name survives, as the reference keeps the named error.
    #[test]
    fn vk_result_message_names_the_result() {
        let formatted = vk_result_message(vk::Result::ERROR_INITIALIZATION_FAILED);
        assert!(formatted.contains("VK_ERROR_INITIALIZATION_FAILED"));
        assert!(formatted.contains("Initialization of an object has failed"));
    }

    // The reference engine has no Vulkan instance and therefore no application
    // name, so this is the render layer's own typed-error contract for the
    // backend argument rather than a reference behaviour.
    #[test]
    fn application_name_accepts_a_plain_name() {
        let name = application_name_cstring("krkr-rs").expect("plain name");
        assert_eq!(name.as_c_str(), c"krkr-rs");
    }

    #[test]
    fn application_name_rejects_an_interior_nul() {
        assert!(matches!(
            application_name_cstring("krkr\0rs"),
            Err(RenderError::InvalidArgument { .. })
        ));
    }
}
