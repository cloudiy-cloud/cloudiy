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
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

/// Upper bound on stored announcements (memory cap; ~1 KiB each).
const MAX_ANNOUNCEMENTS: usize = 10_000;

/// Rolling window for the demand oracle: interest pings older than this are
/// dropped, so `recent_interest` reflects live demand (phase 2).
const DEMAND_WINDOW_SECS: i64 = 3600;

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
    /// Demand oracle: per-endpoint-key timestamps of consumer interest pings
    /// within the rolling window. In-memory only (demand is inherently live).
    demand: HashMap<String, std::collections::VecDeque<i64>>,
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

    /// Record one consumer interest ping for `key` and drop stale timestamps.
    fn record_interest(&mut self, key: &str, now: i64) {
        let cutoff = now - DEMAND_WINDOW_SECS;
        let q = self.demand.entry(key.to_string()).or_default();
        q.push_back(now);
        while q.front().is_some_and(|&t| t < cutoff) {
            q.pop_front();
        }
        // Bound per-key memory against a flood (still a live-window count).
        while q.len() > 100_000 {
            q.pop_front();
        }
    }

    /// The demand table: recent interest (windowed) vs current supply per key.
    /// Supply = fresh providers announcing the key in `served_models`.
    fn demand_table(&mut self, now: i64) -> Vec<proto::DemandEntry> {
        let cutoff = now - DEMAND_WINDOW_SECS;
        // Tally current supply per key from fresh, verified announcements.
        let mut supply: HashMap<String, u32> = HashMap::new();
        for sa in self.by_node.values() {
            if let Ok(ann) = cloudiy_common::verify_announcement(sa, now) {
                for m in ann.served_models {
                    *supply.entry(m).or_default() += 1;
                }
            }
        }
        // Union of keys seen in either demand or supply.
        let mut keys: std::collections::HashSet<String> = supply.keys().cloned().collect();
        keys.extend(self.demand.keys().cloned());
        keys.into_iter()
            .map(|key| {
                let recent_interest = self
                    .demand
                    .get(&key)
                    .map(|q| q.iter().filter(|&&t| t >= cutoff).count() as u32)
                    .unwrap_or(0);
                let providers = supply.get(&key).copied().unwrap_or(0);
                proto::DemandEntry {
                    key,
                    recent_interest,
                    providers,
                }
            })
            .collect()
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
        demand: HashMap::new(),
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
                let mut s = store.lock();
                let fresh = s.fresh(now);
                // Score each fresh, canary-covered provider by its current
                // reputation, then probe LOW-reputation first (RFC-0006 §6): the
                // ramp audits new/low-trust providers heavily and veterans
                // lightly, so within the per-cycle budget the least-trusted are
                // always covered and high-trust ones only if budget remains.
                let mut scored: Vec<(f64, String, Vec<String>)> = fresh
                    .into_iter()
                    .filter_map(|sa| cloudiy_common::verify_announcement(&sa, now).ok())
                    .filter_map(|ann| {
                        let id = ann.identity.as_str().to_string();
                        let models: Vec<String> = ann
                            .served_models
                            .into_iter()
                            .filter(|m| crate::canary::default_bank().iter().any(|c| &c.model == m))
                            .collect();
                        if models.is_empty() {
                            return None;
                        }
                        let score = s.reputation.get(&id).score;
                        Some((score, id, models))
                    })
                    .collect();
                scored.sort_by(|a, b| a.0.total_cmp(&b.0));
                scored.into_iter().map(|(_, id, m)| (id, m)).collect()
            };
            'cycle: for (provider, models) in targets {
                for model in models {
                    // Stop this cycle once the probe budget is spent.
                    if !budget.try_spend() {
                        info!(
                            "canary budget spent ({} probes) — pausing until next cycle",
                            budget.spent()
                        );
                        break 'cycle;
                    }
                    match crate::canary::probe_remote(
                        &endpoint,
                        &provider,
                        &model,
                        token.as_deref(),
                    )
                    .await
                    {
                        Ok(probe) if !probe.items.is_empty() => {
                            let rep = {
                                let mut s = store.lock();
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
            match store.lock().insert(sa, now) {
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
        Request::Providers => Response::Providers(store.lock().fresh(now)),
        Request::Reputation => {
            let scores = store.lock().reputation.scores();
            match cloudiy_common::sign_reputation(secret, &scores, now) {
                Ok(sr) => Response::Reputation(sr),
                Err(e) => Response::Error {
                    message: format!("failed to sign reputation: {e}"),
                },
            }
        }
        // Demand oracle (phase 2): record consumer interest, serve the table.
        Request::EndpointInterest { key } => {
            store.lock().record_interest(&key, now);
            Response::Ack
        }
        Request::Demand => Response::Demand(store.lock().demand_table(now)),
        _ => Response::Error {
            message: "directory nodes only serve Announce, Providers, Reputation and Demand"
                .to_string(),
        },
    }
}
