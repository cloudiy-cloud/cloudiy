//! # Cloudiy execution runtimes
//!
//! Docker is the first runtime. Docker is **not** part of the protocol —
//! it is merely today's implementation. Everything behind the [`Runtime`]
//! trait is interchangeable: Firecracker, Kata Containers, Podman, WASM and
//! microVMs must slot in without touching schedulers, APIs or SDKs.
//!
//! Lifecycle (mandatory, no shortcuts):
//! pull → create isolated environment → allocate → run → collect logs &
//! metrics → destroy → release. Workloads never execute directly on the host.

mod docker;
mod mock;
mod wgsl;

pub use docker::DockerRuntime;
pub use mock::MockRuntime;
pub use wgsl::{execute_kernel, GpuExecutor, WgslRuntime, KERNELS};

use cloudiy_protocol::WorkloadSpec;
use serde::{Deserialize, Serialize};

/// Opaque handle to a running (or finished) execution environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionHandle(pub String);

/// Terminal outcome of a workload execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed { exit_code: Option<i32> },
}

/// Point-in-time usage snapshot reported while a workload runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub cpu_percent: Option<f64>,
    pub memory_mib: Option<u64>,
    /// Raw runtime-specific payload for fields not yet modeled.
    pub raw: serde_json::Value,
}

/// The runtime seam. One implementation per execution technology.
#[async_trait::async_trait]
pub trait Runtime: Send + Sync {
    /// Human-readable driver name (`docker`, `firecracker`, `wasm`…).
    fn name(&self) -> &'static str;

    /// Is this runtime usable on this node right now? (binary installed,
    /// daemon reachable…). Discovery uses this to advertise capabilities.
    async fn supports(&self) -> bool;

    /// Fetch/prepare the environment (e.g. pull the OCI image). Separated
    /// from `run` so schedulers can account pull time distinctly.
    async fn prepare(&self, spec: &WorkloadSpec) -> anyhow::Result<()>;

    /// Create the isolated environment and start the workload.
    async fn run(&self, workload_id: &str, spec: &WorkloadSpec) -> anyhow::Result<ExecutionHandle>;

    /// Block until the workload reaches a terminal state.
    async fn wait(&self, handle: &ExecutionHandle) -> anyhow::Result<Outcome>;

    async fn logs(&self, handle: &ExecutionHandle) -> anyhow::Result<String>;

    async fn metrics(&self, handle: &ExecutionHandle) -> anyhow::Result<MetricsSnapshot>;

    /// Destroy the environment. MUST be idempotent and MUST always be called
    /// — terminal states release resources, no exceptions.
    async fn destroy(&self, handle: ExecutionHandle) -> anyhow::Result<()>;
}

/// Convenience: full lifecycle with guaranteed destruction.
pub async fn execute(
    runtime: &dyn Runtime,
    workload_id: &str,
    spec: &WorkloadSpec,
) -> anyhow::Result<(Outcome, String)> {
    runtime.prepare(spec).await?;
    let handle = runtime.run(workload_id, spec).await?;
    let outcome = runtime.wait(&handle).await;
    let logs = runtime.logs(&handle).await.unwrap_or_default();
    // Destroy no matter what happened above.
    runtime.destroy(handle).await.ok();
    Ok((outcome?, logs))
}
