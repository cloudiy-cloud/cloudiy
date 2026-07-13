//! # Cloudiy scheduler — the kernel of the compute network
//!
//! It does not schedule CPUs; it schedules **workloads** onto heterogeneous
//! nodes. Policy is a modular pipeline — [`Filter`]s remove nodes that
//! *cannot* run the workload (hard constraints), weighted [`Scorer`]s rank
//! the survivors (preferences). Nothing is hardcoded: compose your own
//! pipeline or start from [`Pipeline::default_policy`].

use cloudiy_protocol::{
    capability::satisfies_all, Health, Placement, ProviderAnnouncement, WorkloadSpec,
};

/// Hard constraint: can this node run the workload at all?
pub trait Filter: Send + Sync {
    fn name(&self) -> &'static str;
    fn admit(&self, spec: &WorkloadSpec, node: &ProviderAnnouncement) -> bool;
}

/// Preference: how good is this node for the workload? Returns 0.0–1.0.
pub trait Scorer: Send + Sync {
    fn name(&self) -> &'static str;
    fn score(&self, spec: &WorkloadSpec, node: &ProviderAnnouncement) -> f64;
}

/// The scheduling seam other components depend on (trait, not a struct —
/// swap the whole strategy without touching callers).
pub trait Scheduler: Send + Sync {
    fn place(&self, spec: &WorkloadSpec, nodes: &[ProviderAnnouncement]) -> Option<Placement>;
}

// ---------------------------------------------------------------- filters

/// Node must have every requested resource kind available in quantity.
pub struct ResourceFit;
impl Filter for ResourceFit {
    fn name(&self) -> &'static str {
        "resource_fit"
    }
    fn admit(&self, spec: &WorkloadSpec, node: &ProviderAnnouncement) -> bool {
        node.resources.available().covers(&spec.resources)
    }
}

/// Node must satisfy every required capability (versioned matching).
pub struct CapabilityMatch;
impl Filter for CapabilityMatch {
    fn name(&self) -> &'static str {
        "capability_match"
    }
    fn admit(&self, spec: &WorkloadSpec, node: &ProviderAnnouncement) -> bool {
        satisfies_all(&node.capabilities, &spec.capabilities)
    }
}

/// Unhealthy nodes never receive work.
pub struct HealthyOnly;
impl Filter for HealthyOnly {
    fn name(&self) -> &'static str {
        "healthy_only"
    }
    fn admit(&self, _spec: &WorkloadSpec, node: &ProviderAnnouncement) -> bool {
        node.health != Health::Unhealthy
    }
}

/// Respect the consumer's price ceiling (0 = no ceiling). This guards the
/// **hourly VM lease** rate — the one genuinely hourly product. Posted-priced
/// model endpoints (RFC-0007) enforce the consumer's ceiling at quote time
/// against the posted price instead (the announced hourly rate does not
/// govern them).
pub struct PriceCeiling;
impl Filter for PriceCeiling {
    fn name(&self) -> &'static str {
        "price_ceiling"
    }
    fn admit(&self, spec: &WorkloadSpec, node: &ProviderAnnouncement) -> bool {
        spec.max_price_micro_usdc == 0
            || node.price_micro_usdc_per_hour <= spec.max_price_micro_usdc
    }
}

// ---------------------------------------------------------------- scorers

/// Cheaper is better (normalized against the ceiling or a soft reference).
///
/// **Opt-in only** since RFC-0007: with a protocol-posted, uniform price per
/// model, price competition between providers does not exist on the
/// marketplace path, so this scorer is deliberately absent from
/// [`Pipeline::default_policy`] (a lower announced rate must never win jobs
/// — that was the underprice-to-cheat vector). It remains available for
/// hourly VM rental policies where the hourly rate is the real price.
pub struct CheapestPrice;
impl Scorer for CheapestPrice {
    fn name(&self) -> &'static str {
        "cheapest_price"
    }
    fn score(&self, spec: &WorkloadSpec, node: &ProviderAnnouncement) -> f64 {
        let reference = if spec.max_price_micro_usdc > 0 {
            spec.max_price_micro_usdc as f64
        } else {
            // soft reference so the scorer stays meaningful without a ceiling
            10_000_000.0
        };
        (1.0 - (node.price_micro_usdc_per_hour as f64 / reference)).clamp(0.0, 1.0)
    }
}

