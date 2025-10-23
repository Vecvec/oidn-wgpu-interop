use std::fmt::Debug;

#[cfg(dx12)]
mod dx12;
#[cfg(vulkan)]
mod vulkan;

pub enum DeviceCreateError {
    RequestDeviceError(wgpu::RequestDeviceError),
    OidnUnsupported,
    OidnImportUnsupported,
    MissingFeature,
    UnsupportedBackend(wgpu::Backend),
}

impl Debug for DeviceCreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            DeviceCreateError::RequestDeviceError(err) => err.fmt(f),
            DeviceCreateError::OidnUnsupported => f.write_str(
                "OIDN could not create a device for this Adapter (does this adapter support OIDN?)",
            ),
            DeviceCreateError::OidnImportUnsupported => {
                f.write_str("OIDN does not support the required import method")
            }
            DeviceCreateError::MissingFeature => f.write_str("A required feature is missing"),
            DeviceCreateError::UnsupportedBackend(backend) => {
                f.write_str("The backend ")?;
                backend.fmt(f)?;
                f.write_str(" is not supported.")
            }
        }
    }
}

pub enum SharedBufferCreateError {
    InvalidSize(wgpu::BufferAddress),
    Oidn((oidn::Error, String)),
    OutOfMemory,
}

impl Debug for SharedBufferCreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            SharedBufferCreateError::InvalidSize(size) => {
                f.write_str("Size ")?;
                size.fmt(f)?;
                f.write_str(" is not allowed")
            }
            SharedBufferCreateError::Oidn((error, desc)) => {
                f.write_str("OIDN shared buffer creation failed with error ")?;
                error.fmt(f)?;
                f.write_str(": ")?;
                desc.fmt(f)
            }
            SharedBufferCreateError::OutOfMemory => f.write_str("Out of memory"),
        }
    }
}

#[derive(Eq, PartialEq, Clone, Copy, Debug)]
enum Backend {
    #[cfg(dx12)]
    Dx12,
    #[cfg(vulkan)]
    Vulkan,
}

enum BackendData {
    #[cfg(dx12)]
    Dx12,
    #[cfg(vulkan)]
    Vulkan(vulkan::VulkanSharingMode),
}

impl BackendData {
    fn as_backend(&self) -> Backend {
        match self {
            #[cfg(dx12)]
            BackendData::Dx12 => Backend::Dx12,
            #[cfg(vulkan)]
            BackendData::Vulkan(_) => Backend::Vulkan,
        }
    }
}

pub struct Device {
    wgpu_device: wgpu::Device,
    oidn_device: oidn::Device,
    queue: wgpu::Queue,
    backend_data: BackendData,
}

impl Device {
    pub async fn new(
        adapter: &wgpu::Adapter,
        desc: &wgpu::DeviceDescriptor<'_>,
    ) -> Result<(Self, wgpu::Queue), DeviceCreateError> {
        match adapter.get_info().backend {
            #[cfg(vulkan)]
            wgpu::Backend::Vulkan => Self::new_vulkan(adapter, desc).await,
            #[cfg(dx12)]
            wgpu::Backend::Dx12 => Self::new_dx12(adapter, desc).await,
            _ => Err(DeviceCreateError::UnsupportedBackend(
                adapter.get_info().backend,
            )),
        }
    }
    pub fn allocate_shared_buffers(
        &self,
        size: wgpu::BufferAddress,
    ) -> Result<SharedBuffer, SharedBufferCreateError> {
        if size == 0 {
            return Err(SharedBufferCreateError::InvalidSize(size));
        }
        match self.backend_data.as_backend() {
            #[cfg(dx12)]
            Backend::Dx12 => self.allocate_shared_buffers_dx12(size),
            #[cfg(vulkan)]
            Backend::Vulkan => self.allocate_shared_buffers_vulkan(size),
        }
    }
    pub fn oidn_device(&self) -> &oidn::Device {
        &self.oidn_device
    }

