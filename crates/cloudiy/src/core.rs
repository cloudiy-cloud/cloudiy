//! Transport-agnostic node logic: state, payment gating (x402), job
//! execution and status. Both the P2P (iroh) and HTTP (axum) fronts call
//! into this module so behavior stays identical across transports.

use base64::Engine;
use cloudiy_common::{JobRequest, JobResponse, NodeInfo, StatusResponse};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use subtle::ConstantTimeEq;
use tracing::{info, warn};

use cloudiy_runtime::{GpuExecutor, WgslRuntime};

/// Completed jobs kept in memory for `status` queries. Oldest entries are
/// evicted beyond this to bound memory regardless of how long the node runs.
pub const MAX_STORED_JOBS: usize = 1024;

/// Wall-clock budget per job; consumers get an error instead of hanging.
pub const JOB_TIMEOUT_SECS: u64 = 60;

/// Jobs admitted concurrently (GPU work is serialized by the driver queue —
/// this bounds queuing memory, and excess submits are refused as "busy").
pub const MAX_CONCURRENT_JOBS: usize = 4;

/// Maximum accepted request body (16 MiB). Protects the node from
/// unbounded `input_data` payloads (memory-exhaustion / DoS).
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Cloudiy USDC escrow program on Solana devnet.
pub const ESCROW_PROGRAM: &str = "9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN";
/// Protocol fee charged by the escrow on release (basis points).
pub const PROTOCOL_FEE_BPS: u16 = 400;