/// Prefer well-reputed providers (future on-chain reputation feeds this).
pub struct HighReputation;
impl Scorer for HighReputation {
    fn name(&self) -> &'static str {
        "high_reputation"
    }
    fn score(&self, _spec: &WorkloadSpec, node: &ProviderAnnouncement) -> f64 {
        node.reputation.clamp(0.0, 1.0)
    }
}

/// Prefer idle nodes (spread load, protect latency).
pub struct LowUtilization;
impl Scorer for LowUtilization {
    fn name(&self) -> &'static str {
        "low_utilization"
    }
    fn score(&self, _spec: &WorkloadSpec, node: &ProviderAnnouncement) -> f64 {
        (1.0 - node.utilization).clamp(0.0, 1.0)
    }
}

/// Degraded health is admitted but penalized.
pub struct HealthBonus;
impl Scorer for HealthBonus {
    fn name(&self) -> &'static str {
        "health_bonus"
    }
    fn score(&self, _spec: &WorkloadSpec, node: &ProviderAnnouncement) -> f64 {
        match node.health {
            Health::Healthy => 1.0,
            Health::Degraded => 0.3,
            Health::Unhealthy => 0.0,
        }
    }
}

// --------------------------------------------------------------- pipeline

pub struct Pipeline {
    filters: Vec<Box<dyn Filter>>,
    scorers: Vec<(Box<dyn Scorer>, f64)>,
}

impl Pipeline {
    pub fn new() -> Self {
        Pipeline {
            filters: Vec::new(),
            scorers: Vec::new(),
        }
    }

    pub fn filter(mut self, f: impl Filter + 'static) -> Self {
        self.filters.push(Box::new(f));
        self
    }

    pub fn scorer(mut self, s: impl Scorer + 'static, weight: f64) -> Self {
        self.scorers.push((Box::new(s), weight));
        self
    }

    /// A sensible starting policy — callers are expected to compose their own.
    ///
    /// RFC-0007 §4: the price is protocol-posted and uniform per model, so the
    /// default carries **no price scorer** — providers compete on reputation,
    /// availability and reliability, never on undercutting. `PriceCeiling`
    /// stays as the consumer's hard protection for hourly-priced work;
    /// [`CheapestPrice`] remains in the crate for explicit hourly-rental
    /// policies.
    pub fn default_policy() -> Self {
        Pipeline::new()
            .filter(HealthyOnly)
            .filter(ResourceFit)
            .filter(CapabilityMatch)
            .filter(PriceCeiling)
            .scorer(HighReputation, 0.5)
            .scorer(LowUtilization, 0.3)
            .scorer(HealthBonus, 0.2)
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::default_policy()
    }
}

impl Pipeline {
    /// All admissible placements, best score first. Consumers use the top-N for
    /// redundant/quorum execution (`cloudiy run --replicas N`); `place` is just
    /// the first of these.
    pub fn rank(&self, spec: &WorkloadSpec, nodes: &[ProviderAnnouncement]) -> Vec<Placement> {
        let weight_sum: f64 = self.scorers.iter().map(|(_, w)| w).sum();
        let mut placements: Vec<Placement> = nodes
            .iter()
            .filter(|node| self.filters.iter().all(|f| f.admit(spec, node)))
            .map(|node| {
                let score = if weight_sum > 0.0 {
                    self.scorers
                        .iter()
                        // A NaN score sorts as the maximum under `total_cmp`
                        // and would silently win every placement; treat any
                        // non-finite scorer output as 0 so a bad/hostile
                        // signal can never float a node to the top.
                        .map(|(s, w)| {
                            let v = s.score(spec, node);
                            (if v.is_finite() { v } else { 0.0 }) * w
                        })
                        .sum::<f64>()
                        / weight_sum
                } else {
                    0.0
                };
                Placement {
                    node: node.identity.clone(),
                    price_micro_usdc_per_hour: node.price_micro_usdc_per_hour,
                    score,
                }
            })
            .collect();
        placements.sort_by(|a, b| b.score.total_cmp(&a.score));
        placements
    }
}

