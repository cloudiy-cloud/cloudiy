use crate::{Capability, Identity, Resources};
use serde::{Deserialize, Serialize};

/// Node health as reported by heartbeats. Nodes are heterogeneous by design
/// — GPU nodes, compute nodes, storage nodes, hybrids, future accelerators —
/// so an announcement carries whatever resources/capabilities the node has,
/// never an assumed shape.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Healthy,
    Degraded,
    Unhealthy,
}

/// What a provider's daemon announces to the network: identity, the
/// *chosen* resource slice (what is not shared stays private), functionality
/// and commercial terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAnnouncement {
    pub identity: Identity,
    pub resources: Resources,
    pub capabilities: Vec<Capability>,
    /// Coarse region tag for latency-aware placement (`sa-east`, `eu-west`…).
    #[serde(default)]
    pub region: Option<String>,
    pub price_micro_usdc_per_hour: u64,
    /// 0.0–1.0, from the (future on-chain) reputation module.
    #[serde(default)]
    pub reputation: f64,
    /// 0.0–1.0 current utilization, from heartbeats.
    #[serde(default)]
    pub utilization: f64,
    pub health: Health,
}
