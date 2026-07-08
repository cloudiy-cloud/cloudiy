//! P2P front: accepts iroh QUIC connections (hole-punched / relayed, E2E
//! encrypted, peer authenticated by ed25519 EndpointId) and serves the
//! Cloudiy wire protocol. One-shot RPCs get one response frame; OpenSession
//! and Tunnel take over the stream for the duration.
//!
//! VM ownership is bound to the **authenticated QUIC peer identity**
//! (`conn.remote_id()`), never to a self-declared request field.

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
    let owner = conn.remote_id().to_string();
    info!("peer connected: {owner}");

    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(streams) => streams,
            Err(_) => break, // connection closed by peer
        };
        let state = state.clone();
        let owner = owner.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_stream(send, recv, state, owner).await {
                warn!("stream error: {e:#}");
            }
        });
    }
    Ok(())
}

async fn handle_stream(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    state: SharedState,
    owner: String,
) -> Result<()> {
    let req = match proto::read_msg::<Request>(&mut recv).await {
        Ok(r) => r,
        Err(e) => {
            let _ = proto::write_msg(
                &mut send,
                &Response::Error {
                    message: format!("bad request: {e}"),
                },
            )
            .await;
            return Ok(());
        }
    };

    match req {
        // Streams that take over the connection. Both hold the stream (and a
        // session also a PTY) for their whole lifetime, so a permit from the
        // interactive-stream semaphore is required first (M1); excess opens are
        // refused with an error frame instead of exhausting the node.
        Request::OpenSession {
            command,
            cols,
            rows,
            ..
        } => {
            let Ok(_permit) = state.sessions.clone().try_acquire_owned() else {
                return proto::write_msg(&mut send, &session_busy()).await;
            };
            handle_session(send, recv, state, owner, command, cols, rows).await
        }
        Request::Tunnel { port, .. } => {
            let Ok(_permit) = state.sessions.clone().try_acquire_owned() else {
                return proto::write_msg(&mut send, &session_busy()).await;
            };
            handle_tunnel(send, recv, state, owner, port).await
        }
        // One-shot RPCs.
        other => {
            let resp = handle_rpc(other, state, owner).await;
            proto::write_msg(&mut send, &resp).await
        }
    }
}

/// Error frame returned when the interactive-stream semaphore is exhausted.
fn session_busy() -> Response {
    Response::Error {
        message: "provider is at interactive-session capacity — try again shortly".to_string(),
    }
}

async fn handle_rpc(req: Request, state: SharedState, owner: String) -> Response {
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
        Request::StartVm { request, spec } => {
            match core::start_vm(state, &owner, request, spec, None).await {
                Ok(core::VmOutcome::Ready(info)) => Response::Vm(info),
                Ok(core::VmOutcome::PaymentRequired(requirements)) => {
                    Response::PaymentRequired { requirements }
                }
                Err(message) => Response::Error { message },
            }
        }
        Request::VmStatus { .. } => Response::Vm(core::vm_status(&state, &owner)),
        Request::StopVm { wipe, .. } => match core::stop_vm(state, &owner, wipe).await {
            Ok(()) => Response::Ack,
            Err(message) => Response::Error { message },
        },
        // Discovery messages are served by directory nodes.
        Request::Announce(_) | Request::Providers => Response::Error {
            message: "this is a provider node — Announce/Providers go to a directory node"
                .to_string(),
        },
        // Handled before reaching here.
        Request::OpenSession { .. } | Request::Tunnel { .. } => Response::Error {
            message: "internal routing error".to_string(),
        },
    }
}

/// Interactive shell into the caller's VM. Requires an already-provisioned VM
/// (owned by this authenticated peer); emits `SessionOpened`, then pumps
/// binary session frames until the shell exits.
#[allow(clippy::too_many_arguments)]
async fn handle_session(
    mut send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    state: SharedState,
    owner: String,
    command: Vec<String>,
    cols: u16,
    rows: u16,
) -> Result<()> {
    if !state.vm.has_running(&owner) {
        proto::write_msg(
            &mut send,
            &Response::Error {
                message: "no VM for this identity — run `cloudiy vm up` first".to_string(),
            },
        )
        .await?;
        return Ok(());
    }

    let pty = match state.vm.open_shell(&owner, &command, cols, rows).await {
        Ok(p) => p,
        Err(e) => {
            proto::write_msg(
                &mut send,
                &Response::Error {
                    message: format!("cannot open shell: {e}"),
                },
            )
            .await?;
            return Ok(());
        }
    };

    let vm_id = state.vm.status(&owner).map(|v| v.vm_id).unwrap_or_default();
    proto::write_msg(&mut send, &Response::SessionOpened { vm_id }).await?;
    info!("session opened for {owner}");
    crate::session::pump(send, recv, pty).await
}

/// Forward the stream to `127.0.0.1:port` on the provider — a port the
/// caller's VM published. After `Ack`, raw bidirectional byte copy.
async fn handle_tunnel(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    state: SharedState,
    owner: String,
    port: u16,
) -> Result<()> {
    let allowed = state
        .vm
        .status(&owner)
        .map(|v| v.ports.contains(&port))
        .unwrap_or(false);
    if !allowed {
        proto::write_msg(
            &mut send,
            &Response::Error {
                message: format!("port {port} is not published by your VM"),
            },
        )
        .await?;
        return Ok(());
    }

    let upstream = match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
        Ok(s) => s,
        Err(e) => {
            proto::write_msg(
                &mut send,
                &Response::Error {
                    message: format!("cannot reach 127.0.0.1:{port}: {e}"),
                },
            )
            .await?;
            return Ok(());
        }
    };
    proto::write_msg(&mut send, &Response::Ack).await?;
    info!("tunnel to 127.0.0.1:{port} for {owner}");

    // From here the stream is raw bytes in both directions.
    let (mut up_rd, mut up_wr) = upstream.into_split();
    let c2p = tokio::io::copy(&mut recv, &mut up_wr);
    let p2c = tokio::io::copy(&mut up_rd, &mut send);
    let _ = tokio::try_join!(c2p, p2c);
    let _ = send.finish();
    Ok(())
}