impl Scheduler for Pipeline {
    fn place(&self, spec: &WorkloadSpec, nodes: &[ProviderAnnouncement]) -> Option<Placement> {
        self.rank(spec, nodes).into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudiy_protocol::{Capability, Identity, ResourceKind::*, ResourceVector, Resources};

    fn node(
        id: &str,
        res: &[(cloudiy_protocol::ResourceKind, u64)],
        caps: &[&str],
        price: u64,
        reputation: f64,
        utilization: f64,
        health: Health,
    ) -> ProviderAnnouncement {
        let vector = res
            .iter()
            .cloned()
            .fold(ResourceVector::new(), |v, (k, a)| v.with(k, a));
        ProviderAnnouncement {
            identity: Identity::from(id),
            resources: Resources::declare(vector.clone(), vector).unwrap(),
            capabilities: caps.iter().map(|c| Capability::from(*c)).collect(),
            region: None,
            price_micro_usdc_per_hour: price,
            reputation,
            utilization,
            health,
            served_models: vec![],
            warm_models: vec![],
        }
    }

    /// Heterogeneous network: GPU node, CPU node, storage node — the
    /// scheduler must understand that nodes are not identical.
    fn heterogeneous_network() -> Vec<ProviderAnnouncement> {
        vec![
            node(
                "gpu-node",
                &[(Cpu, 16_000), (Memory, 65_536), (Gpu, 2), (Vram, 49_152)],
                &["docker", "linux", "cuda:12.8", "pytorch"],
                2_000_000,
                0.9,
                0.2,
                Health::Healthy,
            ),
            node(
                "cpu-node",
                &[(Cpu, 64_000), (Memory, 262_144)],
                &["docker", "linux", "ffmpeg"],
                500_000,
                0.8,
                0.1,
                Health::Healthy,
            ),
            node(
                "storage-node",
                &[(Cpu, 4_000), (Storage, 8_388_608)],
                &["docker", "linux"],
                100_000,
                0.7,
                0.05,
                Health::Healthy,
            ),
        ]
    }

    #[test]
    fn cuda_workload_lands_on_the_gpu_node() {
        let spec = WorkloadSpec {
            resources: ResourceVector::new().with(Vram, 24_576),
            capabilities: vec!["cuda:12.8".into(), "pytorch".into()],
            ..Default::default()
        };
        let placement = Pipeline::default_policy()
            .place(&spec, &heterogeneous_network())
            .expect("gpu node should be eligible");
        assert_eq!(placement.node, Identity::from("gpu-node"));
    }

    #[test]
    fn no_node_satisfies_impossible_requirements() {
        let spec = WorkloadSpec {
            capabilities: vec!["comfyui".into()],
            ..Default::default()
        };
        assert!(Pipeline::default_policy()
            .place(&spec, &heterogeneous_network())
            .is_none());
    }

    #[test]
    fn price_ceiling_is_a_hard_constraint() {
        let spec = WorkloadSpec {
            capabilities: vec!["cuda".into()],
            max_price_micro_usdc: 1_000_000, // gpu-node charges 2_000_000
            ..Default::default()
        };
        assert!(Pipeline::default_policy()
            .place(&spec, &heterogeneous_network())
            .is_none());
    }

    #[test]
    fn unhealthy_nodes_never_receive_work() {
        let mut nodes = heterogeneous_network();
        for n in &mut nodes {
            n.health = Health::Unhealthy;
        }
        let spec = WorkloadSpec::default();
        assert!(Pipeline::default_policy().place(&spec, &nodes).is_none());
    }

    #[test]
    fn reputation_breaks_ties_between_equivalent_nodes() {
        let a = node(
            "low-rep",
            &[(Cpu, 8_000)],
            &["docker"],
            1_000,
            0.2,
            0.5,
            Health::Healthy,
        );
        let b = node(
            "high-rep",
            &[(Cpu, 8_000)],
            &["docker"],
            1_000,
            0.95,
            0.5,
            Health::Healthy,
        );
        let spec = WorkloadSpec {
            resources: ResourceVector::new().with(Cpu, 4_000),
            capabilities: vec!["docker".into()],
            ..Default::default()
        };
        let placement = Pipeline::default_policy().place(&spec, &[a, b]).unwrap();
        assert_eq!(placement.node, Identity::from("high-rep"));
    }

    /// RFC-0007: with posted pricing, a lower announced rate must never win
    /// placement — the default policy ranks by reputation/availability only.
    /// (Pre-RFC-0007 this exact pair inverted: the cheap node's price score
    /// outweighed its weak reputation.)
    #[test]
    fn default_policy_never_rewards_undercutting() {
        let cheap_low_rep = node(
            "cheap-low-rep",
            &[(Cpu, 8_000)],
            &["docker"],
            50_000, // 10x cheaper announced rate
            0.3,
            0.5,
            Health::Healthy,
        );
        let fair_high_rep = node(
            "fair-high-rep",
            &[(Cpu, 8_000)],
            &["docker"],
            500_000,
            0.9,
            0.5,
            Health::Healthy,
        );
        let spec = WorkloadSpec {
            resources: ResourceVector::new().with(Cpu, 4_000),
            capabilities: vec!["docker".into()],
            ..Default::default()
        };
        let placement = Pipeline::default_policy()
            .place(&spec, &[cheap_low_rep, fair_high_rep])
            .unwrap();
        assert_eq!(placement.node, Identity::from("fair-high-rep"));
    }

    #[test]
    fn policy_is_composable_not_hardcoded() {
        // A price-only policy, no reputation involved.
        let pipeline = Pipeline::new()
            .filter(ResourceFit)
            .scorer(CheapestPrice, 1.0);
        let spec = WorkloadSpec::default();
        let placement = pipeline.place(&spec, &heterogeneous_network()).unwrap();
        assert_eq!(placement.node, Identity::from("storage-node")); // cheapest
    }

    #[test]
    fn rank_returns_admissible_nodes_best_first() {
        // Three docker-capable nodes; rank must return all admissible, sorted
        // by score descending, and `place` must equal the top of the ranking
        // (this is what `run --replicas N` fans out to).
        let spec = WorkloadSpec {
            resources: ResourceVector::new().with(Cpu, 2_000),
            capabilities: vec!["docker".into()],
            ..Default::default()
        };
        let nodes = heterogeneous_network();
        let ranked = Pipeline::default_policy().rank(&spec, &nodes);
        assert_eq!(ranked.len(), 3, "all docker nodes are admissible");
        for pair in ranked.windows(2) {
            assert!(pair[0].score >= pair[1].score, "scores must be descending");
        }
        let best = Pipeline::default_policy().place(&spec, &nodes).unwrap();
        assert_eq!(ranked[0].node, best.node, "place == top of rank");
    }

    #[test]
    fn rank_excludes_ineligible_nodes() {
        // A cuda requirement only the gpu-node satisfies → rank has exactly one.
        let spec = WorkloadSpec {
            capabilities: vec!["cuda:12.8".into()],
            ..Default::default()
        };
        let ranked = Pipeline::default_policy().rank(&spec, &heterogeneous_network());
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].node, Identity::from("gpu-node"));
    }
}
