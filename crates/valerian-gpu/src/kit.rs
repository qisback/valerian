// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Small wrappers over wgpu: kernels, buffers, dispatch and read-back.

use helia_gpu::wgpu::{
    self, BindGroup, BindGroupDescriptor, BindGroupEntry, Buffer, BufferUsages,
    ComputePassDescriptor, ComputePipeline, ComputePipelineDescriptor, Device, MapMode, PollType,
    Queue, ShaderModuleDescriptor, ShaderSource, util::BufferInitDescriptor, util::DeviceExt,
};

/// One compute entry point with an automatic bind group layout.
pub struct Kernel {
    pipeline: ComputePipeline,
}

impl Kernel {
    /// Compile `wgsl` and make a pipeline for its `main` entry point.
    #[must_use]
    pub fn new(device: &Device, label: &str, wgsl: &str) -> Self {
        let module = device.create_shader_module(ShaderModuleDescriptor {
            label: Some(label),
            source: ShaderSource::Wgsl(wgsl.into()),
        });
        let pipeline = device.create_compute_pipeline(&ComputePipelineDescriptor {
            label: Some(label),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            // Every kernel writes its workgroup memory before reading it.
            compilation_options: wgpu::PipelineCompilationOptions {
                zero_initialize_workgroup_memory: false,
                ..wgpu::PipelineCompilationOptions::default()
            },
            cache: None,
        });
        Self { pipeline }
    }

    /// A bind group of `buffers`, which are bindings 0, 1, 2 and so on.
    #[must_use]
    pub fn bind(&self, device: &Device, buffers: &[&Buffer]) -> BindGroup {
        let entries: Vec<BindGroupEntry<'_>> = buffers
            .iter()
            .enumerate()
            .map(|(i, b)| BindGroupEntry {
                binding: u32::try_from(i).unwrap_or(0),
                resource: b.as_entire_binding(),
            })
            .collect();
        device.create_bind_group(&BindGroupDescriptor {
            label: None,
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &entries,
        })
    }

    /// Run the kernel over `groups` workgroups, and wait for it.
    pub fn run(&self, device: &Device, queue: &Queue, bind: &BindGroup, groups: (u32, u32)) {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = enc.begin_compute_pass(&ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.dispatch_workgroups(groups.0, groups.1, 1);
        }
        queue.submit(Some(enc.finish()));
        // Wait for it, so the time spent can be told from the copies around it.
        let _ = device.poll(PollType::Wait {
            submission_index: None,
            timeout: None,
        });
    }
}

/// Workgroups for `count` items at `per_group` each, as an x and a y so no
/// dimension passes 65535. The kernels index with
/// `(workgroup_id.y * 65535 + workgroup_id.x) * per_group + local_index`.
#[must_use]
pub fn groups_1d(count: usize, per_group: usize) -> (u32, u32) {
    let g = count.div_ceil(per_group).max(1);
    let x = g.min(65535);
    (
        u32::try_from(x).unwrap_or(65535),
        u32::try_from(g.div_ceil(65535)).unwrap_or(1),
    )
}

/// A storage buffer holding `data`.
#[must_use]
pub fn storage_from(device: &Device, label: &str, data: &[u8]) -> Buffer {
    let padded: Vec<u8>;
    let bytes = if data.is_empty() {
        padded = vec![0u8; 4];
        &padded[..]
    } else {
        data
    };
    device.create_buffer_init(&BufferInitDescriptor {
        label: Some(label),
        contents: bytes,
        usage: BufferUsages::STORAGE | BufferUsages::COPY_SRC,
    })
}

/// A uniform buffer holding `data`.
#[must_use]
pub fn uniform_from(device: &Device, data: &[u8]) -> Buffer {
    device.create_buffer_init(&BufferInitDescriptor {
        label: Some("params"),
        contents: data,
        usage: BufferUsages::UNIFORM,
    })
}

/// `count` values of a plain-data type read back from `src` through `staging`,
/// a `MAP_READ | COPY_DST` buffer of at least that many bytes.
///
/// # Errors
/// The reason, when the copy or the mapping fails.
pub fn read_pod<T: bytemuck::Pod>(
    device: &Device,
    queue: &Queue,
    src: &Buffer,
    staging: &Buffer,
    count: usize,
) -> Result<Vec<T>, String> {
    let bytes = (count * std::mem::size_of::<T>()) as u64;
    let padded = bytes.next_multiple_of(4).max(4);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    enc.copy_buffer_to_buffer(src, 0, staging, 0, padded);
    queue.submit(Some(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..padded).map_async(MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .map_err(|e| e.to_string())?;
    rx.recv()
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let out = {
        let view = staging
            .slice(..padded)
            .get_mapped_range()
            .map_err(|e| e.to_string())?;
        bytemuck::pod_collect_to_vec(&view[..usize::try_from(bytes).unwrap_or(0)])
    };
    staging.unmap();
    Ok(out)
}
