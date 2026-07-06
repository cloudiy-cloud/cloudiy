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