/// Compares two auth tokens in constant time to avoid leaking the secret
/// through timing side-channels.
pub fn tokens_match(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// FIFO-bounded store of completed jobs.
#[derive(Default)]
pub struct JobStore {
    map: HashMap<String, JobResponse>,
    order: VecDeque<String>,
}

impl JobStore {
    pub fn insert(&mut self, job: JobResponse) {
        if !self.map.contains_key(&job.job_id) {
            self.order.push_back(job.job_id.clone());
        }
        self.map.insert(job.job_id.clone(), job);
        while self.map.len() > MAX_STORED_JOBS {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            } else {
                break;
            }
        }
    }

    pub fn get(&self, job_id: &str) -> Option<&JobResponse> {
        self.map.get(job_id)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

pub struct AppState {
    /// `None` on GPU-less machines — CPU/RAM providers are first-class:
    /// they serve container workloads and simply don't announce `wgsl`.
    pub gpu: Option<Arc<GpuExecutor>>,
    /// Built-in WGSL runtime — present exactly when `gpu` is.
    pub wgsl: Option<WgslRuntime>,
    pub jobs: Mutex<JobStore>,
    /// Node key — signs job results; its public half is the EndpointId.
    pub secret: iroh::SecretKey,
    /// Bounds concurrent job admissions (see [`MAX_CONCURRENT_JOBS`]).
    pub busy: Arc<tokio::sync::Semaphore>,
    pub token: String,
    /// iroh EndpointId — the node's P2P identity/address.
    pub endpoint_id: String,
    /// Solana wallet pubkey for USDC payouts.
    pub pubkey: String,
    pub gpu_model: String,
    pub vram_mb: u64,
    pub price_micro_usdc: u64,
    pub usdc_mint: String,
    pub network: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Open Compute Protocol accounting: what this node announced and what
    /// workloads currently hold. Allocation round-trips automatically.
    pub resources: Mutex<cloudiy_protocol::Resources>,
    /// Functionality this node offers (`docker`, `wgsl`, `linux`, `arm64`…).
    pub capabilities: Vec<cloudiy_protocol::Capability>,
    /// CloudiyOS: persistent, identity-bound VMs hosted on this node.
    pub vm: crate::vm::VmManager,
}

pub type SharedState = Arc<AppState>;

/// x402 "Payment Required" body (spec v1) quoting this node's price in USDC.
pub fn payment_requirements(state: &AppState) -> serde_json::Value {
    json!({
        "x402Version": 1,
        "error": "X-PAYMENT header is required",
        "accepts": [{
            "scheme": "exact",
            "network": state.network,
            "maxAmountRequired": state.price_micro_usdc.to_string(),
            "resource": "/submit",
            "description": format!("GPU job execution on {}", state.gpu_model),
            "mimeType": "application/json",
            "payTo": state.pubkey,
            "maxTimeoutSeconds": 300,
            "asset": state.usdc_mint,
            "extra": {
                "escrowProgram": ESCROW_PROGRAM,
                "feeBps": PROTOCOL_FEE_BPS,
            }
        }]
    })
}

/// Decodes an x402 payment payload (base64-encoded JSON, per spec).
/// The payload is untrusted input: it is parsed defensively and only the
/// scheme/network fields are inspected — never executed or interpolated.
pub fn decode_payment(raw: &str) -> Option<serde_json::Value> {
    if raw.len() > 8 * 1024 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(raw).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn node_info(state: &AppState) -> NodeInfo {
    let jobs_completed = state.jobs.lock().unwrap().len();
    NodeInfo {
        protocol: "cloudiy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        endpoint_id: state.endpoint_id.clone(),
        solana_pubkey: (state.pubkey != "<no-wallet-configured>").then(|| state.pubkey.clone()),
        gpu_model: state.gpu_model.clone(),
        vram_mb: state.vram_mb,
        jobs_completed,
        price_usdc: state.price_micro_usdc as f64 / 1_000_000.0,
        usdc_mint: state.usdc_mint.clone(),
        network: state.network.clone(),
        payment: "x402".to_string(),
        escrow_program: ESCROW_PROGRAM.to_string(),
        fee_bps: PROTOCOL_FEE_BPS,
        resources: Some(state.resources.lock().unwrap().clone()),
        capabilities: state.capabilities.clone(),
    }
}

pub fn job_status(state: &AppState, job_id: String) -> StatusResponse {
    let jobs = state.jobs.lock().unwrap();
    if let Some(job) = jobs.get(&job_id) {
        StatusResponse {
            job_id,
            status: job.status.clone(),
            progress: if job.status == "completed" { 100.0 } else { 50.0 },
            provider_pubkey: job.provider_pubkey.clone(),
        }
    } else {
        StatusResponse {
            job_id,
            status: "not_found".to_string(),
            progress: 0.0,
            provider_pubkey: None,
        }
    }
}

pub enum SubmitOutcome {
    /// Job executed; receipt is also embedded in `JobResponse.payment_receipt`.
    Completed(JobResponse),
    /// Payment missing/invalid — caller gets the x402 requirements to satisfy.
    PaymentRequired(serde_json::Value),
}

/// Payment gate + execution. `payment_override` lets the HTTP front pass the
/// `X-PAYMENT` header; P2P callers carry the payload in `JobRequest.payment`.
pub fn submit(
    state: &AppState,
    req: JobRequest,
    payment_override: Option<String>,
) -> SubmitOutcome {
    let payment = payment_override
        .as_deref()
        .or(req.payment.as_deref())
        .and_then(decode_payment);
    let dev_token_ok = tokens_match(&req.auth_token, &state.token);

    let settled_via = match (&payment, dev_token_ok) {
        (Some(p), _) => {
            let scheme = p.get("scheme").and_then(|v| v.as_str()).unwrap_or("?");
            let network = p.get("network").and_then(|v| v.as_str()).unwrap_or("?");
            info!(
                "Job {}: payment received (scheme={}, network={}) — settling via escrow {}",
                req.job_id, scheme, network, ESCROW_PROGRAM
            );
            "x402"
        }
        (None, true) => {
            info!("Job {}: accepted via dev token (no payment)", req.job_id);
            "dev-token"
        }
        (None, false) => {
            warn!("Job {}: no payment and invalid token — payment required", req.job_id);
            return SubmitOutcome::PaymentRequired(payment_requirements(state));
        }
    };

    info!("Received job {} — kernel: {}", req.job_id, req.kernel);

    let started = std::time::Instant::now();
    let result = match &state.gpu {
        Some(gpu) => execute_on_gpu(gpu, &req.kernel, &req.input_data, &req.params),
        None => Err(
            "this node shares CPU/RAM only (no GPU) — submit a container workload instead"
                .to_string(),
        ),
    };
    info!(
        "Job {} finished in {:?} on {}",
        req.job_id,
        started.elapsed(),
        state.gpu_model
    );

    let response = match result {
        Ok(output) => {
            // x402 settlement receipt (base64 JSON, per spec) — only issued
            // for successfully executed jobs.
            let receipt = base64::engine::general_purpose::STANDARD.encode(
                json!({
                    "success": true,
                    "network": state.network,
                    "settledVia": settled_via,
                    "payee": state.pubkey,
                })
                .to_string(),
            );
            let output_data = output.into_bytes();
            // Offline-verifiable proof that THIS node produced THIS output —
            // the artifact the escrow needs to release payment.
            let signature =
                cloudiy_common::sign_result(&state.secret, &req.job_id, &output_data);
            JobResponse {
                job_id: req.job_id.clone(),
                output_data,
                status: "completed".to_string(),
                error_message: None,
                provider_pubkey: Some(state.pubkey.clone()),
                payment_receipt: Some(receipt),
                signature: Some(signature),
                signed_by: Some(state.endpoint_id.clone()),
            }
        }
        Err(message) => JobResponse {
            job_id: req.job_id.clone(),
            output_data: vec![],
            status: "error".to_string(),
            error_message: Some(message),
            provider_pubkey: Some(state.pubkey.clone()),
            payment_receipt: None,
            signature: None,
            signed_by: None,
        },
    };

    state.jobs.lock().unwrap().insert(response.clone());

    SubmitOutcome::Completed(response)
}

/// Admission-controlled, time-bounded submit: refuses when the node is at
/// capacity and cuts the consumer loose after [`JOB_TIMEOUT_SECS`]. The
/// concurrency permit lives inside the blocking task so it is only released
/// when the GPU work actually finishes, even if the caller timed out.
pub async fn submit_guarded(
    state: SharedState,
    req: JobRequest,
    payment_override: Option<String>,
) -> Result<SubmitOutcome, String> {
    let Ok(permit) = state.busy.clone().try_acquire_owned() else {
        warn!("Job {}: refused — node at capacity", req.job_id);
        return Err("node busy — try again shortly".to_string());
    };

    let state2 = state.clone();
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        submit(&state2, req, payment_override)
    });

    match tokio::time::timeout(std::time::Duration::from_secs(JOB_TIMEOUT_SECS), job).await {
        Err(_) => Err(format!("job timed out after {JOB_TIMEOUT_SECS}s")),
        Ok(Err(e)) => Err(format!("job execution failed: {e}")),
        Ok(Ok(outcome)) => Ok(outcome),
    }
}

/// Default wall-clock budget for container workloads (image pull included).
pub const WORKLOAD_TIMEOUT_SECS: u64 = 300;

/// Payment gate shared by kernels and workloads. Returns how the request was
/// settled, or the x402 requirements the caller must satisfy.
fn payment_gate(
    state: &AppState,
    req: &JobRequest,
    payment_override: Option<&str>,
) -> Result<&'static str, serde_json::Value> {
    let payment = payment_override
        .or(req.payment.as_deref())
        .and_then(decode_payment);
    let dev_token_ok = tokens_match(&req.auth_token, &state.token);
    match (payment, dev_token_ok) {
        (Some(p), _) => {
            info!(
                "Job {}: payment received (scheme={}, network={}) — settling via escrow {}",
                req.job_id,
                p.get("scheme").and_then(|v| v.as_str()).unwrap_or("?"),
                p.get("network").and_then(|v| v.as_str()).unwrap_or("?"),
                ESCROW_PROGRAM
            );
            Ok("x402")
        }
        (None, true) => {
            info!("Job {}: accepted via dev token (no payment)", req.job_id);
            Ok("dev-token")
        }
        (None, false) => {
            warn!("Job {}: no payment and invalid token — payment required", req.job_id);
            Err(payment_requirements(state))
        }
    }
}

