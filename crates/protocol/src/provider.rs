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
    /// Hourly rate for a dedicated VM lease on this node — the one product
    /// that is genuinely billed by the hour. For model endpoints this is
    /// **not** a matching input and not shown to consumers: the network posts
    /// the price (RFC-0007, `crate::pricing::PricingTable`) and providers are
    /// price takers; announcing a model in `served_models` IS the provider's
    /// price signal ("I participate at the posted price"). At most the
    /// pricing layer may read this as an internal cost hint when governance
    /// calibrates the posted table.
    pub price_micro_usdc_per_hour: u64,
    /// 0.0–1.0, from the (future on-chain) reputation module.
    #[serde(default)]
    pub reputation: f64,
    /// 0.0–1.0 current utilization, from heartbeats.
    #[serde(default)]
    pub utilization: f64,
    pub health: Health,
    /// Catalog model endpoints ("Models") this provider can serve. A model
    /// need not be pre-installed: the node pulls the worker image and weights
    /// on demand, so this lists what it is *willing and able* to run given its
    /// hardware (a CPU node lists text only; a GPU node adds image/video).
    #[serde(default)]
    pub served_models: Vec<String>,
    /// Subset of `served_models` currently resident/warm — served with no cold
    /// start. The scheduler prefers a warm provider for lower latency.
    #[serde(default)]
    pub warm_models: Vec<String>,
}
