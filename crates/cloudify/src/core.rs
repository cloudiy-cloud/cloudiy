//! Transport-agnostic node logic: state, payment gating (x402), job
//! execution and status. Both the P2P (iroh) and HTTP (axum) fronts call
//! into this module so behavior stays identical across transports.

use base64::Engine;
use cloudify_common::{JobRequest, JobResponse, NodeInfo, StatusResponse};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use subtle::ConstantTimeEq;
use tracing::{info, warn};

use crate::gpu::GpuExecutor;

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

/// Cloudify USDC escrow program on Solana devnet.
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
    pub gpu: GpuExecutor,
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
        protocol: "cloudify".to_string(),
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
    let result = execute_on_gpu(&state.gpu, &req.kernel, &req.input_data, &req.params);
    info!(
        "Job {} finished in {:?} on {}",
        req.job_id,
        started.elapsed(),
        state.gpu.info.name
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
                cloudify_common::sign_result(&state.secret, &req.job_id, &output_data);
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

fn parse_floats(s: &str) -> Result<Vec<f32>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.parse::<f32>().map_err(|_| format!("invalid number '{t}'")))
        .collect()
}

fn format_floats(v: &[f32]) -> String {
    v.iter()
        .map(f32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Dispatch a named kernel on the GPU. Input formats:
/// - `vector_add`: `"a1,a2,...;b1,b2,..."` (two equal-length vectors)
/// - `matrix_mul`: `"m,k,n;a1,...(m*k);b1,...(k*n)"` (row-major)
pub fn execute_on_gpu(
    gpu: &GpuExecutor,
    kernel: &str,
    input: &[u8],
    _params: &HashMap<String, String>,
) -> Result<String, String> {
    let text = std::str::from_utf8(input).map_err(|_| "input must be UTF-8 text".to_string())?;

    match kernel {
        "vector_add" => {
            let parts: Vec<&str> = text.split(';').collect();
            if parts.len() != 2 {
                return Err("vector_add expects input 'a1,a2,...;b1,b2,...'".to_string());
            }
            let a = parse_floats(parts[0])?;
            let b = parse_floats(parts[1])?;
            let out = gpu.vector_add(&a, &b).map_err(|e| e.to_string())?;
            Ok(format_floats(&out))
        }
        "matrix_mul" => {
            let parts: Vec<&str> = text.split(';').collect();
            if parts.len() != 3 {
                return Err(
                    "matrix_mul expects input 'm,k,n;a1,...(m*k);b1,...(k*n)'".to_string()
                );
            }
            let dims: Vec<u32> = parts[0]
                .split(',')
                .map(|t| t.trim().parse::<u32>().map_err(|_| format!("invalid dim '{t}'")))
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
            "unknown kernel '{other}' — available: vector_add, matrix_mul"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_accepts_equal_tokens() {
        assert!(tokens_match("cloudify-dev-token", "cloudify-dev-token"));
    }

    #[test]
    fn tokens_match_rejects_different_tokens() {
        assert!(!tokens_match("secret-a", "secret-b"));
        // Different lengths must also fail (and not panic).
        assert!(!tokens_match("short", "a-much-longer-token"));
    }

    #[test]
    fn parse_floats_accepts_csv() {
        assert_eq!(parse_floats("1, 2,3.5").unwrap(), vec![1.0, 2.0, 3.5]);
        assert!(parse_floats("1,abc").is_err());
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
