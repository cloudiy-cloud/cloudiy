//! WGSL compute runtime — the *safest* runtime class: consumers pick a fixed
//! kernel shipped with the node and never send executable code, so it is
//! sandboxed by construction and available on any GPU (Metal / Vulkan /
//! DX12) via wgpu. This is the runtime every personal provider can enable;
//! OCI/Docker stays opt-in for hosts with proper isolation.
//!
//! Workload convention: `spec.template` names the kernel (`vector_add`,
//! `matrix_mul`) and `spec.command[0]` carries the input string.

use crate::{ExecutionHandle, MetricsSnapshot, Outcome, Runtime};
use anyhow::{anyhow, Context, Result};
use cloudiy_protocol::WorkloadSpec;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

/// Kernels this runtime ships. Advertised as `kernel:<name>` capabilities.
pub const KERNELS: &[&str] = &["vector_add", "matrix_mul"];

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

fn parse_floats(s: &str) -> Result<Vec<f32>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.parse::<f32>()
                .map_err(|_| format!("invalid number '{t}'"))
        })
        .collect()
}

fn format_floats(v: &[f32]) -> String {
    v.iter().map(f32::to_string).collect::<Vec<_>>().join(",")
}

/// Dispatch a named kernel on the GPU. Input formats:
/// - `vector_add`: `"a1,a2,...;b1,b2,..."` (two equal-length vectors)
/// - `matrix_mul`: `"m,k,n;a1,...(m*k);b1,...(k*n)"` (row-major)
pub fn execute_kernel(gpu: &GpuExecutor, kernel: &str, input: &str) -> Result<String, String> {
    match kernel {
        "vector_add" => {
            let parts: Vec<&str> = input.split(';').collect();
            if parts.len() != 2 {
                return Err("vector_add expects input 'a1,a2,...;b1,b2,...'".to_string());
            }
            let a = parse_floats(parts[0])?;
            let b = parse_floats(parts[1])?;
            let out = gpu.vector_add(&a, &b).map_err(|e| e.to_string())?;
            Ok(format_floats(&out))
        }
        "matrix_mul" => {
            let parts: Vec<&str> = input.split(';').collect();
            if parts.len() != 3 {
                return Err("matrix_mul expects input 'm,k,n;a1,...(m*k);b1,...(k*n)'".to_string());
            }
            let dims: Vec<u32> = parts[0]
                .split(',')
                .map(|t| {
                    t.trim()
                        .parse::<u32>()
                        .map_err(|_| format!("invalid dim '{t}'"))
                })
                .collect::<Result<_, _>>()?;
            if dims.len() != 3 {
                return Err("matrix_mul dims must be 'm,k,n'".to_string());
            }
            let a = parse_floats(parts[1])?;
            let b = parse_floats(parts[2])?;
            let out = gpu
                .matmul(&a, &b, dims[0], dims[1], dims[2])
                .map_err(|e| e.to_string())?;
            Ok(format_floats(&out))
        }
        other => Err(format!(
            "unknown kernel '{other}' — available: {}",
            KERNELS.join(", ")
        )),
    }
}

/// WGSL as a [`Runtime`]: `spec.template` = kernel name, `spec.command[0]` =
/// input string, logs = the numeric output. Environments here are pure GPU
/// dispatches — nothing to pull and nothing survives `destroy`.
pub struct WgslRuntime {
    gpu: Arc<GpuExecutor>,
    outputs: Mutex<HashMap<String, String>>,
}

impl WgslRuntime {
    pub fn new(gpu: Arc<GpuExecutor>) -> Self {
        WgslRuntime {
            gpu,
            outputs: Mutex::new(HashMap::new()),
        }
    }

    fn kernel_of(spec: &WorkloadSpec) -> Result<String> {
        let kernel = spec
            .template
            .as_deref()
            .ok_or_else(|| anyhow!("wgsl workload needs `template` = kernel name"))?;
        anyhow::ensure!(
            KERNELS.contains(&kernel),
            "unknown kernel '{kernel}' — available: {}",
            KERNELS.join(", ")
        );
        Ok(kernel.to_string())
    }
}

#[async_trait::async_trait]
impl Runtime for WgslRuntime {
    fn name(&self) -> &'static str {
        "wgsl"
    }

    async fn supports(&self) -> bool {
        // Only constructed after successful GPU detection.
        true
    }

    async fn prepare(&self, spec: &WorkloadSpec) -> anyhow::Result<()> {
        Self::kernel_of(spec).map(|_| ())
    }

    async fn run(&self, workload_id: &str, spec: &WorkloadSpec) -> anyhow::Result<ExecutionHandle> {
        let kernel = Self::kernel_of(spec)?;
        let input = spec.command.first().cloned().unwrap_or_default();
        let gpu = self.gpu.clone();

        // GPU dispatch blocks on readback — keep it off the async runtime.
        let output = tokio::task::spawn_blocking(move || execute_kernel(&gpu, &kernel, &input))
            .await?
            .map_err(|e| anyhow!(e))?;

        self.outputs
            .lock()
            .unwrap()
            .insert(workload_id.to_string(), output);
        Ok(ExecutionHandle(workload_id.to_string()))
    }

    async fn wait(&self, _handle: &ExecutionHandle) -> anyhow::Result<Outcome> {
        // `run` is synchronous with the dispatch: reaching here means success.
        Ok(Outcome::Succeeded)
    }

    async fn logs(&self, handle: &ExecutionHandle) -> anyhow::Result<String> {
        Ok(self
            .outputs
            .lock()
            .unwrap()
            .get(&handle.0)
            .cloned()
            .unwrap_or_default())
    }

    async fn metrics(&self, _handle: &ExecutionHandle) -> anyhow::Result<MetricsSnapshot> {
        Ok(MetricsSnapshot::default())
    }

    async fn destroy(&self, handle: ExecutionHandle) -> anyhow::Result<()> {
        self.outputs.lock().unwrap().remove(&handle.0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_floats_accepts_csv() {
        assert_eq!(parse_floats("1, 2,3.5").unwrap(), vec![1.0, 2.0, 3.5]);
        assert!(parse_floats("1,abc").is_err());
    }

    /// Full Runtime-trait lifecycle on the real GPU — self-skips without one.
    #[tokio::test]
    async fn wgsl_runtime_lifecycle() {
        let Ok(gpu) = GpuExecutor::new().await else {
            eprintln!("no GPU adapter available — skipping");
            return;
        };
        let rt = WgslRuntime::new(Arc::new(gpu));
        let spec = WorkloadSpec {
            template: Some("vector_add".into()),
            command: vec!["1,2,3;10,20,30".into()],
            ..Default::default()
        };
        let (outcome, logs) = crate::execute(&rt, "wl-test", &spec).await.unwrap();
        assert_eq!(outcome, Outcome::Succeeded);
        assert_eq!(logs, "11,22,33");

        // Unknown kernels are rejected in prepare.
        let bad = WorkloadSpec {
            template: Some("mystery".into()),
            ..Default::default()
        };
        assert!(rt.prepare(&bad).await.is_err());
    }
}
