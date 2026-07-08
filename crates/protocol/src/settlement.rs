//! Settlement **interfaces only** — blockchain is an implementation detail,
//! not the product. Solana, USDC, escrow, streaming payments, on-chain
//! reputation and proof-of-compute plug in behind these traits later;
//! nothing in the protocol layer may depend on a concrete chain.

use crate::{Identity, Workload};
use serde::{Deserialize, Serialize};

/// A payment quote in the x402 shape: price, payee, asset, settlement hint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentQuote {
    pub amount_micro_usdc: u64,
    pub pay_to: Identity,
    /// Asset identifier (e.g. an SPL mint). Opaque to the protocol.
    pub asset: String,
    /// Settlement network label (e.g. `solana-devnet`). Opaque to the protocol.
    pub network: String,
    /// Raw requirements document (x402 `accepts`), forward-compatible.
    pub raw: serde_json::Value,
}

/// Proof that a quote was settled. Opaque to the protocol; verified by the
/// settlement module that issued the quote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentProof(pub serde_json::Value);

/// The settlement seam. Future modules: Solana escrow (deployed on devnet),
/// streaming payments, proof-of-compute attestations.
#[async_trait::async_trait]
pub trait Settlement: Send + Sync {
    /// Quote the price for running `workload` on `provider`.
    async fn quote(&self, workload: &Workload, provider: &Identity)
        -> anyhow::Result<PaymentQuote>;

    /// Verify a submitted payment proof against a quote.
    async fn verify(&self, quote: &PaymentQuote, proof: &PaymentProof) -> anyhow::Result<bool>;

    /// Release funds to the provider after successful completion
    /// (escrow `release`) or back to the consumer (`refund`).
    async fn settle(&self, workload: &Workload, proof: &PaymentProof) -> anyhow::Result<()>;
}

/// Reputation seam — future on-chain module; the scheduler only reads scores.
#[async_trait::async_trait]
pub trait Reputation: Send + Sync {
    async fn score(&self, provider: &Identity) -> anyhow::Result<f64>;
}
