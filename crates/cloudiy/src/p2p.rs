//! P2P front: accepts iroh QUIC connections (hole-punched / relayed, E2E
//! encrypted, peer authenticated by ed25519 EndpointId) and serves the
//! Cloudiy wire protocol — one request per bi-stream.

use anyhow::Result;
use cloudiy_common::proto::{self, Request, Response};
use tracing::{info, warn};

use crate::core::{self, SharedState};

pub async fn serve(endpoint: iroh::Endpoint, state: SharedState) -> Result<()> {
    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    if let Err(e) = handle_conn(conn, state).await {
                        warn!("connection error: {e:#}");
                    }
                }
                Err(e) => warn!("incoming connection failed: {e}"),
            }
        });
    }
    Ok(())
}

async fn handle_conn(conn: iroh::endpoint::Connection, state: SharedState) -> Result<()> {
    let remote = conn.remote_id();
    info!("peer connected: {remote:?}");

    // A client may issue several requests over one connection, each on its
    // own bi-stream. The loop ends when the peer closes the connection.
    loop {
        let (mut send, mut recv) = match conn.accept_bi().await {
            Ok(streams) => streams,
            Err(_) => break, // connection closed by peer
        };
        let state = state.clone();
        tokio::spawn(async move {
            let resp = match proto::read_msg::<Request>(&mut recv).await {
                Ok(req) => handle_request(req, state).await,
                Err(e) => Response::Error {
                    message: format!("bad request: {e}"),
                },
            };
            if let Err(e) = proto::write_msg(&mut send, &resp).await {
                warn!("failed to send response: {e:#}");
            }
        });
    }
    Ok(())
}

async fn handle_request(req: Request, state: SharedState) -> Response {
    match req {
        Request::Info => Response::Info(core::node_info(&state)),
        Request::Status { job_id } => Response::Status(core::job_status(&state, job_id)),
        Request::Submit(job) => match core::submit_guarded(state, job, None).await {
            Ok(core::SubmitOutcome::Completed(r)) => Response::Job(r),
            Ok(core::SubmitOutcome::PaymentRequired(requirements)) => {
                Response::PaymentRequired { requirements }
            }
            Err(message) => Response::Error { message },
        },
        Request::RunWorkload { request, spec } => {
            match core::run_workload(state, request, spec, None).await {
                Ok(core::SubmitOutcome::Completed(r)) => Response::Job(r),
                Ok(core::SubmitOutcome::PaymentRequired(requirements)) => {
                    Response::PaymentRequired { requirements }
                }
                Err(message) => Response::Error { message },
            }
        }
        // Discovery messages are served by directory nodes (`cloudiy directory`).
        Request::Announce(_) | Request::Providers => Response::Error {
            message: "this is a provider node — Announce/Providers go to a directory node"
                .to_string(),
        },
    }
}
