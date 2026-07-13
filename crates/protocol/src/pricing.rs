//! Protocol-posted pricing (RFC-0007) — pure types and pure math, zero
//! infrastructure, like everything in this crate.
//!
//! The network, not the provider, posts a **stable, cost-plus, per-request**
//! price for each model endpoint:
//!
//! ```text
//! posted(model) = gpu_class_cost(model.class) / 1h × compute_ms_per_cu × margin
//! ```
//!
//! - `gpu_class_cost` is the **market axis**: a small governance table
//!   (later: oracle median) of USDC cost per hour for each GPU class.
//! - `compute_ms_per_cu` is the **physical axis**: how many milliseconds of
//!   that class one compute unit (CU) takes — a measured property of the
//!   model, benchmarked once per version (RFC-0007 §3.2).
//! - `margin` is the **policy axis**: one transparent network-wide
//!   multiplier in basis points (RFC-0007 §3: `1 + overhead + target_profit`).
//!
//! A model is priced against its **minimum viable class** — the consumer
//! class (RTX 4090 tier) when it runs there, a datacenter class when it
//! physically cannot (video generation). The price stays uniform per model
//! regardless of which provider serves it: better hardware than the pricing
//! class is the provider's margin lever, never the consumer's price.
//!
//! Providers do not price calls. They participate at the posted price and
//! compete on reputation, latency and uptime (RFC-0007 §4-5).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The compute unit (CU) a model class is metered by — chosen so the count
/// is **verifiable by the consumer at the edge** (RFC-0007 §3.1, §8): tokens
/// are countable from the consumer's own input + output, audio duration is
/// known from the consumer's own file, and fixed-shape jobs are one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CuKind {
    /// One bounded request (one image, one clip, one bounded transcription).
    PerRequest,
    /// 1k tokens, input + output (chat / language).
    Per1kTokens,
    /// One second of audio in or out.
    PerAudioSecond,
}

/// The physical axis for one model: what one CU costs in compute, measured
/// on the model's pricing class (RFC-0007 §3.2 benchmark process).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCompute {
    /// GPU class this model is priced against (its minimum viable class).
    pub class: String,
    /// Milliseconds of `class` compute per CU — median of N runs of the
    /// canonical benchmark harness, versions pinned.
    pub compute_ms_per_cu: u64,
    /// How the CU is counted for this model.
    pub cu: CuKind,
    /// `false` = provisional governance estimate awaiting the canonical
    /// benchmark. Kept explicit so nothing provisional can masquerade as
    /// measured (the same honesty rule as RFC-0006).
    pub benchmarked: bool,
}

/// The full posted-price state (RFC-0007 §3). Deliberately tiny and
/// auditable: ~a handful of class costs, one margin scalar, and one
/// `ModelCompute` per model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTable {
    /// Default pricing class for models that run on consumer hardware.
    pub reference_class: String,
    /// Market axis: USDC cost per hour of each GPU class, in micro-USDC.
    /// Governance-maintained now; an oracle median later (same shape).
    pub gpu_class_cost_micro_usdc_per_hour: BTreeMap<String, u64>,
    /// Policy axis: the network-wide margin in basis points
    /// (14_000 = 1.4x). Governance's last lever.
    pub margin_bps: u32,
    /// Physical axis: per-model measured compute.
    pub models: BTreeMap<String, ModelCompute>,
}

const MS_PER_HOUR: u128 = 3_600_000;
const BPS: u128 = 10_000;

impl PricingTable {
    /// The posted price for one CU of `model_key`, in micro-USDC, rounded to
    /// the nearest micro. `None` when the model or its class is not in the
    /// table (an unlisted model has no posted price — it cannot be quoted).
    pub fn posted_price_micro_usdc(&self, model_key: &str) -> Option<u64> {
        let m = self.models.get(model_key)?;
        let cost = *self.gpu_class_cost_micro_usdc_per_hour.get(&m.class)?;
        let num = cost as u128 * m.compute_ms_per_cu as u128 * self.margin_bps as u128;
        let den = MS_PER_HOUR * BPS;
        Some(((num + den / 2) / den) as u64)
    }

    /// Posted price in USDC (display convenience).
    pub fn posted_price_usdc(&self, model_key: &str) -> Option<f64> {
        self.posted_price_micro_usdc(model_key)
            .map(|m| m as f64 / 1_000_000.0)
    }

