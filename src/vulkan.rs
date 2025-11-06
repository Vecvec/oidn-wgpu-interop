use ash::{ext, khr, vk};
use oidn::sys::{
    OIDNExternalMemoryTypeFlag_OIDN_EXTERNAL_MEMORY_TYPE_FLAG_DMA_BUF,
    OIDNExternalMemoryTypeFlag_OIDN_EXTERNAL_MEMORY_TYPE_FLAG_OPAQUE_FD,
    OIDNExternalMemoryTypeFlag_OIDN_EXTERNAL_MEMORY_TYPE_FLAG_OPAQUE_WIN32,
};

use std::ptr;
use wgpu::hal::api::Vulkan;
use wgpu::hal::{CommandEncoder, vulkan};
use wgpu::util::align_to;
use wgpu::{BufferDescriptor, BufferUsages, DeviceDescriptor};

// We can't rely on the windows crate existing here and this may also be either a u32 or u64.
const ACCESS_GENERIC_ALL: vk::DWORD = 268435456;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum VulkanSharingMode {
    Win32,
    Fd,
    Dma,
}

impl crate::Device {
    pub(crate) async fn new_vulkan(
        adapter: &wgpu::Adapter,
        desc: &DeviceDescriptor<'_>,
    ) -> Result<(Self, wgpu::Queue), crate::DeviceCreateError> {
        let mut win_32_handle_supported = false;
        let mut fd_supported = false;
        let mut dma_buf_supported = false;

        let adapter_vulkan_desc = {
            // # SAFETY: the raw handle is not manually destroyed.
            let adapter = unsafe { adapter.as_hal::<Vulkan>() };
            adapter
                .and_then(|adapter| {
                    win_32_handle_supported = adapter
                        .physical_device_capabilities()
                        .supports_extension(khr::external_memory_win32::NAME);
                    fd_supported = adapter
                        .physical_device_capabilities()
                        .supports_extension(khr::external_memory_fd::NAME);
                    dma_buf_supported = adapter
                        .physical_device_capabilities()
                        .supports_extension(ext::external_memory_dma_buf::NAME)
                        && adapter
                            .physical_device_capabilities()
                            .supports_extension(khr::external_memory_fd::NAME);

                    let any_supported =
                        win_32_handle_supported || dma_buf_supported || fd_supported;

                    // `get_physical_device_properties2` requires version >= 1.1
                    (any_supported
                        && unsafe {
                            adapter
                                .shared_instance()
                                .raw_instance()
                                .get_physical_device_properties(adapter.raw_physical_device())
                        }
                        .api_version
                            >= vk::API_VERSION_1_1)
                        .then_some(adapter)
                })
                .map(|adapter| {
                    let mut id_properties = vk::PhysicalDeviceIDProperties::default();
                    unsafe {
                        adapter
                            .shared_instance()
                            .raw_instance()
                            .get_physical_device_properties2(
                                adapter.raw_physical_device(),
                                &mut vk::PhysicalDeviceProperties2::default()
                                    .push_next(&mut id_properties),
                            )
                    };
                    id_properties
                })
        };
        let Some(vk_desc) = adapter_vulkan_desc else {
            return Err(crate::DeviceCreateError::MissingFeature);
        };
        let device = unsafe {
            let mut dev_raw: oidn::sys::OIDNDevice = std::ptr::null_mut();
            if dev_raw.is_null() && vk_desc.device_luid_valid == vk::TRUE {
                dev_raw =
                    oidn::sys::oidnNewDeviceByLUID((&vk_desc.device_luid) as *const _ as *const _)
            }
            if dev_raw.is_null() {
                dev_raw =
                    oidn::sys::oidnNewDeviceByUUID((&vk_desc.device_uuid) as *const _ as *const _)
            }
            dev_raw
        };
        Self::new_from_raw_oidn_adapter(device, adapter, desc, |flag| {
            let oidn_supports_win32 =
                flag & OIDNExternalMemoryTypeFlag_OIDN_EXTERNAL_MEMORY_TYPE_FLAG_OPAQUE_WIN32 != 0;
            let oidn_supports_fd =
                flag & OIDNExternalMemoryTypeFlag_OIDN_EXTERNAL_MEMORY_TYPE_FLAG_OPAQUE_FD != 0;
            let oidn_supports_dma =
                flag & OIDNExternalMemoryTypeFlag_OIDN_EXTERNAL_MEMORY_TYPE_FLAG_DMA_BUF != 0;
            if oidn_supports_win32 && win_32_handle_supported {
                return Some(crate::BackendData::Vulkan(VulkanSharingMode::Win32));
            }
            if oidn_supports_fd && fd_supported {
                return Some(crate::BackendData::Vulkan(VulkanSharingMode::Fd));
            }
            if oidn_supports_dma && dma_buf_supported {
                return Some(crate::BackendData::Vulkan(VulkanSharingMode::Dma));
            }
            None
        })
        .await
    }
    pub(crate) fn allocate_shared_buffers_vulkan(
        &self,
        size: wgpu::BufferAddress,
    ) -> Result<crate::SharedBuffer, crate::SharedBufferCreateError> {
        // can happen if all other backends are switched off
        #[allow(unreachable_patterns)]
        let data = match self.backend_data {
            crate::BackendData::Vulkan(data) => data,
            _ => unreachable!(),
        };

        // # SAFETY: the raw handle is not manually destroyed.
        let device = unsafe { self.wgpu_device.as_hal::<Vulkan>() }.unwrap();
        let mut win_32_funcs = None;

        let mut fd_funcs = None;

        let handle_ty = match data {
            VulkanSharingMode::Win32 => {
                win_32_funcs = Some(khr::external_memory_win32::Device::new(
                    device.shared_instance().raw_instance(),
                    device.raw_device(),
                ));
                vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32_KHR
            }
            VulkanSharingMode::Fd => {
                fd_funcs = Some(khr::external_memory_fd::Device::new(
                    device.shared_instance().raw_instance(),
                    device.raw_device(),
                ));
                vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD_KHR
            }
            VulkanSharingMode::Dma => {
                fd_funcs = Some(khr::external_memory_fd::Device::new(
                    device.shared_instance().raw_instance(),
                    device.raw_device(),
                ));
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT
            }
        };

        let mut vk_external_memory_info =
            vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle_ty);

