//! Cloudiy wire protocol over iroh QUIC bi-streams.
//!
//! Every message is a **length-prefixed frame** (`u32` big-endian length +
//! payload), so a stream is self-delimiting and can carry either a single
//! request/response exchange *or* a long-lived interactive session /
//! byte-tunnel without ever relying on stream-finish to mark boundaries.
//!
//! - One-shot RPC (Submit, Info, Announce, Providers…): client writes one
//!   request frame, server writes one response frame.
//! - Interactive session (OpenSession): request frame → `SessionOpened`
//!   response frame → a duplex stream of binary [`SessionFrame`]s until exit.
//! - Tunnel: request frame → `Ack` → raw bidirectional byte copy.

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{JobRequest, JobResponse, NodeInfo, SignedAnnouncement, StatusResponse, VmInfo};

/// ALPN identifying the Cloudiy protocol (bump the suffix on breaking changes).
pub const ALPN: &[u8] = b"cloudiy/0";

/// Upper bound for any single protocol frame, requests and responses alike.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

/// One row of the directory's demand oracle: how much recent consumer interest
/// an endpoint key has drawn vs how many providers currently announce it.
/// `recent_interest / (providers + 1)` is a supply-adjusted demand signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemandEntry {
    pub key: String,
    /// Interest pings within the directory's rolling window (default ~1h).
    pub recent_interest: u32,
    /// Fresh providers announcing this key right now.
    pub providers: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    Submit(JobRequest),
    /// Open Compute Protocol: run a declared workload (image/template +
    /// command + resources + capabilities) in an isolated runtime.
    RunWorkload {
        request: JobRequest,
        spec: cloudiy_protocol::WorkloadSpec,
    },
    Status {
        job_id: String,
    },
    Info,
    /// Discovery: register/refresh a signed announcement on a directory node.
    Announce(SignedAnnouncement),
    /// Discovery: list fresh provider announcements (consumer re-verifies all).
    Providers,
    /// Discovery: the directory's authoritative, canary-derived reputation per
    /// provider (RFC-0006 §6) — consumers rank on *earned* trust, overriding a
    /// provider's self-reported `reputation`.
    Reputation,
    /// Demand oracle (phase 2): a consumer/gateway signals interest in a model
    /// endpoint. The directory keeps a windowed count so providers can learn
    /// what is in demand (and under-supplied) before hosting it. Fire-and-forget
    /// — the directory replies `Ack`.
    EndpointInterest {
        key: String,
    },
    /// Demand oracle: a provider polls the directory for the per-endpoint demand
    /// table (recent interest vs current supply) to drive auto-hosting.
    Demand,

    // --- CloudiyOS: persistent, identity-bound VMs -----------------------
    /// Create (or return the existing) persistent VM for the caller identity.
    StartVm {
        request: JobRequest,
        spec: cloudiy_protocol::WorkloadSpec,
    },
    /// Current state of the caller's VM.
    VmStatus {
        request: JobRequest,
    },
    /// Destroy the caller's VM. `wipe` also deletes its persistent volume.
    StopVm {
        request: JobRequest,
        wipe: bool,
    },
    /// Open an interactive shell into the caller's VM. After a
    /// `Response::SessionOpened` the stream carries binary [`SessionFrame`]s.
    OpenSession {
        request: JobRequest,
        /// Optional shell/argv; empty = the VM's default shell.
        command: Vec<String>,
        /// Initial pseudo-terminal size (0 = default 80x24).
        #[serde(default)]
        cols: u16,
        #[serde(default)]
        rows: u16,
    },
    /// Forward a TCP connection to `127.0.0.1:port` on the provider (a port
    /// published by the caller's VM). After a `Response::Ack` the stream is a
    /// raw bidirectional byte copy — no framing.
    Tunnel {
        request: JobRequest,
        port: u16,
    },
    /// Run a catalog model endpoint (App Store "Models") that this provider
    /// hosts. Payment is admitted like any other job; the signed output is a
    /// `Response::Job` whose `output_data` is the model's JSON result.
    RunEndpoint {
        request: JobRequest,
        /// Catalog key (e.g. `llama-ep`, `flux2`).
        key: String,
        prompt: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Job(JobResponse),
    Status(StatusResponse),
    Info(NodeInfo),
    /// x402 "402 Payment Required": retry with a valid `payment` payload.
    PaymentRequired {
        requirements: serde_json::Value,
    },
    /// Positive acknowledgement for requests with no other payload.
    Ack,
    Providers(Vec<SignedAnnouncement>),
    /// Demand oracle table: per-endpoint recent interest and current supply.
    Demand(Vec<DemandEntry>),
    /// Authoritative canary-derived reputation, **signed by the directory's node
    /// key** (RFC-0006 §6/§10) so it is non-repudiable and tamper-evident even
    /// when relayed. The consumer verifies it against the directory it dialed.
    Reputation(crate::SignedReputation),
    /// A VM's descriptor (StartVm / VmStatus).
    Vm(VmInfo),
    /// Interactive session accepted — the stream now speaks [`SessionFrame`].
    SessionOpened {
        vm_id: String,
    },
    Error {
        message: String,
    },
}

