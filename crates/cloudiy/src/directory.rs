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
    /// Authoritative, canary-derived reputation the directory aggregates and
    /// serves (RFC-0006 §6). Persisted so a restart keeps earned trust; a
    /// prober (RFC-0006 §5.1) folds canary verdicts in. Consumers override a
    /// provider's self-reported reputation with this.
    reputation: crate::reputation::Registry,
    /// Where `reputation` is persisted (set when serving; empty in tests).
    /// Written through by the canary prober.
    rep_path: std::path::PathBuf,
}

/// File the directory persists its reputation registry to
/// (`CLOUDIY_REPUTATION_PATH` overrides).
fn reputation_path() -> std::path::PathBuf {
    std::env::var("CLOUDIY_REPUTATION_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| cloudiy_common::config_dir().join("directory-reputation.json"))
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

pub async fn serve(endpoint: iroh::Endpoint, secret: iroh::SecretKey) -> Result<()> {
    let secret = Arc::new(secret);
    let rep_path = reputation_path();
    let reputation = crate::reputation::Registry::load(&rep_path);
    info!(
        "directory reputation: {} providers scored (from {})",
        reputation.scores().len(),
        rep_path.display()
    );
    let store = Arc::new(Mutex::new(Store {
        by_node: HashMap::new(),
        reputation,
        rep_path,
    }));

    // Background canary prober (RFC-0006 §5.1 → §6): periodically probe fresh
    // providers on the models we have canaries for, fold the verdicts into the
    // reputation registry, and persist. The directory's *own* probes are the
    // trust source (no external verdict submission to spoof). Providers that
    // gate on payment need CLOUDIY_CANARY_TOKEN; unreachable / can't-pay probes
    // are skipped, never penalized (see canary::probe_remote).
    spawn_prober(endpoint.clone(), store.clone());

    while let Some(incoming) = endpoint.accept().await {
        let store = store.clone();
        let secret = secret.clone();
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
                let secret = secret.clone();
                tokio::spawn(async move {
                    let resp = match proto::read_msg::<Request>(&mut recv).await {
                        Ok(req) => handle(req, &store, &secret),
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

/// Spawn the periodic canary prober. Snapshots fresh providers under the lock,
/// probes them with the lock released (network I/O), then briefly re-locks to
/// record verdicts and persist.
fn spawn_prober(endpoint: iroh::Endpoint, store: Arc<Mutex<Store>>) {
    let period = std::env::var("CLOUDIY_CANARY_PERIOD_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(600)
        .max(30);
    let token = std::env::var("CLOUDIY_CANARY_TOKEN").ok();
    // Cap provider-probes per cycle so a large fleet can't blow up prober cost
    // (RFC-0006 §10). Funding paid canaries is separate (operator config).
    let max_runs = std::env::var("CLOUDIY_CANARY_MAX_RUNS_PER_CYCLE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50)
        .max(1);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(period)).await;
            let mut budget = crate::canary::CanaryBudget::new(max_runs);
            // Snapshot (provider_id, served_models) for fresh, canary-covered
            // providers, then drop the lock before any network I/O.
            let targets: Vec<(String, Vec<String>)> = {
                let now = chrono::Utc::now().timestamp();
                let mut s = store.lock().unwrap();
                s.fresh(now)
                    .into_iter()
                    .filter_map(|sa| cloudiy_common::verify_announcement(&sa, now).ok())
                    .map(|ann| {
                        let models = ann
                            .served_models
                            .into_iter()
                            .filter(|m| crate::canary::default_bank().iter().any(|c| &c.model == m))
                            .collect::<Vec<_>>();
                        (ann.identity.as_str().to_string(), models)
                    })
                    .filter(|(_, m)| !m.is_empty())
                    .collect()
            };
            'cycle: for (provider, models) in targets {
                for model in models {
                    // Stop this cycle once the probe budget is spent.
                    if !budget.try_spend() {
                        info!("canary budget spent ({} probes) — pausing until next cycle", budget.spent());
                        break 'cycle;
                    }
                    match crate::canary::probe_remote(&endpoint, &provider, &model, token.as_deref())
                        .await
                    {
                        Ok(probe) if !probe.items.is_empty() => {
                            let rep = {
                                let mut s = store.lock().unwrap();
                                let rep = s.reputation.record_probe(&provider, &probe);
                                let path = s.rep_path.clone();
                                if let Err(e) = s.reputation.save(&path) {
                                    warn!("failed to persist reputation: {e}");
                                }
                                rep
                            };
                            info!(
                                "canary {}/{}: {} → score {:.2} ({})",
                                probe.passed(),
                                probe.total(),
                                &provider[..provider.len().min(8)],
                                rep.score,
                                rep.tier().label()
                            );
                        }
                        _ => {} // unreachable / can't-pay / no canaries → skip
                    }
                }
            }
        }
    });
}

fn handle(req: Request, store: &Mutex<Store>, secret: &iroh::SecretKey) -> Response {
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
        Request::Reputation => {
            let scores = store.lock().unwrap().reputation.scores();
            match cloudiy_common::sign_reputation(secret, &scores, now) {
                Ok(sr) => Response::Reputation(sr),
                Err(e) => Response::Error {
                    message: format!("failed to sign reputation: {e}"),
                },
            }
        }
        _ => Response::Error {
            message: "directory nodes only serve Announce, Providers and Reputation".to_string(),
        },
    }
}