        let vk_info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            // technically exclusive because cross adapter doesn't matter here
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut vk_external_memory_info);

        let raw_buffer = unsafe { device.raw_device().create_buffer(&vk_info, None) }
            .map_err(|_| crate::SharedBufferCreateError::OutOfMemory)?;

        let req = unsafe {
            device
                .raw_device()
                .get_buffer_memory_requirements(raw_buffer)
        };

        let aligned_size = align_to(size, req.alignment);

        let mem_properties = unsafe {
            device
                .shared_instance()
                .raw_instance()
                .get_physical_device_memory_properties(device.raw_physical_device())
        };

        let mut idx = None;

        let flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;

        for (i, mem_ty) in mem_properties.memory_types_as_slice().iter().enumerate() {
            let types_bits = 1 << i;
            let is_required_memory_type = req.memory_type_bits & types_bits != 0;
            let has_required_properties = mem_ty.property_flags & flags == flags;
            if is_required_memory_type && has_required_properties {
                idx = Some(i);
                break;
            }
        }

        let Some(idx) = idx else {
            return Err(crate::SharedBufferCreateError::OutOfMemory);
        };

        let mut info = vk::MemoryAllocateInfo::default()
            .allocation_size(aligned_size)
            .memory_type_index(idx as u32);

        let mut export_alloc_info = vk::ExportMemoryAllocateInfo::default().handle_types(handle_ty);

        let mut win32_info;

        match data {
            VulkanSharingMode::Win32 => {
                win32_info =
                    vk::ExportMemoryWin32HandleInfoKHR::default().dw_access(ACCESS_GENERIC_ALL);
                info = info.push_next(&mut win32_info);
            }
            VulkanSharingMode::Dma | VulkanSharingMode::Fd => {}
        }

        info = info.push_next(&mut export_alloc_info);

        let memory = match unsafe { device.raw_device().allocate_memory(&info, None) } {
            Ok(memory) => memory,
            Err(_) => return Err(crate::SharedBufferCreateError::OutOfMemory),
        };

        unsafe {
            device
                .raw_device()
                .bind_buffer_memory(raw_buffer, memory, 0)
        }
        .map_err(|_| crate::SharedBufferCreateError::OutOfMemory)?;

        let oidn_buffer = match data {
            VulkanSharingMode::Win32 => unsafe {
                let handle = win_32_funcs
                    .as_ref()
                    .unwrap()
                    .get_memory_win32_handle(
                        &vk::MemoryGetWin32HandleInfoKHR::default()
                            .memory(memory)
                            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32_KHR),
                    )
                    .map_err(|_| crate::SharedBufferCreateError::OutOfMemory)?;
                oidn::sys::oidnNewSharedBufferFromWin32Handle(
                    self.oidn_device.raw(),
                    OIDNExternalMemoryTypeFlag_OIDN_EXTERNAL_MEMORY_TYPE_FLAG_OPAQUE_WIN32,
                    handle as *mut _,
                    ptr::null(),
                    size as usize,
                )
            },
            VulkanSharingMode::Fd => unsafe {
                let bit = fd_funcs
                    .as_ref()
                    .unwrap()
                    .get_memory_fd(
                        &vk::MemoryGetFdInfoKHR::default()
                            .memory(memory)
                            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD_KHR),
                    )
                    .map_err(|_| crate::SharedBufferCreateError::OutOfMemory)?;
                oidn::sys::oidnNewSharedBufferFromFD(
                    self.oidn_device.raw(),
                    OIDNExternalMemoryTypeFlag_OIDN_EXTERNAL_MEMORY_TYPE_FLAG_OPAQUE_FD,
                    bit as _,
                    size as usize,
                )
            },
            VulkanSharingMode::Dma => {
                let bit = unsafe {
                    fd_funcs.as_ref().unwrap().get_memory_fd(
                        &vk::MemoryGetFdInfoKHR::default()
                            .memory(memory)
                            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT),
                    )
                }
                .map_err(|_| crate::SharedBufferCreateError::OutOfMemory)?;
                unsafe {
                    oidn::sys::oidnNewSharedBufferFromFD(
                        self.oidn_device.raw(),
                        OIDNExternalMemoryTypeFlag_OIDN_EXTERNAL_MEMORY_TYPE_FLAG_DMA_BUF,
                        bit as _,
                        size as usize,
                    )
                }
            }
        };
        if oidn_buffer.is_null() {
            return Err(crate::SharedBufferCreateError::Oidn(
                self.oidn_device.get_error().unwrap_err(),
            ));
        }
        let buf = unsafe { vulkan::Buffer::from_raw_managed(raw_buffer, memory, 0, size) };
        let mut encoder = self.wgpu_device.create_command_encoder(&Default::default());
        // # SAFETY: the raw handle is not manually destroyed.
        unsafe {
            encoder.as_hal_mut::<Vulkan, _, _>(|encoder| {
                encoder.unwrap().clear_buffer(&buf, 0..size);
            })
        };
        self.queue.submit([encoder.finish()]);
        // # SAFETY: Just initialized buffer, created it from the same device and made with
        // the manually mapped usages.
        let wgpu_buffer = unsafe {
            self.wgpu_device.create_buffer_from_hal::<Vulkan>(
                buf,
                &BufferDescriptor {
                    label: None,
                    size,
                    usage: BufferUsages::COPY_SRC | BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            )
        };
        Ok(crate::SharedBuffer {
            _allocation: crate::Allocation::Vulkan,
            wgpu_buffer,
            oidn_buffer: unsafe { self.oidn_device.create_buffer_from_raw(oidn_buffer) },
        })
    }
}