// --------------------------------------------------------------- framing

/// Write a length-prefixed frame. Does **not** finish the stream, so more
/// frames (or a session) may follow.
pub async fn write_frame(send: &mut iroh::endpoint::SendStream, bytes: &[u8]) -> Result<()> {
    anyhow::ensure!(bytes.len() <= MAX_FRAME, "frame exceeds MAX_FRAME");
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    send.write_all(bytes).await?;
    Ok(())
}

/// Read one length-prefixed frame. Returns `None` on a clean end-of-stream
/// (peer closed between frames).
pub async fn read_frame(recv: &mut iroh::endpoint::RecvStream) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match recv.read_exact(&mut len_buf).await {
        Ok(()) => {}
        // Clean boundary: nothing more on this stream.
        Err(iroh::endpoint::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(e).context("reading frame length"),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    anyhow::ensure!(len <= MAX_FRAME, "frame length exceeds MAX_FRAME");
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .context("reading frame body")?;
    Ok(Some(buf))
}

pub async fn write_msg<T: Serialize>(send: &mut iroh::endpoint::SendStream, msg: &T) -> Result<()> {
    write_frame(send, &serde_json::to_vec(msg)?).await
}

pub async fn read_msg<T: DeserializeOwned>(recv: &mut iroh::endpoint::RecvStream) -> Result<T> {
    let frame = read_frame(recv)
        .await?
        .context("stream closed before a message was received")?;
    Ok(serde_json::from_slice(&frame)?)
}

// ----------------------------------------------------- interactive sessions

/// A frame in an interactive session, either direction. Binary-framed (not
/// JSON) so terminal bytes travel without per-byte encoding overhead.
///
/// Wire form: `[u32 len][u8 tag][payload]` (len covers tag+payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionFrame {
    /// stdin (client→server) or stdout/stderr (server→client).
    Data(Vec<u8>),
    /// Client→server: resize the pseudo-terminal.
    Resize { cols: u16, rows: u16 },
    /// Client→server: close stdin (EOF).
    Eof,
    /// Server→client: the session process exited.
    Exit(Option<i32>),
    /// Server→client: fatal session error.
    Error(String),
}

const TAG_DATA: u8 = 1;
const TAG_RESIZE: u8 = 2;
const TAG_EOF: u8 = 3;
const TAG_EXIT: u8 = 4;
const TAG_ERROR: u8 = 5;

pub async fn write_session_frame(
    send: &mut iroh::endpoint::SendStream,
    frame: &SessionFrame,
) -> Result<()> {
    let mut buf = Vec::new();
    match frame {
        SessionFrame::Data(d) => {
            buf.push(TAG_DATA);
            buf.extend_from_slice(d);
        }
        SessionFrame::Resize { cols, rows } => {
            buf.push(TAG_RESIZE);
            buf.extend_from_slice(&cols.to_be_bytes());
            buf.extend_from_slice(&rows.to_be_bytes());
        }
        SessionFrame::Eof => buf.push(TAG_EOF),
        SessionFrame::Exit(code) => {
            buf.push(TAG_EXIT);
            buf.extend_from_slice(&code.unwrap_or(i32::MIN).to_be_bytes());
        }
        SessionFrame::Error(msg) => {
            buf.push(TAG_ERROR);
            buf.extend_from_slice(msg.as_bytes());
        }
    }
    write_frame(send, &buf).await
}

pub async fn read_session_frame(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<Option<SessionFrame>> {
    let Some(buf) = read_frame(recv).await? else {
        return Ok(None);
    };
    let (tag, payload) = buf.split_first().context("empty session frame")?;
    let frame = match *tag {
        TAG_DATA => SessionFrame::Data(payload.to_vec()),
        TAG_RESIZE => {
            anyhow::ensure!(payload.len() == 4, "bad resize frame");
            SessionFrame::Resize {
                cols: u16::from_be_bytes([payload[0], payload[1]]),
                rows: u16::from_be_bytes([payload[2], payload[3]]),
            }
        }
        TAG_EOF => SessionFrame::Eof,
        TAG_EXIT => {
            anyhow::ensure!(payload.len() == 4, "bad exit frame");
            let code = i32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
            SessionFrame::Exit((code != i32::MIN).then_some(code))
        }
        TAG_ERROR => SessionFrame::Error(String::from_utf8_lossy(payload).into_owned()),
        other => anyhow::bail!("unknown session frame tag {other}"),
    };
    Ok(Some(frame))
}
