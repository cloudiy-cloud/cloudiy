//! # Cloudiy SDK (Rust)
//!
//! Lightweight consumer library for the Cloudiy GPU network — the
//! integration surface for apps and **AI agents**. The full node
//! (`cloudiy share`) is only needed by GPU *providers*; consumers just
//! embed this crate:
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use cloudiy_sdk::{Client, SubmitOptions};
//!
//! let client = Client::connect("<node-id>").await?;
//! println!("{:?}", client.info().await?);
//!
//! let result = client
//!     .submit(SubmitOptions::kernel("vector_add", "1,2,3;4,5,6").token("access-code"))
//!     .await?;
//! println!("{}", String::from_utf8_lossy(&result.output));
//! # Ok(()) }
//! ```
//!
//! Payment flow (x402): a submit without payment returns
//! [`SubmitError::PaymentRequired`] carrying the node's USDC [`Quote`] —
//! settle it (escrow `create_job` / x402 payload) and retry with
//! [`SubmitOptions::payment`].

// `SubmitError` intentionally carries the full `Quote` by value (a small,
// consumer-facing type) rather than boxing it behind an allocation.
#![allow(clippy::result_large_err)]

use base64::Engine;
use cloudiy_common::proto::{self, Request, Response};
use cloudiy_common::{JobRequest, NodeInfo, StatusResponse};
use serde_json::json;
use std::collections::HashMap;
use uuid::Uuid;

pub use cloudiy_common::{self as common, JobResponse};
pub use cloudiy_protocol::{self as protocol, WorkloadSpec};

/// Standalone demo x402 payload (flow demonstration only — real settlement
/// uses the Cloudiy escrow on devnet).
pub fn demo_payment_payload() -> String {
    let payload = json!({
        "x402Version": 1,
        "scheme": "exact",
        "network": "solana-devnet",
        "payload": {
            "from": cloudiy_common::load_pubkey().ok(),
            "note": "demo payment — settlement via Cloudiy escrow (devnet)",
        }
    });
    base64::engine::general_purpose::STANDARD.encode(payload.to_string())
}

/// Real x402 payload referencing a funded escrow `Job` account (base58) the
/// consumer created on-chain. The provider verifies it before executing.
pub fn escrow_payment_payload(escrow_account: &str) -> String {
    let payload = json!({
        "x402Version": 1,
        "scheme": "exact",
        "network": "solana-devnet",
        "escrow": escrow_account,
    });
    base64::engine::general_purpose::STANDARD.encode(payload.to_string())
}

/// A connected Cloudiy consumer. Cheap to clone requests over: one QUIC
/// connection, a fresh bi-stream per request.
pub struct Client {
    endpoint: iroh::Endpoint,
    conn: iroh::endpoint::Connection,
    node: iroh::EndpointId,
}

/// Typed view of an x402 payment quote (`accepts[0]` of the requirements).
#[derive(Debug, Clone)]
pub struct Quote {
    pub price_micro_usdc: u64,
    pub pay_to: String,
    pub asset: String,
    pub network: String,
    pub escrow_program: String,
    /// The raw x402 requirements document, for forward compatibility.
    pub raw: serde_json::Value,
}

impl Quote {
    pub fn price_usdc(&self) -> f64 {
        self.price_micro_usdc as f64 / 1_000_000.0
    }

