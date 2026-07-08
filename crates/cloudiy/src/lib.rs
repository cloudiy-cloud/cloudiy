//! Library surface of the `cloudiy` node, exposing the self-contained,
//! dependency-light pieces that are useful to integration tests and examples
//! (see `examples/permissionless_release.rs`). The binary (`main.rs`) keeps its
//! own module tree; this lib re-exposes only what external harnesses need.

pub mod payments;
pub mod solana;
