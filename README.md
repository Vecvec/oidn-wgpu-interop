# OIDN wgpu interoperability


[![Crates.io](https://img.shields.io/crates/v/oidn-wgpu-interop.svg)](https://crates.io/crates/oidn-wgpu-interop)
[![CI](https://github.com/Vecvec/oidn-wgpu-interop/actions/workflows/ci.yml/badge.svg)](https://github.com/Vecvec/oidn-wgpu-interop/actions/workflows/ci.yml)
---

A helper library to create shared buffers between OIDN and
wgpu. 

## Getting started

### Creating the device

Simply replace the `adapter.request_device` call with
`oidn_wgpu_interop::Device::new` (using
`adapter.request_device` only if
`oidn_wgpu_interop::Device::new` fails). You are then able
to call `device.wgpu_device` to get the created wgpu device
and `device.oidn_device` to get the OIDN device.

### Creating shared buffers

To create a shared buffer call
`device.allocate_shared_buffers`. The shared buffer may be
used with usages
`BufferUsages::COPY_SRC | BufferUsages::COPY_DST`. To get
the wgpu buffer call `buffer.wgpu_buffer` and to get the
OIDN buffer call `buffer.oidn_buffer`. It is recommended to
minimise the number of shared buffers that exist at a given
time due to them each requiring a separate allocation.

## Synchronisation

There is no synchronisation between OIDN and wgpu currently
possible. This lack of synchronisation means that after
using any `SharedBuffer` in wgpu, but before using it with
OIDN, all wgpu command buffers that use the shared buffer
must finish. The same must happen in the opposite direction,
any OIDN functions that use this buffer must have finished.

## Platform Support

Currently the following platforms are supported (individual GPUs may or may not be supported):
- DirectX (tested personally)
- Vulkan on Windows (tested personally)
- Vulkan on Linux (best effort, will compile)

Platform support could also be expanded to Metal, but I don't want to do too much.