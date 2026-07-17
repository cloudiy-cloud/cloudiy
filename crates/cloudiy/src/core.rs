//! Transport-agnostic node logic: state, payment gating (x402), job
//! execution and status. Both the P2P (iroh) and HTTP (axum) fronts call
//! into this module so behavior stays identical across transports.

use base64::Engine;
use cloudiy_common::{JobRequest, JobResponse, NodeInfo, StatusResponse};
use parking_lot::Mutex;
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
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

/// Interactive streams (shell sessions + TCP tunnels) served concurrently.
/// Each holds a stream — and a session a PTY — for its whole lifetime, so an
/// unbounded count is a trivial resource-exhaustion vector (M1). Excess opens
/// are refused rather than spawned.
pub const MAX_CONCURRENT_SESSIONS: usize = 16;

/// Inbound P2P streams accepted concurrently across the whole node. A peer can
/// open N bi-streams on one connection, and each stream buffers up to a full
/// frame ([`crate::proto::MAX_FRAME`]) while its request is read — *before* any
/// auth or per-request permit. An unbounded count is therefore a memory- and
/// task-exhaustion vector (MEDIUM). A stream that can't claim a permit is
/// refused with an error frame instead of being served. Kept ≥
/// [`MAX_CONCURRENT_SESSIONS`] so a session-heavy node hits the session cap on
/// its own terms rather than being starved here first.
pub const MAX_CONCURRENT_INBOUND_STREAMS: usize = 64;

/// Inbound streams a *single* connection may hold concurrently. The node-wide
/// budget alone lets one peer starve every other; this bounds any one
/// connection's share of it.
pub const MAX_INBOUND_STREAMS_PER_CONN: usize = 16;

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

/// A durable audit-trail line: when a job completed, plus its metadata (never
/// the payload bytes — those stay off the receipts log).
#[derive(serde::Serialize, serde::Deserialize)]
struct JobReceipt {
    at: chrono::DateTime<chrono::Utc>,
    job: JobResponse,
}

/// FIFO-bounded store of completed jobs, optionally backed by an append-only
/// JSONL receipts log so `status` and the audit trail survive a restart.
#[derive(Default)]
pub struct JobStore {
    map: HashMap<String, JobResponse>,
    order: VecDeque<String>,
    /// Receipts log path; `None` = in-memory only (tests/dev).
    persist_path: Option<std::path::PathBuf>,
    /// Appends since the last compaction — bounds the receipts file (see
    /// [`JobStore::compact`]).
    appends_since_compaction: usize,
}

impl JobStore {
    /// Open a durable store, replaying the last [`MAX_STORED_JOBS`] receipts
    /// from `path` into memory, then compacting so a large pre-existing log is
    /// trimmed to what's retained.
    pub fn with_persistence(path: std::path::PathBuf) -> Self {
        let mut store = JobStore {
            persist_path: Some(path.clone()),
            ..Default::default()
        };
        if let Ok(content) = std::fs::read_to_string(&path) {
            for line in content.lines() {
                if let Ok(r) = serde_json::from_str::<JobReceipt>(line) {
                    store.remember(r.job); // replay without re-appending
                }
            }
            info!(
                "Loaded {} job receipt(s) from {}",
                store.len(),
                path.display()
            );
            store.compact(); // trim the on-disk log to the retained window
        }
        store
    }

    /// Rewrite the receipts log from the retained (bounded) in-memory set,
    /// atomically (temp file + rename). Bounds the file to the same window as
    /// memory, so a long-running node's audit log can't grow without limit.
    /// Compaction stamps `at` with the current time (the per-receipt timestamp
    /// is best-effort metadata, never read back for logic).
    fn compact(&mut self) {
        let Some(path) = self.persist_path.clone() else {
            return;
        };
        let now = chrono::Utc::now();
        let mut out = String::new();
        for id in &self.order {
            if let Some(job) = self.map.get(id) {
                let receipt = JobReceipt {
                    at: now,
                    job: JobResponse {
                        output_data: Vec::new(),
                        ..job.clone()
                    },
                };
                if let Ok(line) = serde_json::to_string(&receipt) {
                    out.push_str(&line);
                    out.push('\n');
                }
            }
        }
        let tmp = path.with_extension("jsonl.tmp");
        if let Err(e) = std::fs::write(&tmp, &out).and_then(|()| std::fs::rename(&tmp, &path)) {
            warn!("job receipt compaction failed: {e}");
            return;
        }
        self.appends_since_compaction = 0;
    }