    pub fn wgpu_device(&self) -> &wgpu::Device {
        &self.wgpu_device
    }

    async fn new_from_raw_oidn_adapter<
        F: FnOnce(oidn::sys::OIDNExternalMemoryTypeFlag) -> Option<BackendData>,
    >(
        device: oidn::sys::OIDNDevice,
        adapter: &wgpu::Adapter,
        desc: &wgpu::DeviceDescriptor<'_>,
        backend_data_callback: F,
    ) -> Result<(Self, wgpu::Queue), DeviceCreateError> {
        if device.is_null() {
            return Err(crate::DeviceCreateError::OidnUnsupported);
        }

        let supported_memory_types = unsafe {
            oidn::sys::oidnCommitDevice(device);
            oidn::sys::oidnGetDeviceInt(device, b"externalMemoryTypes\0" as *const _ as _)
        } as oidn::sys::OIDNExternalMemoryTypeFlag;
        let Some(backend_data) = backend_data_callback(supported_memory_types) else {
            unsafe {
                oidn::sys::oidnReleaseDevice(device);
            }
            return Err(DeviceCreateError::OidnImportUnsupported);
        };
        let oidn_device = unsafe { oidn::Device::from_raw(device) };
        let (wgpu_device, queue) = adapter
            .request_device(desc)
            .await
            .map_err(crate::DeviceCreateError::RequestDeviceError)?;
        Ok((
            Self {
                wgpu_device,
                oidn_device,
                queue: queue.clone(),
                backend_data,
            },
            queue,
        ))
    }
}

enum Allocation {
    // we keep these around to keep the allocations alive
    #[cfg(dx12)]
    Dx12 { _dx12: dx12::Dx12Allocation },
    #[cfg(vulkan)]
    Vulkan,
}

pub struct SharedBuffer {
    _allocation: Allocation,
    oidn_buffer: oidn::Buffer,
    wgpu_buffer: wgpu::Buffer,
}

impl SharedBuffer {
    pub fn oidn_buffer(&self) -> &oidn::Buffer {
        &self.oidn_buffer
    }
    pub fn oidn_buffer_mut(&mut self) -> &mut oidn::Buffer {
        &mut self.oidn_buffer
    }
    pub fn wgpu_buffer(&self) -> &wgpu::Buffer {
        &self.wgpu_buffer
    }
}

#[cfg(test)]
#[async_std::test]
async fn test() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12 | wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());
    for adapter in adapters {
        match adapter.get_info().backend {
            wgpu::Backend::Vulkan => {
                eprintln!("Testing vulkan device {}", adapter.get_info().name);
            }
            wgpu::Backend::Dx12 => {
                eprintln!("Testing dx12 device {}", adapter.get_info().name);
            }
            _ => continue,
        }
        let (device, queue) = match Device::new(&adapter, &wgpu::DeviceDescriptor::default()).await
        {
            Ok((device, queue)) => (device, queue),
            Err(err) => {
                eprintln!("Device creation failed");
                eprintln!("    {err:?}");
                continue;
            }
        };
        let mut bufs = device
            .allocate_shared_buffers(size_of::<[f32; 3]>() as wgpu::BufferAddress)
            .unwrap();
        queue.write_buffer(bufs.wgpu_buffer(), 0, &1.0_f32.to_ne_bytes());
        queue.submit([]);
        device
            .wgpu_device()
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        assert_eq!(bufs.oidn_buffer_mut().read()[0], 1.0);
        let mut filter = oidn::RayTracing::new(device.oidn_device());
        filter.image_dimensions(1, 1);
        filter
            .filter_in_place_buffer(&mut bufs.oidn_buffer_mut())
            .unwrap();
        match device.oidn_device().get_error() {
            Ok(_) | Err((oidn::Error::OutOfMemory, _)) => {}
            Err(err) => panic!("{err:?}"),
        }
    }
}
