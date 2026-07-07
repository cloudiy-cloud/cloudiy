//! Cloudiy wire protocol: JSON request/response over iroh QUIC bi-streams.
//!
//! One request per bi-directional stream. The sender writes the JSON payload
//! and finishes the stream; the receiver reads to end-of-stream (bounded by
//! [`MAX_FRAME`]) and replies the same way. Connections stay open so a client
//! can issue multiple requests over new streams.

use anyhow::Result;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{JobRequest, JobResponse, NodeInfo, SignedAnnouncement, StatusResponse};

/// ALPN identifying the Cloudiy protocol (bump the suffix on breaking changes).
pub const ALPN: &[u8] = b"cloudiy/0";

/// Upper bound for any single protocol message, requests and responses alike.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    Submit(JobRequest),
    /// Open Compute Protocol: run a declared workload (image/template +
    /// command + resources + capabilities) in an isolated runtime.
    /// `request` carries identity/auth/payment; `spec` carries the WHAT.
    RunWorkload {
        request: JobRequest,
        spec: cloudiy_protocol::WorkloadSpec,
    },
    Status { job_id: String },
    Info,
    /// Discovery: a provider registers/refreshes its signed announcement on
    /// a directory node. Heartbeat = re-announcing before the TTL lapses.
    Announce(SignedAnnouncement),
    /// Discovery: list currently fresh provider announcements. Consumers
    /// verify every signature themselves — the directory is untrusted relay.
    Providers,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Job(JobResponse),
    Status(StatusResponse),
    Info(NodeInfo),
    /// x402 "402 Payment Required" equivalent: the caller must retry the
    /// submit with a valid `payment` payload satisfying these requirements.
    PaymentRequired { requirements: serde_json::Value },
    /// Positive acknowledgement for requests with no other payload (Announce).
    Ack,
    /// Fresh provider announcements known to a directory node.
    Providers(Vec<SignedAnnouncement>),
    Error { message: String },
}

pub async fn write_msg<T: Serialize>(
    send: &mut iroh::endpoint::SendStream,
    msg: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec(msg)?;
    anyhow::ensure!(bytes.len() <= MAX_FRAME, "message exceeds MAX_FRAME");
    send.write_all(&bytes).await?;
    send.finish()?;
    Ok(())
}

pub async fn read_msg<T: DeserializeOwned>(recv: &mut iroh::endpoint::RecvStream) -> Result<T> {
    let bytes = recv.read_to_end(MAX_FRAME).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
