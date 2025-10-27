use std::{fs::File, sync::mpsc};

use futures::executor::block_on;
use image::{ImageBuffer, Rgb, buffer::ConvertBuffer};
use wgpu::{
    Backends, BufferAddress, BufferUsages, Instance, InstanceDescriptor,
    wgt::{BufferDescriptor, DeviceDescriptor, PollType},
};

fn main() {
    let image = image::load_from_memory(include_bytes!("../box.png"))
        .unwrap()
        .to_rgb32f();

    // Set up the oidn shared device and wgpu queue
    let instance = Instance::new(&InstanceDescriptor::default());

    let mut device_queue = None;

    for adapter in instance.enumerate_adapters(Backends::all()) {
        if let Ok(dev) = block_on(oidn_wgpu_interop::Device::new(
            &adapter,
            &DeviceDescriptor::default(),
        )) {
            device_queue = Some(dev);
        }
    }

    // A real implementation should probably fall back to cpu copying.
    let (device, queue) = device_queue.expect("Failed to find an interoperability capable device");

    let image_byte_size = size_of_val::<[f32]>(image.as_raw());

    // Fake a real workload, just write image to buffer.
    // This buffer would get the output of the ray tracer
    let buffer = device.wgpu_device().create_buffer(&BufferDescriptor {
        label: Some("renderer output buffer"),
        size: image_byte_size as BufferAddress,
        usage: BufferUsages::COPY_DST | BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    let shared_buffer = device
        .allocate_shared_buffers(image_byte_size as BufferAddress)
        .unwrap();

    queue.write_buffer(&buffer, 0, bytemuck::cast_slice(image.as_raw()));

    let mut encoder = device
        .wgpu_device()
        .create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(
        &buffer,
        0,
        shared_buffer.wgpu_buffer(),
        0,
        image_byte_size as u64,
    );
    queue.submit([encoder.finish()]);

    // setup filter
    let mut filter = oidn::filter::RayTracing::new(device.oidn_device());
    filter.image_dimensions(image.width() as usize, image.height() as usize);

    // Must wait for wgpu to finish before we can start oidn workload.
    device
        .wgpu_device()
        .poll(PollType::wait_indefinitely())
        .unwrap();

    // filter
    filter
        .filter_in_place_buffer(&shared_buffer.oidn_buffer())
        .unwrap();

    // Output to a wgpu buffer (in this case to be saved to disk). No sync needed here because oidn blocks the CPU until the workload is finished.
    let out_buffer = device.wgpu_device().create_buffer(&BufferDescriptor {
        label: Some("save buffer"),
        size: image_byte_size as BufferAddress,
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device
        .wgpu_device()
        .create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(
        shared_buffer.wgpu_buffer(),
        0,
        &out_buffer,
        0,
        image_byte_size as u64,
    );
    let (send, recv) = mpsc::channel();
    encoder.map_buffer_on_submit(&out_buffer, wgpu::MapMode::Read, .., move |res| {
        res.unwrap();
        send.send(()).unwrap()
    });
    queue.submit([encoder.finish()]);

    // Wait for copy to finish, so we can save to disk.
    device
        .wgpu_device()
        .poll(PollType::wait_indefinitely())
        .unwrap();
    recv.recv().unwrap();

    let mapped_range = out_buffer.get_mapped_range(..);
    let image: ImageBuffer<Rgb<f32>, _> = ImageBuffer::from_raw(
        image.width(),
        image.height(),
        bytemuck::cast_slice(&*mapped_range),
    )
    .unwrap();

    let mut file = File::create("./examples/denoise/box_denoised.png").unwrap();
    ConvertBuffer::<ImageBuffer<Rgb<u8>, _>>::convert(&image)
        .write_to(&mut file, image::ImageFormat::Png)
        .unwrap();

    drop(mapped_range);
    out_buffer.unmap();
}
