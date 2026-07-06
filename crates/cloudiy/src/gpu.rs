//! Real GPU execution via wgpu (Metal on macOS, Vulkan/DX12 elsewhere).
//!
//! Kernels are fixed WGSL shaders compiled at startup — consumers pick one
//! by name and never send executable code, so the provider machine only
//! ever runs shaders it shipped with (sandboxed by construction).

use anyhow::{anyhow, Context, Result};
use wgpu::util::DeviceExt;

/// Hard cap on elements per input vector/matrix (64M floats = 256 MiB).
const MAX_ELEMENTS: usize = 64 * 1024 * 1024;

const VECTOR_ADD_WGSL: &str = r#"
@group(0) @binding(0) var<storage, read> a: array<f32>;
@group(0) @binding(1) var<storage, read> b: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < arrayLength(&out)) {
        out[i] = a[i] + b[i];
    }
}
"#;

const MATMUL_WGSL: &str = r#"
struct Dims { m: u32, k: u32, n: u32, _pad: u32 };

@group(0) @binding(0) var<storage, read> a: array<f32>;
@group(0) @binding(1) var<storage, read> b: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
@group(0) @binding(3) var<uniform> dims: Dims;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let col = gid.x;
    let row = gid.y;
    if (row >= dims.m || col >= dims.n) {
        return;
    }
    var sum = 0.0;
    for (var i = 0u; i < dims.k; i = i + 1u) {
        sum = sum + a[row * dims.k + i] * b[i * dims.n + col];
    }
    out[row * dims.n + col] = sum;
}
"#;

pub struct GpuExecutor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    vector_add: wgpu::ComputePipeline,
    matmul: wgpu::ComputePipeline,
    pub info: wgpu::AdapterInfo,
}

impl GpuExecutor {
    pub async fn new() -> Result<Self> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            })
            .await
            .context("no compatible GPU adapter found")?;
        let info = adapter.get_info();

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .context("failed to acquire GPU device")?;

        let make_pipeline = |label: &str, src: &str| -> wgpu::ComputePipeline {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            })
        };

        let vector_add = make_pipeline("vector_add", VECTOR_ADD_WGSL);
        let matmul = make_pipeline("matmul", MATMUL_WGSL);

        Ok(Self {
            device,
            queue,
            vector_add,
            matmul,
            info,
        })
    }

    /// Element-wise sum of two equal-length f32 vectors.
    pub fn vector_add(&self, a: &[f32], b: &[f32]) -> Result<Vec<f32>> {
        anyhow::ensure!(a.len() == b.len(), "vectors must have the same length");
        anyhow::ensure!(!a.is_empty(), "vectors must not be empty");
        anyhow::ensure!(a.len() <= MAX_ELEMENTS, "vector too large");

        let n = a.len();
        let workgroups = (n as u32).div_ceil(256);
        self.run(
            &self.vector_add,
            &[
                InputBuffer::Storage(bytemuck::cast_slice(a)),
                InputBuffer::Storage(bytemuck::cast_slice(b)),
            ],
            (n * 4) as u64,
            (workgroups, 1, 1),
        )
    }

    /// Row-major matrix product: A (m×k) · B (k×n) → out (m×n).
    pub fn matmul(&self, a: &[f32], b: &[f32], m: u32, k: u32, n: u32) -> Result<Vec<f32>> {
        anyhow::ensure!(m > 0 && k > 0 && n > 0, "dimensions must be positive");
        let (m_, k_, n_) = (m as usize, k as usize, n as usize);
        anyhow::ensure!(a.len() == m_ * k_, "A must have m*k elements");
        anyhow::ensure!(b.len() == k_ * n_, "B must have k*n elements");
        anyhow::ensure!(
            m_ * k_ <= MAX_ELEMENTS && k_ * n_ <= MAX_ELEMENTS && m_ * n_ <= MAX_ELEMENTS,
            "matrix too large"
        );

        let dims: [u32; 4] = [m, k, n, 0];
        self.run(
            &self.matmul,
            &[
                InputBuffer::Storage(bytemuck::cast_slice(a)),
                InputBuffer::Storage(bytemuck::cast_slice(b)),
                InputBuffer::Uniform(bytemuck::cast_slice(&dims)),
            ],
            (m_ * n_ * 4) as u64,
            (n.div_ceil(16), m.div_ceil(16), 1),
        )
    }

    /// Shared dispatch: upload inputs, bind an output storage buffer of
    /// `output_bytes`, dispatch, and read the result back as f32s.
    fn run(
        &self,
        pipeline: &wgpu::ComputePipeline,
        inputs: &[InputBuffer<'_>],
        output_bytes: u64,
        workgroups: (u32, u32, u32),
    ) -> Result<Vec<f32>> {
        let output = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("output"),
            size: output_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging"),
            size: output_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let input_buffers: Vec<wgpu::Buffer> = inputs
            .iter()
            .map(|input| {
                let (contents, usage) = match input {
                    InputBuffer::Storage(bytes) => (*bytes, wgpu::BufferUsages::STORAGE),
                    InputBuffer::Uniform(bytes) => (*bytes, wgpu::BufferUsages::UNIFORM),
                };
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("input"),
                        contents,
                        usage,
                    })
            })
            .collect();

        let mut entries: Vec<wgpu::BindGroupEntry> = input_buffers
            .iter()
            .enumerate()
            .map(|(i, buf)| {
                // The output buffer occupies binding 2 in both kernels, so
                // the uniform (dims) that follows it binds at index 3.
                let binding = if i < 2 { i as u32 } else { i as u32 + 1 };
                wgpu::BindGroupEntry {
                    binding,
                    resource: buf.as_entire_binding(),
                }
            })
            .collect();
        entries.push(wgpu::BindGroupEntry {
            binding: 2,
            resource: output.as_entire_binding(),
        });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bind"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &entries,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups.0, workgroups.1, workgroups.2);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &staging, 0, output_bytes);
        self.queue.submit(Some(encoder.finish()));

        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| anyhow!("GPU poll failed: {e:?}"))?;
        rx.recv()
            .context("GPU readback channel closed")?
            .map_err(|e| anyhow!("GPU buffer map failed: {e:?}"))?;

        let data = slice
            .get_mapped_range()
            .map_err(|e| anyhow!("GPU mapped range failed: {e:?}"))?;
        let result: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
        drop(data);
        staging.unmap();
        Ok(result)
    }
}

enum InputBuffer<'a> {
    Storage(&'a [u8]),
    Uniform(&'a [u8]),
}
