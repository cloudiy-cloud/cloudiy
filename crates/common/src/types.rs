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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResponse {
    pub job_id: String,
    pub output_data: Vec<u8>,
    pub status: String,
    pub error_message: Option<String>,
    pub provider_pubkey: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub job_id: String,
    pub status: String,
    pub progress: f32,
    pub provider_pubkey: Option<String>,
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