    fn parse(raw: serde_json::Value) -> Self {
        let offer = &raw["accepts"][0];
        Quote {
            price_micro_usdc: offer["maxAmountRequired"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            pay_to: offer["payTo"].as_str().unwrap_or_default().to_string(),
            asset: offer["asset"].as_str().unwrap_or_default().to_string(),
            network: offer["network"].as_str().unwrap_or_default().to_string(),
            escrow_program: offer["extra"]["escrowProgram"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            raw,
        }
    }
}

/// A completed job with its provenance checked.
#[derive(Debug, Clone)]
pub struct JobResult {
    pub job_id: String,
    pub output: Vec<u8>,
    /// True when the result carried a valid ed25519 signature from the
    /// exact node this client dialed.
    pub signature_verified: bool,
    /// Decoded x402 settlement receipt, when the node returned one.
    pub payment_receipt: Option<serde_json::Value>,
    /// Provider's Solana pubkey (USDC payout address).
    pub provider_pubkey: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    /// The node quoted its price — settle and retry with a payment payload.
    #[error("payment required: {} USDC to {}", .0.price_usdc(), .0.pay_to)]
    PaymentRequired(Quote),
    /// The result arrived with a missing or invalid signature — do not trust it.
    #[error("result signature invalid or missing — refusing to trust output")]
    BadSignature,
    #[error("provider error: {0}")]
    Provider(String),
    #[error(transparent)]
    Transport(#[from] anyhow::Error),
}

/// Options for a job submission.
#[derive(Debug, Clone, Default)]
pub struct SubmitOptions {
    pub kernel: String,
    pub data: Vec<u8>,
    pub params: HashMap<String, String>,
    pub token: Option<String>,
    /// Base64-encoded x402 payment payload.
    pub payment: Option<String>,
    /// Require a valid provider signature (default: true).
    pub require_signature: bool,
}

impl SubmitOptions {
    pub fn kernel(kernel: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        SubmitOptions {
            kernel: kernel.into(),
            data: data.into(),
            require_signature: true,
            ..Default::default()
        }
    }

    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn payment(mut self, payment_b64: impl Into<String>) -> Self {
        self.payment = Some(payment_b64.into());
        self
    }

    /// Attach a demo x402 payload (flow demonstration only — real settlement
    /// uses the Cloudiy escrow on devnet).
    pub fn demo_payment(mut self) -> Self {
        let payload = json!({
            "x402Version": 1,
            "scheme": "exact",
            "network": "solana-devnet",
            "payload": {
                "from": cloudiy_common::load_pubkey().ok(),
                "note": "demo payment — settlement via Cloudiy escrow (devnet)",
            }
        });
        self.payment = Some(base64::engine::general_purpose::STANDARD.encode(payload.to_string()));
        self
    }

    /// Accept unsigned results (older providers). Not recommended.
    pub fn allow_unsigned(mut self) -> Self {
        self.require_signature = false;
        self
    }
}

impl Client {
    /// Dial a provider by Node ID (NAT traversal + E2E encryption via iroh).
    pub async fn connect(node_id: &str) -> anyhow::Result<Self> {
        let node: iroh::EndpointId = node_id
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid Node ID — copy it from `cloudiy share`"))?;
        let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0).await?;
        let conn = endpoint.connect(node, proto::ALPN).await?;
        Ok(Client {
            endpoint,
            conn,
            node,
        })
    }

    async fn request(&self, req: Request) -> anyhow::Result<Response> {
        let (mut send, mut recv) = self.conn.open_bi().await?;
        proto::write_msg(&mut send, &req).await?;
        proto::read_msg(&mut recv).await
    }

    /// Node capabilities, price and payout identity.
    pub async fn info(&self) -> anyhow::Result<NodeInfo> {
        match self.request(Request::Info).await? {
            Response::Info(info) => Ok(info),
            Response::Error { message } => anyhow::bail!("provider error: {message}"),
            other => anyhow::bail!("unexpected response: {other:?}"),
        }
    }

    /// Status of a previously submitted job.
    pub async fn status(&self, job_id: impl Into<String>) -> anyhow::Result<StatusResponse> {
        match self
            .request(Request::Status {
                job_id: job_id.into(),
            })
            .await?
        {
            Response::Status(s) => Ok(s),
            Response::Error { message } => anyhow::bail!("provider error: {message}"),
            other => anyhow::bail!("unexpected response: {other:?}"),
        }
    }

    /// Submit a job and wait for the signed result.
    pub async fn submit(&self, opts: SubmitOptions) -> Result<JobResult, SubmitError> {
        let job_id = Uuid::new_v4().to_string();
        let job = JobRequest {
            job_id: job_id.clone(),
            kernel: opts.kernel,
            input_data: opts.data,
            params: opts.params,
            auth_token: opts.token.unwrap_or_default(),
            consumer_pubkey: cloudiy_common::load_pubkey().ok(),
            payment: opts.payment,
        };

        let resp = self
            .request(Request::Submit(job))
            .await
            .map_err(SubmitError::Transport)?;
        self.handle_job_response(resp, opts.require_signature)
    }

    /// Open Compute Protocol: run a declared workload (image/template +
    /// command + resources + capabilities) on the node's isolated runtime.
    /// Same payment (x402) and signed-result semantics as `submit`.
    pub async fn run_workload(
        &self,
        spec: cloudiy_protocol::WorkloadSpec,
        token: Option<String>,
        payment: Option<String>,
    ) -> Result<JobResult, SubmitError> {
        let request = JobRequest {
            job_id: Uuid::new_v4().to_string(),
            kernel: "workload".to_string(),
            input_data: vec![],
            params: HashMap::new(),
            auth_token: token.unwrap_or_default(),
            consumer_pubkey: cloudiy_common::load_pubkey().ok(),
            payment,
        };
        let resp = self
            .request(Request::RunWorkload { request, spec })
            .await
            .map_err(SubmitError::Transport)?;
        self.handle_job_response(resp, true)
    }

    fn handle_job_response(
        &self,
        resp: Response,
        require_signature: bool,
    ) -> Result<JobResult, SubmitError> {
        match resp {
            Response::Job(resp) => {
                if resp.status == "error" {
                    return Err(SubmitError::Provider(
                        resp.error_message.unwrap_or_else(|| "unknown error".into()),
                    ));
                }
                let signature_verified = match (&resp.signature, &resp.signed_by) {
                    (Some(sig), Some(signed_by)) => {
                        *signed_by == self.node.to_string()
                            && cloudiy_common::verify_result(
                                &self.node,
                                &resp.job_id,
                                &resp.output_data,
                                sig,
                            )
                    }
                    _ => false,
                };
                if require_signature && !signature_verified {
                    return Err(SubmitError::BadSignature);
                }
                let payment_receipt = resp.payment_receipt.as_deref().and_then(|r| {
                    let bytes = base64::engine::general_purpose::STANDARD.decode(r).ok()?;
                    serde_json::from_slice(&bytes).ok()
                });
                Ok(JobResult {
                    job_id: resp.job_id,
                    output: resp.output_data,
                    signature_verified,
                    payment_receipt,
                    provider_pubkey: resp.provider_pubkey,
                })
            }
            Response::PaymentRequired { requirements } => {
                Err(SubmitError::PaymentRequired(Quote::parse(requirements)))
            }
            Response::Error { message } => Err(SubmitError::Provider(message)),
            other => Err(SubmitError::Transport(anyhow::anyhow!(
                "unexpected response: {other:?}"
            ))),
        }
    }

    /// Close the connection gracefully.
    pub async fn close(self) {
        self.conn.close(0u32.into(), b"done");
        self.endpoint.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_parses_x402_requirements() {
        let raw = json!({
            "x402Version": 1,
            "accepts": [{
                "scheme": "exact",
                "network": "solana-devnet",
                "maxAmountRequired": "20000",
                "payTo": "GnaU",
                "asset": "4zMM",
                "extra": { "escrowProgram": "9zMB" }
            }]
        });
        let q = Quote::parse(raw);
        assert_eq!(q.price_micro_usdc, 20_000);
        assert!((q.price_usdc() - 0.02).abs() < f64::EPSILON);
        assert_eq!(q.pay_to, "GnaU");
        assert_eq!(q.escrow_program, "9zMB");
    }

    #[test]
    fn submit_options_builder() {
        let opts = SubmitOptions::kernel("vector_add", "1,2;3,4")
            .token("t")
            .demo_payment();
        assert!(opts.require_signature);
        assert_eq!(opts.token.as_deref(), Some("t"));
        assert!(opts.payment.is_some());
    }
}