    /// The devnet governance table (RFC-0007 phase 1).
    ///
    /// Class costs are governance-set plausible spot medians; `compute_ms_per_cu`
    /// values are **provisional** (`benchmarked: false`), calibrated so launch
    /// posted prices match the previously published catalog until the canonical
    /// benchmark harness replaces them. Revisions to any number here are a
    /// deliberate, announced governance action (RFC-0007 §3: "stable").
    pub fn devnet_v1() -> Self {
        let mut gpu = BTreeMap::new();
        gpu.insert("cpu-16c".into(), 80_000); // 0.08 USDC/h
        gpu.insert("rtx-4090".into(), 350_000); // 0.35 USDC/h
        gpu.insert("a100-80g".into(), 1_200_000); // 1.20 USDC/h
        gpu.insert("h100".into(), 2_500_000); // 2.50 USDC/h

        let mut models = BTreeMap::new();
        let mut m = |key: &str, class: &str, ms: u64, cu: CuKind| {
            models.insert(
                key.to_string(),
                ModelCompute {
                    class: class.into(),
                    compute_ms_per_cu: ms,
                    cu,
                    benchmarked: false,
                },
            );
        };
        use CuKind::*;
        // video — physically datacenter-class; priced against h100
        m("hailuo-fast", "h100", 185_143, PerRequest);
        m("veo-fast", "h100", 329_143, PerRequest);
        m("hailuo-std", "h100", 246_857, PerRequest);
        m("p-video", "h100", 154_286, PerRequest);
        m("vidu-t2v", "h100", 195_429, PerRequest);
        m("vidu-i2v", "h100", 216_000, PerRequest);
        m("kling", "h100", 288_000, PerRequest);
        // image — priced against a100-80g
        m("nano-banana", "a100-80g", 85_714, PerRequest);
        m("z-image", "a100-80g", 25_714, PerRequest);
        m("qwen-edit", "a100-80g", 64_286, PerRequest);
        m("flux2", "a100-80g", 107_143, PerRequest);
        // audio — stable-audio needs a GPU; whisper/piper serve on CPU today
        m("stable-audio", "a100-80g", 128_571, PerRequest);
        m("whisper-ep", "cpu-16c", 192_857, PerRequest);
        m("chatterbox", "cpu-16c", 642_857, PerRequest);
        // language — CPU Ollama nodes today, metered per 1k tokens
        m("llama-ep", "cpu-16c", 128_571, Per1kTokens);

        PricingTable {
            reference_class: "rtx-4090".into(),
            gpu_class_cost_micro_usdc_per_hour: gpu,
            margin_bps: 14_000, // 1.4x = 1 + ~0.2 overhead + ~0.2 target profit
            models,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example from RFC-0007 §3.4: 1.20 USDC/h reference cost,
    /// 0.9 GPU-seconds per 1k tokens, margin 1.4 -> 420 micro-USDC / 1k.
    #[test]
    fn rfc_worked_example() {
        let mut gpu = BTreeMap::new();
        gpu.insert("ref".to_string(), 1_200_000u64);
        let mut models = BTreeMap::new();
        models.insert(
            "chat".to_string(),
            ModelCompute {
                class: "ref".into(),
                compute_ms_per_cu: 900,
                cu: CuKind::Per1kTokens,
                benchmarked: true,
            },
        );
        let t = PricingTable {
            reference_class: "ref".into(),
            gpu_class_cost_micro_usdc_per_hour: gpu,
            margin_bps: 14_000,
            models,
        };
        assert_eq!(t.posted_price_micro_usdc("chat"), Some(420));
    }

    /// Devnet launch prices reproduce the published catalog exactly.
    #[test]
    fn devnet_v1_matches_published_catalog() {
        let t = PricingTable::devnet_v1();
        let expect = [
            ("hailuo-fast", 180_000),
            ("veo-fast", 320_000),
            ("hailuo-std", 240_000),
            ("p-video", 150_000),
            ("vidu-t2v", 190_000),
            ("vidu-i2v", 210_000),
            ("kling", 280_000),
            ("nano-banana", 40_000),
            ("z-image", 12_000),
            ("qwen-edit", 30_000),
            ("flux2", 50_000),
            ("stable-audio", 60_000),
            ("whisper-ep", 6_000),
            ("chatterbox", 20_000),
            ("llama-ep", 4_000),
        ];
        for (key, micro) in expect {
            assert_eq!(
                t.posted_price_micro_usdc(key),
                Some(micro),
                "posted price mismatch for {key}"
            );
        }
    }

    #[test]
    fn unlisted_model_has_no_posted_price() {
        let t = PricingTable::devnet_v1();
        assert_eq!(t.posted_price_micro_usdc("not-a-model"), None);
    }

    /// Margin is applied: zeroing it out prices at raw cost.
    #[test]
    fn margin_scales_the_price() {
        let mut t = PricingTable::devnet_v1();
        let with_margin = t.posted_price_micro_usdc("llama-ep").unwrap();
        t.margin_bps = 10_000; // 1.0x
        let at_cost = t.posted_price_micro_usdc("llama-ep").unwrap();
        assert!(with_margin > at_cost);
        // 1.4x within a micro of rounding
        assert!((with_margin as i64 - (at_cost as f64 * 1.4).round() as i64).abs() <= 1);
    }

    /// The devnet table is explicit about being provisional (RFC honesty rule).
    #[test]
    fn devnet_v1_is_marked_unbenchmarked() {
        let t = PricingTable::devnet_v1();
        assert!(t.models.values().all(|m| !m.benchmarked));
    }

    #[test]
    fn table_roundtrips_as_json() {
        let t = PricingTable::devnet_v1();
        let json = serde_json::to_string(&t).unwrap();
        let back: PricingTable = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.posted_price_micro_usdc("flux2"),
            t.posted_price_micro_usdc("flux2")
        );
    }
}
