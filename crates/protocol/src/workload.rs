use crate::{Capability, Identity, ResourceVector};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What a consumer declares. Only the WHAT — the protocol decides the HOW.
///
/// Docker, VMs, GPU drivers, CUDA installs, networking, SSH and Kubernetes
/// must never appear here: they are runtime implementation details.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkloadSpec {
    /// OCI image reference (`ubuntu:24.04`, `pytorch/pytorch:2.4`), if the
    /// consumer picked one directly…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// …or a template/recipe name (`pytorch`, `ollama`, `comfyui`) that the
    /// network expands into an environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Command to execute (argv form; empty = image default).
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Requested quantities: cpu (millicores), memory/vram/storage (MiB), gpu (count)…
    #[serde(default)]
    pub resources: ResourceVector,
    /// Required functionality: `cuda:12.8`, `pytorch`, `linux`, `arm64`…
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Ports the workload exposes.
    #[serde(default)]
    pub ports: Vec<u16>,
    /// Persistent storage volume in MiB (0 = ephemeral only).
    #[serde(default)]
    pub persistent_storage_mib: u64,
    /// Hard wall-clock limit in seconds (0 = network default).
    #[serde(default)]
    pub max_duration_secs: u64,
    /// Price ceiling in micro-USDC the consumer accepts (0 = no ceiling).
    #[serde(default)]
    pub max_price_micro_usdc: u64,
    /// Value at stake for this job in micro-USDC — the funded escrow amount (or
    /// a quote). Drives the reputation value-cap at placement (RFC-0006 §6): a
    /// provider may only take jobs within its earned tier. 0 = no value at stake
    /// (free/demo run), so the cap is inert and any provider may serve it.
    #[serde(default)]
    pub job_value_micro_usdc: u64,
    #[serde(default)]
    pub restart: RestartPolicy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFailure,
    Always,
}

/// Scheduler output: a workload bound to a node at a price.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub node: Identity,
    pub price_micro_usdc_per_hour: u64,
    /// Scheduler score, for observability/debugging of decisions.
    pub score: f64,
}

/// Full lifecycle. Terminal states always destroy the environment and
/// release resources — no exceptions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum WorkloadState {
    Declared,
    Scheduled { node: Identity },
    Preparing { node: Identity },
    Running { node: Identity },
    Succeeded,
    Failed { reason: String },
    Cancelled,
}

impl WorkloadState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WorkloadState::Succeeded | WorkloadState::Failed { .. } | WorkloadState::Cancelled
        )
    }
}

/// A workload instance owned by an identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workload {
    pub id: String,
    pub owner: Identity,
    pub spec: WorkloadSpec,
    pub state: WorkloadState,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Workload {
    pub fn declare(owner: Identity, spec: WorkloadSpec) -> Self {
        Workload {
            id: uuid::Uuid::new_v4().to_string(),
            owner,
            spec,
            state: WorkloadState::Declared,
            created_at: chrono::Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourceKind::*;

    #[test]
    fn manifest_example_spec_roundtrips_as_json() {
        // "Ubuntu 24, Python, CUDA, PyTorch, 8 CPU, 32GB RAM, 24GB VRAM,
        //  persistent storage, port 8080, command: python app.py"
        let spec = WorkloadSpec {
            image: Some("ubuntu:24.04".into()),
            command: vec!["python".into(), "app.py".into()],
            resources: ResourceVector::new()
                .with(Cpu, 8_000)
                .with(Memory, 32_768)
                .with(Vram, 24_576),
            capabilities: vec!["python".into(), "cuda".into(), "pytorch".into()],
            ports: vec![8080],
            persistent_storage_mib: 10_240,
            ..Default::default()
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: WorkloadSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back.resources.get(&Vram), 24_576);
        assert_eq!(back.ports, vec![8080]);
    }

    #[test]
    fn terminal_states_are_terminal() {
        assert!(WorkloadState::Succeeded.is_terminal());
        assert!(WorkloadState::Failed {
            reason: "oom".into()
        }
        .is_terminal());
        assert!(WorkloadState::Cancelled.is_terminal());
        assert!(!WorkloadState::Running {
            node: Identity::from("n1")
        }
        .is_terminal());
    }
}
