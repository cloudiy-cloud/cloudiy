use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A kind of computational resource. An **open set** by design: future
/// hardware (TPUs, NPUs, quantum co-processors…) arrives as `Custom` without
/// any architectural change.
///
/// Unit conventions: `Cpu` in millicores (1000 = one core), `Memory`/`Vram`/
/// `Storage` in MiB, `Gpu` in device count, `Bandwidth` in Mbps.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResourceKind {
    Cpu,
    Memory,
    Gpu,
    Vram,
    Storage,
    Bandwidth,
    /// Escape hatch for future hardware — first-class, not second-class.
    Custom(String),
}

impl std::fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResourceKind::Custom(name) => write!(f, "custom:{name}"),
            other => write!(f, "{}", format!("{other:?}").to_lowercase()),
        }
    }
}

/// A quantity per resource kind. Used for requests ("I need 8 CPU, 24GB
/// VRAM"), announcements and accounting. Missing kind = zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceVector(pub BTreeMap<ResourceKind, u64>);

impl ResourceVector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, kind: ResourceKind, amount: u64) -> Self {
        self.0.insert(kind, amount);
        self
    }

    pub fn get(&self, kind: &ResourceKind) -> u64 {
        self.0.get(kind).copied().unwrap_or(0)
    }

    /// True when `self` can satisfy `request` on every kind.
    pub fn covers(&self, request: &ResourceVector) -> bool {
        request.0.iter().all(|(k, need)| self.get(k) >= *need)
    }

    /// Per-kind addition that saturates instead of overflowing — the name
    /// promises "checked", so an oversized sum clamps at `u64::MAX` rather
    /// than panicking (debug) or wrapping (release) for a direct caller.
    pub fn checked_add(&self, other: &ResourceVector) -> ResourceVector {
        let mut out = self.clone();
        for (k, v) in &other.0 {
            let slot = out.0.entry(k.clone()).or_insert(0);
            *slot = slot.saturating_add(*v);
        }
        out
    }

    /// Saturating per-kind subtraction.
    pub fn saturating_sub(&self, other: &ResourceVector) -> ResourceVector {
        let mut out = self.clone();
        for (k, v) in &other.0 {
            let e = out.0.entry(k.clone()).or_insert(0);
            *e = e.saturating_sub(*v);
        }
        out
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResourceError {
    #[error("insufficient {kind}: requested {requested}, available {available}")]
    Insufficient {
        kind: ResourceKind,
        requested: u64,
        available: u64,
    },
    #[error("cannot share more than total for {0}")]
    ShareExceedsTotal(ResourceKind),
}

/// Per-node resource accounting: `total` is what the hardware has, `shared`
/// is the slice the provider **chose** to contribute (the rest stays
/// private), `allocated` is in use by workloads.
/// `available = shared − allocated`, and allocation always round-trips:
/// what a workload takes, its termination returns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    pub total: ResourceVector,
    pub shared: ResourceVector,
    pub allocated: ResourceVector,
}

impl Resources {
    /// Declare hardware totals and the shared slice, validating that no kind
    /// shares more than it has.
    pub fn declare(total: ResourceVector, shared: ResourceVector) -> Result<Self, ResourceError> {
        for (kind, amount) in &shared.0 {
            if *amount > total.get(kind) {
                return Err(ResourceError::ShareExceedsTotal(kind.clone()));
            }
        }
        Ok(Resources {
            total,
            shared,
            allocated: ResourceVector::new(),
        })
    }

    pub fn available(&self) -> ResourceVector {
        self.shared.saturating_sub(&self.allocated)
    }

    /// Reserve resources for a placed workload.
    pub fn allocate(&mut self, request: &ResourceVector) -> Result<(), ResourceError> {
        let available = self.available();
        for (kind, need) in &request.0 {
            let have = available.get(kind);
            if have < *need {
                return Err(ResourceError::Insufficient {
                    kind: kind.clone(),
                    requested: *need,
                    available: have,
                });
            }
        }
        self.allocated = self.allocated.checked_add(request);
        Ok(())
    }

    /// Automatic return on workload completion.
    pub fn release(&mut self, request: &ResourceVector) {
        self.allocated = self.allocated.saturating_sub(request);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ResourceKind::*;

    fn vec_of(pairs: &[(ResourceKind, u64)]) -> ResourceVector {
        pairs
            .iter()
            .cloned()
            .fold(ResourceVector::new(), |v, (k, a)| v.with(k, a))
    }

    #[test]
    fn provider_shares_a_slice_and_keeps_the_rest_private() {
        // Machine: 32 CPU, 128GB RAM, 4 GPUs. Shares: 16 CPU, 64GB, 2 GPUs.
        let total = vec_of(&[(Cpu, 32_000), (Memory, 131_072), (Gpu, 4)]);
        let shared = vec_of(&[(Cpu, 16_000), (Memory, 65_536), (Gpu, 2)]);
        let r = Resources::declare(total, shared).unwrap();
        assert_eq!(r.available().get(&Gpu), 2);
        assert_eq!(r.available().get(&Cpu), 16_000);
    }

    #[test]
    fn sharing_more_than_total_is_rejected() {
        let total = vec_of(&[(Gpu, 2)]);
        let shared = vec_of(&[(Gpu, 4)]);
        assert_eq!(
            Resources::declare(total, shared),
            Err(ResourceError::ShareExceedsTotal(Gpu))
        );
    }

    #[test]
    fn allocate_and_automatic_release_roundtrip() {
        let total = vec_of(&[(Cpu, 8_000), (Vram, 24_576)]);
        let mut r = Resources::declare(total.clone(), total).unwrap();
        let req = vec_of(&[(Cpu, 4_000), (Vram, 24_576)]);

        r.allocate(&req).unwrap();
        assert_eq!(r.available().get(&Vram), 0);
        // Second workload wanting VRAM must fail while the first runs.
        assert!(matches!(
            r.allocate(&vec_of(&[(Vram, 1)])),
            Err(ResourceError::Insufficient { .. })
        ));

        r.release(&req);
        assert_eq!(r.available().get(&Vram), 24_576);
        assert_eq!(r.available().get(&Cpu), 8_000);
    }

    #[test]
    fn future_hardware_needs_no_redesign() {
        let kind = Custom("npu".into());
        let total = ResourceVector::new().with(kind.clone(), 2);
        let r = Resources::declare(total.clone(), total).unwrap();
        assert_eq!(r.available().get(&kind), 2);
        assert_eq!(kind.to_string(), "custom:npu");
    }
}