    /// In-memory bookkeeping with FIFO eviction (shared by insert + replay).
    fn remember(&mut self, job: JobResponse) {
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

    pub fn insert(&mut self, job: JobResponse) {
        // Persist a compact receipt (metadata only — never the payload) before
        // caching, so a crash right after completion can't lose the record.
        if let Some(path) = self.persist_path.clone() {
            let receipt = JobReceipt {
                at: chrono::Utc::now(),
                job: JobResponse {
                    output_data: Vec::new(),
                    ..job.clone()
                },
            };
            match serde_json::to_string(&receipt) {
                Ok(line) => {
                    if let Err(e) = append_line(&path, &line) {
                        warn!("failed to persist job receipt: {e}");
                    }
                }
                Err(e) => warn!("failed to serialize job receipt: {e}"),
            }
        }
        self.remember(job);

        // Keep the on-disk log bounded: after a window of appends, rewrite it
        // from the retained set (file stays ≤ ~2× MAX_STORED_JOBS lines).
        if self.persist_path.is_some() {
            self.appends_since_compaction += 1;
            if self.appends_since_compaction >= MAX_STORED_JOBS {
                self.compact();
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

/// Escrows already consumed by an admitted job (A1 anti-replay), bounded so a
/// long-running node can't grow it without limit. Eviction is safe: an escrow
/// is spent exactly once and closed on-chain when paid (C2), so a forgotten old
/// entry that gets re-submitted simply fails on-chain re-verification (the Job
/// account no longer exists) — the chain, not this set, is the authority.
#[derive(Default)]
pub struct ServedEscrows {
    seen: std::collections::HashSet<String>,
    order: VecDeque<String>,
}

/// Max escrow ids remembered for fast replay rejection.
pub const MAX_SERVED_ESCROWS: usize = 4096;

impl ServedEscrows {
    pub fn contains(&self, escrow: &str) -> bool {
        self.seen.contains(escrow)
    }

    pub fn insert(&mut self, escrow: String) {
        if self.seen.insert(escrow.clone()) {
            self.order.push_back(escrow);
            while self.order.len() > MAX_SERVED_ESCROWS {
                if let Some(oldest) = self.order.pop_front() {
                    self.seen.remove(&oldest);
                } else {
                    break;
                }
            }
        }
    }
}

/// Append one line to a file, creating it (and its parent dir) if needed.
fn append_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{line}")
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
    /// Bounds concurrent interactive streams (see [`MAX_CONCURRENT_SESSIONS`]).
    pub sessions: Arc<tokio::sync::Semaphore>,
    /// Bounds concurrent pre-auth inbound streams node-wide (see
    /// [`MAX_CONCURRENT_INBOUND_STREAMS`]).
    pub inbound: Arc<tokio::sync::Semaphore>,
    pub token: String,
    /// iroh EndpointId — the node's P2P identity/address.
    pub endpoint_id: String,
    /// Solana wallet pubkey for USDC payouts.
    pub pubkey: String,
    pub gpu_model: String,
    pub vram_mb: u64,
    pub price_micro_usdc: u64,
    /// Hourly lease price advertised for a dedicated VM on this node.
    pub price_micro_usdc_per_hour: u64,
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
    /// Solana RPC endpoint for on-chain escrow verification (None = off).
    pub rpc_url: Option<String>,
    /// When true, only a verified on-chain escrow admits a job — the dev
    /// token and demo payment are rejected.
    pub require_payment: bool,
    /// OCI runtime for containers/VMs (`runsc`, `kata-runtime`…); None = runc.
    pub container_runtime: Option<String>,
    /// Escrow accounts already consumed by an admitted job — an escrow funds
    /// exactly one execution, so a replayed submission against the same escrow
    /// is rejected here (A1: anti-replay).
    pub served_escrows: Mutex<ServedEscrows>,
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
    let jobs_completed = state.jobs.lock().len();
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
        resources: Some(state.resources.lock().clone()),
        capabilities: state.capabilities.clone(),
    }
}

pub fn job_status(state: &AppState, job_id: String) -> StatusResponse {
    let jobs = state.jobs.lock();
    if let Some(job) = jobs.get(&job_id) {
        StatusResponse {
            job_id,
            status: job.status.clone(),
            progress: if job.status == "completed" {
                100.0
            } else {
                50.0
            },
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

/// Map a UUID job id to the 16 raw bytes the escrow `Job.job_id` stores.
pub fn uuid_bytes(s: &str) -> Option<[u8; 16]> {
    uuid::Uuid::parse_str(s).ok().map(|u| *u.as_bytes())
}

/// x402 requirements annotated with why the last attempt failed.
fn payment_requirements_with_error(state: &AppState, error: &str) -> serde_json::Value {
    let mut req = payment_requirements(state);
    req["error"] = json!(error);
    req
}

/// Admission: decide how this request is paid for. A payment payload carrying
/// an `escrow` account is verified on-chain (when an RPC is configured) — real
/// locked USDC for this exact job. Otherwise the dev token / demo payment is a
/// bypass, disabled entirely under `--require-payment`.
/// On success, returns the settlement mode and the funded amount in
/// micro-USDC (0 for dev-mode paths that carry no on-chain budget).
pub async fn authorize(
    state: &AppState,
    req: &JobRequest,
    payment_override: Option<&str>,
) -> Result<(&'static str, u64), serde_json::Value> {
    let payment = payment_override
        .or(req.payment.as_deref())
        .and_then(decode_payment);
    let escrow = payment
        .as_ref()
        .and_then(|p| p.get("escrow"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let consumer_sig = payment
        .as_ref()
        .and_then(|p| p.get("consumer_sig"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    // Run-auth expiry (unix secs) the consumer signed over (MEDIUM-2).
    let auth_expiry = payment
        .as_ref()
        .and_then(|p| p.get("expiry"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // Real payment path: an escrow account we can verify on-chain.
    if let (Some(acct), Some(rpc)) = (escrow.as_deref(), state.rpc_url.as_deref()) {
        let Some(job_bytes) = uuid_bytes(&req.job_id) else {
            return Err(payment_requirements_with_error(
                state,
                "job_id must be a UUID to bind an escrow",
            ));
        };
        // A1: an escrow funds exactly one execution. Reject before touching the
        // chain if this escrow was already spent by an admitted job.
        if state.served_escrows.lock().contains(acct) {
            warn!(
                "Job {}: escrow {} already consumed — replay",
                req.job_id, acct
            );
            return Err(payment_requirements_with_error(
                state,
                "escrow already consumed by a prior job (replay)",
            ));
        }
        // A2: require enough runway before the escrow deadline that we finish
        // and can be released before the consumer could refund. Margin = the
        // hard job cap plus slack for release latency.
        let min_remaining = WORKLOAD_TIMEOUT_SECS as i64 + 120;
        let now = chrono::Utc::now().timestamp();
        return match crate::payments::verify_escrow(
            rpc,
            acct,
            ESCROW_PROGRAM,
            &state.pubkey,
            &state.usdc_mint,
            state.price_micro_usdc,
            job_bytes,
            min_remaining,
            consumer_sig.as_deref(),
            &req.input_data,
            auth_expiry,
            now,
        )
        .await
        {
            Ok(amount) => {
                // Reserve now that it verified — closes the replay window.
                state.served_escrows.lock().insert(acct.to_string());
                info!("Job {}: escrow {} verified on-chain", req.job_id, acct);
                Ok(("escrow-verified", amount))
            }
            Err(e) => {
                warn!("Job {}: escrow verification failed: {e}", req.job_id);
                Err(payment_requirements_with_error(state, &e))
            }
        };
    }

    if state.require_payment {
        warn!(
            "Job {}: rejected — this node requires a verified on-chain escrow",
            req.job_id
        );
        return Err(payment_requirements_with_error(
            state,
            "provider requires an on-chain escrow payment (attach `escrow`)",
        ));
    }

    // Dev-mode fallbacks (disabled by --require-payment above).
    if payment.is_some() {
        info!(
            "Job {}: accepted via demo payment (not verified on-chain)",
            req.job_id
        );
        Ok(("x402-demo", 0))
    } else if tokens_match(&req.auth_token, &state.token) {
        info!("Job {}: accepted via dev token", req.job_id);
        Ok(("dev-token", 0))
    } else {
        warn!(
            "Job {}: no payment and invalid token — payment required",
            req.job_id
        );
        Err(payment_requirements(state))
    }
}

/// Execute a kernel job. Payment is already authorized by the caller;
/// `settled_via` records how (for the receipt).
pub fn submit(state: &AppState, req: JobRequest, settled_via: &str) -> SubmitOutcome {
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
            // Offline-verifiable proof that THIS node produced THIS output for
            // the consumer's exact input (RFC-0006 §4) — the artifact the
            // escrow / delivery verification needs to release payment.
            let signature = cloudiy_common::sign_result(
                &state.secret,
                &req.job_id,
                &req.input_data,
                &output_data,
            );
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

    state.jobs.lock().insert(response.clone());

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
    // Authorize (incl. on-chain escrow verification) before reserving the GPU.
    let (settled_via, _funded) = match authorize(&state, &req, payment_override.as_deref()).await {
        Ok(v) => v,
        Err(requirements) => return Ok(SubmitOutcome::PaymentRequired(requirements)),
    };

    let Ok(permit) = state.busy.clone().try_acquire_owned() else {
        warn!("Job {}: refused — node at capacity", req.job_id);
        return Err("node busy — try again shortly".to_string());
    };

    let state2 = state.clone();
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        submit(&state2, req, settled_via)
    });

    match tokio::time::timeout(std::time::Duration::from_secs(JOB_TIMEOUT_SECS), job).await {
        Err(_) => Err(format!("job timed out after {JOB_TIMEOUT_SECS}s")),
        Ok(Err(e)) => Err(format!("job execution failed: {e}")),
        Ok(Ok(outcome)) => Ok(outcome),
    }
}

/// Sliding-window rate limit per consumer identity for endpoint runs. In-memory
/// and best-effort (resets on restart); bounds how fast one wallet can drive a
/// provider. Returns false when the caller is over the window's budget.
fn endpoint_rate_ok(owner: &str) -> bool {
    use std::time::Instant;
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
    const MAX_PER_WINDOW: usize = 30;
    static HITS: std::sync::OnceLock<
        parking_lot::Mutex<std::collections::HashMap<String, Vec<Instant>>>,
    > = std::sync::OnceLock::new();
    let map = HITS.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()));
    let now = Instant::now();
    let mut m = map.lock();
    let hits = m.entry(owner.to_string()).or_default();
    hits.retain(|t| now.duration_since(*t) < WINDOW);
    if hits.len() >= MAX_PER_WINDOW {
        return false;
    }
    hits.push(now);
    true
}

/// Serve a catalog model endpoint for a paying consumer. Admission is the same
/// x402/escrow gate as any job (`authorize`); the model then runs on this
/// provider host and its JSON output is signed with the node key, so an
/// endpoint run settles against a proof of the result exactly like a kernel
/// run — closing the gap where `/api/endpoint` used to be free and unmetered.
pub async fn run_endpoint_guarded(
    state: SharedState,
    req: JobRequest,
    key: String,
    prompt: String,
    owner: &str,
) -> Result<SubmitOutcome, String> {
    // Rate-limit per authenticated consumer identity so one wallet can't flood
    // the provider (DoS / cost-grief), checked before any work or payment.
    if !endpoint_rate_ok(owner) {
        return Err("rate limit exceeded — too many endpoint runs, slow down".to_string());
    }
    let (settled_via, _funded) = match authorize(&state, &req, None).await {
        Ok(v) => v,
        Err(requirements) => return Ok(SubmitOutcome::PaymentRequired(requirements)),
    };
    // A worker-level problem (no GPU, unknown key) comes back inside the JSON
    // as an `"error"` field rather than failing the request; the result is
    // still signed so the consumer can verify who produced it.
    // Remote runs carry a prompt only; audio uploads stay on the caller's
    // local gateway (they don't transit the protocol frame today).
    let output = crate::gateway::serve_endpoint(&key, &prompt, None).await;
    let bytes = serde_json::to_vec(&output).map_err(|e| format!("encoding result: {e}"))?;
    // The model input for an endpoint run is the prompt — bind it so the
    // signed result proves output-for-this-prompt (RFC-0006 §4).
    Ok(SubmitOutcome::Completed(signed_response(
        &state,
        &req.job_id,
        prompt.as_bytes(),
        bytes,
        settled_via,
    )))
}

/// Default wall-clock budget for container workloads (image pull included).
pub const WORKLOAD_TIMEOUT_SECS: u64 = 300;

fn signed_response(
    state: &AppState,
    job_id: &str,
    input: &[u8],
    output: Vec<u8>,
    settled_via: &str,
) -> JobResponse {
    let receipt = base64::engine::general_purpose::STANDARD.encode(
        json!({
            "success": true,
            "network": state.network,
            "settledVia": settled_via,
            "payee": state.pubkey,
        })
        .to_string(),
    );
    let signature = cloudiy_common::sign_result(&state.secret, job_id, input, &output);
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

    let (settled_via, _funded) = match authorize(&state, &req, payment_override.as_deref()).await {
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
        docker = DockerRuntime::with_runtime(state.container_runtime.clone());
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
    state.resources.lock().release(&spec.resources);

    let response = match result {
        Ok(logs) => signed_response(
            &state,
            &req.job_id,
            &req.input_data,
            logs.into_bytes(),
            settled_via,
        ),
        Err(message) => error_response(&state, &req.job_id, message),
    };
    state.jobs.lock().insert(response.clone());
    Ok(SubmitOutcome::Completed(response))
}

// --------------------------------------------------- CloudiyOS VM lifecycle

pub enum VmOutcome {
    Ready(cloudiy_common::VmInfo),
    PaymentRequired(serde_json::Value),
}

fn node_has_docker(state: &AppState) -> bool {
    state.capabilities.iter().any(|c| c.name() == "docker")
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
    let funded = match authorize(&state, &req, payment_override.as_deref()).await {
        Ok((_, amount)) => amount,
        Err(requirements) => return Ok(VmOutcome::PaymentRequired(requirements)),
    };
    if !node_has_docker(state.as_ref()) {
        return Err("this node cannot host VMs (no container runtime)".to_string());
    }

    let resources = crate::vm::VmManager::vm_resources(&spec, state.gpu.is_some());
    state
        .resources
        .lock()
        .allocate(&resources)
        .map_err(|e| e.to_string())?;

    // Prepaid compute lease: the funded escrow amount buys `funded / rate`
    // hours at this node's hourly price. `budget == 0` (dev-mode / unpaid) is
    // unmetered. The reaper (see `vm::VmManager::reap_expired`) stops a VM once
    // its lease is spent, so a tenant can't hold hardware indefinitely (#2).
    let rate = state.price_micro_usdc;
    match state
        .vm
        .start(owner, &spec, resources.clone(), rate, funded)
        .await
    {
        Ok(info) => {
            info!(
                "VM up for {}: {} ({}) — lease {} micro-USDC @ {}/h",
                owner, info.vm_id, info.image, funded, rate
            );
            Ok(VmOutcome::Ready(info))
        }
        Err(e) => {
            // Roll back the reservation if the container never came up.
            state.resources.lock().release(&resources);
            Err(format!("VM start failed: {e}"))
        }
    }
}

pub fn vm_status(state: &AppState, owner: &str) -> cloudiy_common::VmInfo {
    state
        .vm
        .status(owner)
        .unwrap_or_else(|| cloudiy_common::VmInfo {
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
            price_micro_usdc_per_hour: 0,
            lease_micro_usdc: 0,
            lease_remaining_secs: None,
        })
}

/// Destroy the caller's VM and return its reservation to the pool.
pub async fn stop_vm(state: SharedState, owner: &str, wipe: bool) -> Result<(), String> {
    let released = state
        .vm
        .stop(owner, wipe)
        .await
        .map_err(|e| e.to_string())?;
    state.resources.lock().release(&released);
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
    fn endpoint_rate_limit_bounds_a_single_identity() {
        // A fresh identity may run up to the window budget, then is throttled.
        let who = "test-owner-rate-A";
        for _ in 0..30 {
            assert!(endpoint_rate_ok(who), "within budget should pass");
        }
        assert!(!endpoint_rate_ok(who), "31st in the window is rejected");
        // A different identity is independent (not affected by the first).
        assert!(endpoint_rate_ok("test-owner-rate-B"));
    }

    #[test]
    fn inbound_stream_caps_are_consistent() {
        // The node-wide inbound budget must leave room for the full interactive
        // capacity, or streams would be refused before the session cap ever
        // applies.
        const { assert!(MAX_CONCURRENT_INBOUND_STREAMS >= MAX_CONCURRENT_SESSIONS) };
        // A single connection's share never exceeds the node-wide budget.
        const { assert!(MAX_INBOUND_STREAMS_PER_CONN <= MAX_CONCURRENT_INBOUND_STREAMS) };
        // Both caps must be positive, or the accept loop would refuse everything.
        const { assert!(MAX_INBOUND_STREAMS_PER_CONN > 0) };
    }

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

    #[test]
    fn job_store_persists_and_reloads_without_payload() {
        let path = std::env::temp_dir().join(format!("cloudiy-jobs-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);

        {
            let mut store = JobStore::with_persistence(path.clone());
            store.insert(JobResponse {
                job_id: "job-1".into(),
                output_data: b"large-payload-bytes".to_vec(),
                status: "completed".into(),
                error_message: None,
                provider_pubkey: Some("prov".into()),
                payment_receipt: Some("receipt".into()),
                signature: Some("sig".into()),
                signed_by: Some("node".into()),
            });
        }

        // Reopen from disk (simulating a restart): the record survives, but the
        // payload was never written to the audit log.
        let store = JobStore::with_persistence(path.clone());
        let got = store.get("job-1").expect("job survived restart");
        assert_eq!(got.status, "completed");
        assert_eq!(got.provider_pubkey.as_deref(), Some("prov"));
        assert_eq!(got.signature.as_deref(), Some("sig"));
        assert!(got.output_data.is_empty(), "payload must not be persisted");

        let _ = std::fs::remove_file(&path);
    }

    fn receipt(id: usize) -> JobResponse {
        JobResponse {
            job_id: format!("job-{id}"),
            output_data: vec![],
            status: "completed".into(),
            error_message: None,
            provider_pubkey: None,
            payment_receipt: None,
            signature: None,
            signed_by: None,
        }
    }

    #[test]
    fn served_escrows_evicts_oldest_beyond_cap() {
        let mut s = ServedEscrows::default();
        for i in 0..(MAX_SERVED_ESCROWS + 10) {
            s.insert(format!("escrow-{i}"));
        }
        assert!(s.seen.len() <= MAX_SERVED_ESCROWS, "set is bounded");
        assert!(!s.contains("escrow-0"), "oldest was evicted");
        assert!(
            s.contains(&format!("escrow-{}", MAX_SERVED_ESCROWS + 9)),
            "newest retained"
        );
        // Re-inserting a present key doesn't grow the order log.
        let before = s.order.len();
        let key = format!("escrow-{}", MAX_SERVED_ESCROWS + 9);
        s.insert(key);
        assert_eq!(s.order.len(), before);
    }

    #[test]
    fn job_log_is_compacted_to_the_retained_window() {
        let path =
            std::env::temp_dir().join(format!("cloudiy-compact-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut store = JobStore::with_persistence(path.clone());
        // Write well past the retention window + a compaction cycle.
        for i in 0..(MAX_STORED_JOBS + MAX_STORED_JOBS / 2) {
            store.insert(receipt(i));
        }
        // Memory is capped, and the file was compacted so it isn't the full
        // unbounded history.
        assert_eq!(store.len(), MAX_STORED_JOBS);
        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        assert!(
            lines <= 2 * MAX_STORED_JOBS,
            "log stays bounded, got {lines} lines"
        );
        let _ = std::fs::remove_file(&path);
    }
}
