//! # Cloudiy Open Compute Protocol — domain layer
//!
//! Pure types, zero infrastructure. This crate is the protocol's vocabulary:
//! [`Identity`], [`ResourceKind`]/[`ResourceVector`]/[`Resources`],
//! [`Capability`], [`Workload`] and provider announcements.
//!
//! Design axioms (see PROTOCOL.md):
//! - everything is a resource; resource kinds are an open set
//! - everything is an identity; the protocol never asks who is behind it
//! - consumers describe WHAT (workloads + capabilities), never HOW
//! - settlement is an interface ([`settlement`]), not an implementation

pub mod capability;
pub mod identity;
pub mod provider;
pub mod resource;
pub mod settlement;
pub mod workload;

pub use capability::Capability;
pub use identity::Identity;
pub use provider::{Health, ProviderAnnouncement};
pub use resource::{ResourceError, ResourceKind, ResourceVector, Resources};
pub use workload::{Placement, RestartPolicy, Workload, WorkloadSpec, WorkloadState};

/// Protocol schema version. Breaking changes bump this; implementations
/// negotiate (mirrors the wire ALPN `cloudiy/0`).
pub const PROTOCOL_VERSION: &str = "compute/0";
