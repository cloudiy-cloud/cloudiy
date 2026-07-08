use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    pub job_id: String,
    pub kernel: String,
    pub input_data: Vec<u8>,
    pub params: HashMap<String, String>,
    pub auth_token: String,
    pub consumer_pubkey: Option<String>,
    /// x402 payment payload (base64-encoded JSON). Over HTTP this travels in
    /// the `X-PAYMENT` header; over P2P it is carried inline here.
    #[serde(default)]
    pub payment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResponse {
    pub job_id: String,
    pub output_data: Vec<u8>,
    pub status: String,
    pub error_message: Option<String>,
    pub provider_pubkey: Option<String>,
    /// x402 settlement receipt (base64-encoded JSON). Over HTTP this travels
    /// in the `X-PAYMENT-RESPONSE` header; over P2P it is carried inline.
    #[serde(default)]
    pub payment_receipt: Option<String>,
    /// Hex ed25519 signature over `(job_id, sha256(output_data))` by the
    /// provider's node key — see `cloudiy_common::sig`.
    #[serde(default)]
    pub signature: Option<String>,
    /// EndpointId (node identity) that produced `signature`.
    #[serde(default)]
    pub signed_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub job_id: String,
    pub status: String,
    pub progress: f32,
    pub provider_pubkey: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub protocol: String,
    pub version: String,
    /// iroh EndpointId — the node's network identity.
    pub endpoint_id: String,
    /// Solana wallet pubkey for USDC payouts, if configured.
    pub solana_pubkey: Option<String>,
    pub gpu_model: String,
    pub vram_mb: u64,
    pub jobs_completed: usize,
    /// Price per job in USDC, quoted via x402.
    pub price_usdc: f64,
    pub usdc_mint: String,
    pub network: String,
    pub payment: String,
    pub escrow_program: String,
    pub fee_bps: u16,
    /// Open Compute Protocol announcement: total/shared/allocated resources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<cloudiy_protocol::Resources>,
    /// Functionality this node offers (`docker`, `wgsl`, `linux`, `arm64`…).
    #[serde(default)]
    pub capabilities: Vec<cloudiy_protocol::Capability>,
}

/// Descriptor of a persistent, identity-bound VM on a provider (CloudiyOS).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmInfo {
    /// Stable id derived from the owner identity (one VM per identity/node).
    pub vm_id: String,
    /// Owner identity (consumer EndpointId).
    pub owner: String,
    pub image: String,
    /// "running", "stopped", "missing".
    pub state: String,
    pub cpu_millis: u64,
    pub memory_mib: u64,
    pub gpu: bool,
    /// Named persistent volume backing the VM's home (survives stop/start).
    pub volume: String,
    /// Ports the VM publishes on the provider, reachable via `cloudiy tunnel`.
    pub ports: Vec<u16>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Prepaid compute lease: hourly rate, total budget, and seconds left.
    /// `lease_micro_usdc == 0` means unmetered (dev mode); `lease_remaining_secs`
    /// is `None` when unmetered.
    #[serde(default)]
    pub price_micro_usdc_per_hour: u64,
    #[serde(default)]
    pub lease_micro_usdc: u64,
    #[serde(default)]
    pub lease_remaining_secs: Option<i64>,
}

/// A provider announcement signed by the announcing node key. Verifiable
/// end-to-end: directories only relay it — they cannot forge or alter it,
/// and consumers verify the signature before trusting a single field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedAnnouncement {
    /// JSON-serialized `cloudiy_protocol::ProviderAnnouncement` — exactly
    /// the bytes that were signed (serialize once, verify byte-for-byte).
    pub payload: String,
    /// Unix seconds when signed. Announcements expire after
    /// [`crate::sig::ANNOUNCE_TTL_SECS`] — heartbeats refresh them.
    pub issued_at: i64,
    /// EndpointId of the announcer (must match the identity in `payload`).
    pub signed_by: String,
    /// Hex ed25519 signature over the announce domain payload.
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub pubkey: String,
    pub gpu_model: String,
    pub vram_mb: u64,
    pub cuda_cores: u32,
    pub endpoint: String,
    pub reputation_score: f64,
    pub last_heartbeat: chrono::DateTime<chrono::Utc>,
}