fn signed_response(state: &AppState, job_id: &str, output: Vec<u8>, settled_via: &str) -> JobResponse {
    let receipt = base64::engine::general_purpose::STANDARD.encode(
        json!({
            "success": true,
            "network": state.network,
            "settledVia": settled_via,
            "payee": state.pubkey,
        })
        .to_string(),
    );
    let signature = cloudiy_common::sign_result(&state.secret, job_id, &output);
    JobResponse {
        job_id: job_id.to_string(),
        output_data: output,
        status: "completed".to_string(),
        error_message: None,
        provider_pubkey: Some(state.pubkey.clone()),
        payment_receipt: Some(receipt),
        signature: Some(signature),
        signed_by: Some(state.endpoint_id.clone()),
    }
}

fn error_response(state: &AppState, job_id: &str, message: String) -> JobResponse {
    JobResponse {
        job_id: job_id.to_string(),
        output_data: vec![],
        status: "error".to_string(),
        error_message: Some(message),
        provider_pubkey: Some(state.pubkey.clone()),
        payment_receipt: None,
        signature: None,
        signed_by: None,
    }
}

/// Open Compute Protocol path: run a declared workload in an isolated
/// runtime. Full lifecycle with automatic resource accounting:
/// gate payment → check capabilities → allocate → prepare/run/wait →
/// collect logs → destroy → release. Resources ALWAYS return.
pub async fn run_workload(
    state: SharedState,
    req: JobRequest,
    spec: cloudiy_protocol::WorkloadSpec,
    payment_override: Option<String>,
) -> Result<SubmitOutcome, String> {
    use cloudiy_runtime::{DockerRuntime, Outcome, Runtime};

    let settled_via = match payment_gate(&state, &req, payment_override.as_deref()) {
        Ok(via) => via,
        Err(requirements) => return Ok(SubmitOutcome::PaymentRequired(requirements)),
    };

    // Consumers request functionality, never machines.
    if !cloudiy_protocol::capability::satisfies_all(&state.capabilities, &spec.capabilities) {
        return Err(format!(
            "capabilities not satisfied — node offers: {}",
            state
                .capabilities
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Runtime selection by workload shape: an OCI image needs a container
    // runtime; a kernel template runs on the built-in WGSL runtime — the
    // sandboxed-by-construction class every GPU provider can serve.
    let docker;
    let runtime: &dyn Runtime = if spec.image.is_some() {
        docker = DockerRuntime::default();
        if !docker.supports().await {
            return Err(
                "no container runtime on this node — declare a `template` kernel instead"
                    .to_string(),
            );
        }
        &docker
    } else {
        match &state.wgsl {
            Some(wgsl) => wgsl,
            None => {
                return Err(
                    "this node has no GPU — submit a container workload (`image`) instead"
                        .to_string(),
                )
            }
        }
    };

    let Ok(_permit) = state.busy.clone().try_acquire_owned() else {
        return Err("node busy — try again shortly".to_string());
    };

    // Allocate the requested resources; whatever happens next, release them.
    state
        .resources
        .lock()
        .unwrap()
        .allocate(&spec.resources)
        .map_err(|e| e.to_string())?;

    let timeout_secs = if spec.max_duration_secs > 0 {
        spec.max_duration_secs.min(3_600)
    } else {
        WORKLOAD_TIMEOUT_SECS
    };

    info!(
        "Workload {}: image={:?} cmd={:?} (runtime={}, timeout={}s)",
        req.job_id,
        spec.image,
        spec.command,
        runtime.name(),
        timeout_secs
    );

    let execution = async {
        runtime.prepare(&spec).await.map_err(|e| e.to_string())?;
        let handle = runtime
            .run(&req.job_id, &spec)
            .await
            .map_err(|e| e.to_string())?;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            runtime.wait(&handle),
        )
        .await;
        let logs = runtime.logs(&handle).await.unwrap_or_default();
        // Destroy no matter what: isolation environments never outlive jobs.
        runtime.destroy(handle).await.ok();
        match outcome {
            Err(_) => Err(format!("workload timed out after {timeout_secs}s")),
            Ok(Err(e)) => Err(format!("runtime error: {e}")),
            Ok(Ok(Outcome::Succeeded)) => Ok(logs),
            Ok(Ok(Outcome::Failed { exit_code })) => Err(format!(
                "workload exited with code {:?}; logs:\n{}",
                exit_code, logs
            )),
        }
    };
    let result = execution.await;

    // Automatic return — the accounting invariant of the protocol.
    state.resources.lock().unwrap().release(&spec.resources);

    let response = match result {
        Ok(logs) => signed_response(&state, &req.job_id, logs.into_bytes(), settled_via),
        Err(message) => error_response(&state, &req.job_id, message),
    };
    state.jobs.lock().unwrap().insert(response.clone());
    Ok(SubmitOutcome::Completed(response))
}

// --------------------------------------------------- CloudiyOS VM lifecycle

pub enum VmOutcome {
    Ready(cloudiy_common::VmInfo),
    PaymentRequired(serde_json::Value),
}

fn node_has_docker(state: &AppState) -> bool {
    state
        .capabilities
        .iter()
        .any(|c| c.name() == "docker")
}

/// Provision (or return) the caller's persistent VM. `owner` is the
/// authenticated QUIC peer identity — VM ownership is bound to the transport
/// identity, never to a self-declared field. Payment gates provisioning; an
/// already-running VM is returned without re-charging or re-allocating.
pub async fn start_vm(
    state: SharedState,
    owner: &str,
    req: JobRequest,
    spec: cloudiy_protocol::WorkloadSpec,
    payment_override: Option<String>,
) -> Result<VmOutcome, String> {
    if let Some(info) = state.vm.status(owner) {
        return Ok(VmOutcome::Ready(info));
    }
    if let Err(requirements) = payment_gate(&state, &req, payment_override.as_deref()) {
        return Ok(VmOutcome::PaymentRequired(requirements));
    }
    if !node_has_docker(state.as_ref()) {
        return Err("this node cannot host VMs (no container runtime)".to_string());
    }

    let resources = crate::vm::VmManager::vm_resources(&spec, state.gpu.is_some());
    state
        .resources
        .lock()
        .unwrap()
        .allocate(&resources)
        .map_err(|e| e.to_string())?;

    match state.vm.start(owner, &spec, resources.clone()).await {
        Ok(info) => {
            info!("VM up for {}: {} ({})", owner, info.vm_id, info.image);
            Ok(VmOutcome::Ready(info))
        }
        Err(e) => {
            // Roll back the reservation if the container never came up.
            state.resources.lock().unwrap().release(&resources);
            Err(format!("VM start failed: {e}"))
        }
    }
}

pub fn vm_status(state: &AppState, owner: &str) -> cloudiy_common::VmInfo {
    state.vm.status(owner).unwrap_or_else(|| cloudiy_common::VmInfo {
        vm_id: String::new(),
        owner: owner.to_string(),
        image: String::new(),
        state: "missing".to_string(),
        cpu_millis: 0,
        memory_mib: 0,
        gpu: false,
        volume: String::new(),
        ports: vec![],
        created_at: chrono::Utc::now(),
    })
}

/// Destroy the caller's VM and return its reservation to the pool.
pub async fn stop_vm(state: SharedState, owner: &str, wipe: bool) -> Result<(), String> {
    let released = state.vm.stop(owner, wipe).await.map_err(|e| e.to_string())?;
    state.resources.lock().unwrap().release(&released);
    info!("VM down for {owner} (wipe={wipe})");
    Ok(())
}

/// Dispatch a named kernel on the GPU (delegates to the WGSL runtime crate).
/// Input formats:
/// - `vector_add`: `"a1,a2,...;b1,b2,..."` (two equal-length vectors)
/// - `matrix_mul`: `"m,k,n;a1,...(m*k);b1,...(k*n)"` (row-major)
pub fn execute_on_gpu(
    gpu: &GpuExecutor,
    kernel: &str,
    input: &[u8],
    _params: &HashMap<String, String>,
) -> Result<String, String> {
    let text = std::str::from_utf8(input).map_err(|_| "input must be UTF-8 text".to_string())?;
    cloudiy_runtime::execute_kernel(gpu, kernel, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_accepts_equal_tokens() {
        assert!(tokens_match("cloudiy-dev-token", "cloudiy-dev-token"));
    }

    #[test]
    fn tokens_match_rejects_different_tokens() {
        assert!(!tokens_match("secret-a", "secret-b"));
        // Different lengths must also fail (and not panic).
        assert!(!tokens_match("short", "a-much-longer-token"));
    }

    /// End-to-end GPU tests — skipped gracefully on machines without a GPU.
    #[tokio::test]
    async fn gpu_vector_add_and_matmul() {
        let Ok(gpu) = GpuExecutor::new().await else {
            eprintln!("no GPU adapter available — skipping");
            return;
        };
        let params = HashMap::new();

        let out = execute_on_gpu(&gpu, "vector_add", b"1,2,3;10,20,30", &params).unwrap();
        assert_eq!(out, "11,22,33");

        // A(2x2) = [[1,2],[3,4]], B(2x2) = identity → A
        let out = execute_on_gpu(&gpu, "matrix_mul", b"2,2,2;1,2,3,4;1,0,0,1", &params).unwrap();
        assert_eq!(out, "1,2,3,4");

        let err = execute_on_gpu(&gpu, "mystery", b"", &params).unwrap_err();
        assert!(err.contains("mystery"));

        let err = execute_on_gpu(&gpu, "vector_add", b"1,2;1", &params).unwrap_err();
        assert!(err.contains("same length"));
    }

    #[test]
    fn decode_payment_roundtrip() {
        let payload = json!({"scheme": "exact", "network": "solana-devnet"});
        let encoded = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
        let decoded = decode_payment(&encoded).unwrap();
        assert_eq!(decoded["scheme"], "exact");
    }

    #[test]
    fn decode_payment_rejects_garbage() {
        assert!(decode_payment("not-base64!!!").is_none());
    }
}
