//! Directory node — the bootstrap discovery layer of the network.
//!
//! Deliberately dumb: it stores fresh *signed* announcements and hands them
//! out. It cannot forge entries (signatures are the announcer's) and it is
//! not trusted by consumers (they re-verify every signature). Scheduling
//! happens client-side, so anyone can run a directory and swapping it for
//! gossip or an on-chain registry later changes nothing else.

use anyhow::Result;
use cloudiy_common::proto::{self, Request, Response};
use cloudiy_common::SignedAnnouncement;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

/// Upper bound on stored announcements (memory cap; ~1 KiB each).
const MAX_ANNOUNCEMENTS: usize = 10_000;

#[derive(Default)]
struct Store {
    by_node: HashMap<String, SignedAnnouncement>,
}

impl Store {
    fn prune(&mut self, now: i64) {
        self.by_node
            .retain(|_, sa| cloudiy_common::verify_announcement(sa, now).is_ok());
    }

    fn insert(&mut self, sa: SignedAnnouncement, now: i64) -> Result<(), String> {
        // Full verification at the door: freshness, signature, identity match.
        cloudiy_common::verify_announcement(&sa, now)?;
        self.prune(now);
        if !self.by_node.contains_key(&sa.signed_by) && self.by_node.len() >= MAX_ANNOUNCEMENTS {
            return Err("directory full".to_string());
        }
        self.by_node.insert(sa.signed_by.clone(), sa);
        Ok(())
    }

    fn fresh(&mut self, now: i64) -> Vec<SignedAnnouncement> {
        self.prune(now);
        self.by_node.values().cloned().collect()
    }
}

pub async fn serve(endpoint: iroh::Endpoint) -> Result<()> {
    let store = Arc::new(Mutex::new(Store::default()));

    while let Some(incoming) = endpoint.accept().await {
        let store = store.clone();
        tokio::spawn(async move {
            let conn = match incoming.await {
                Ok(conn) => conn,
                Err(e) => {
                    warn!("incoming connection failed: {e}");
                    return;
                }
            };
            loop {
                let (mut send, mut recv) = match conn.accept_bi().await {
                    Ok(streams) => streams,
                    Err(_) => break,
                };
                let store = store.clone();
                tokio::spawn(async move {
                    let resp = match proto::read_msg::<Request>(&mut recv).await {
                        Ok(req) => handle(req, &store),
                        Err(e) => Response::Error {
                            message: format!("bad request: {e}"),
                        },
                    };
                    if let Err(e) = proto::write_msg(&mut send, &resp).await {
                        warn!("failed to send response: {e:#}");
                    }
                });
            }
        });
    }
    Ok(())
}

fn handle(req: Request, store: &Mutex<Store>) -> Response {
    let now = chrono::Utc::now().timestamp();
    match req {
        Request::Announce(sa) => {
            let node = sa.signed_by.clone();
            match store.lock().unwrap().insert(sa, now) {
                Ok(()) => {
                    info!("announce: {node}");
                    Response::Ack
                }
                Err(message) => {
                    warn!("announce rejected for {node}: {message}");
                    Response::Error { message }
                }
            }
        }
        Request::Providers => Response::Providers(store.lock().unwrap().fresh(now)),
        _ => Response::Error {
            message: "directory nodes only serve Announce and Providers".to_string(),
        },
    }
}
