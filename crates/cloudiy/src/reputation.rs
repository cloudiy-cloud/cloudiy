//! Reputation ramp (RFC-0006 §6) — the stake-free incentive layer.
//!
//! Providers join instantly, free, at **zero trust**, and *earn* trust with a
//! clean canary record. Trust is the collateral (not deposited capital), so
//! honesty is the profit-maximizing strategy for anyone who wants volume, and a
//! caught cheat craters the score back to the bottom — self-eliminating.
//!
//! A provider's tier gates three ramp levers:
//! - **max job value** it may take (a fresh identity can only reach jobs too
//!   small to be worth cheating — Sybil / hit-and-run containment),
//! - **canary sampling rate** (new = heavily audited, veteran = light), and
//! - **earnings holdback** (challenge window before funds release; the
//!   *enforcement* of the clawback is on-chain, RFC-0006 §4.4/§6.2 — this module
//!   only sets the policy durations).
//!
//! This is the pure, testable core. Where the registry lives so it is
//! tamper-resistant without a stake chain (signed log on the directory, or an
//! on-chain reputation account) is RFC-0006 §10; the store here is the shape
//! that layer persists.

// Part of this registry API (per-provider `get`, `record_job`, the `may_take`
// gate) is consumed by the scheduler's job-assignment gating and the paid-job
// path, which land with the rest of the settlement layer (RFC-0006 §6). The
// logic is fully unit-tested now; allow the not-yet-wired surface.
#![allow(dead_code)]

use std::collections::HashMap;

/// Rolling trust for one provider identity (its node id).
#[derive(Clone, Debug)]
pub struct Reputation {
    /// Rolling trust in `0.0..=1.0`. Climbs slowly on clean canaries, craters on
    /// a failure — a single caught cheat must cost more than it earned.
    pub score: f64,
    /// Canaries the provider has passed (history depth gates the tier, so trust
    /// can't be bought instantly with one lucky pass).
    pub canary_pass: u64,
    /// Canaries the provider has failed (ever).
    pub canary_fail: u64,
    /// Clean paid jobs completed.
    pub jobs_ok: u64,
}

impl Default for Reputation {
    fn default() -> Self {
        // A brand-new identity starts at zero trust — bottom of the ramp.
        Reputation {
            score: 0.0,
            canary_pass: 0,
            canary_fail: 0,
            jobs_ok: 0,
        }
    }
}

/// How much trust one clean canary adds (asymptotic climb toward 1.0).
const CLIMB: f64 = 0.05;
/// A failed canary multiplies the score by this — a sharp, punishing drop.
const CRATER: f64 = 0.25;

impl Reputation {
    /// Record a canary verdict. A pass nudges trust up (diminishing as it
    /// approaches 1.0); a fail craters it, sending the provider back down the
    /// ramp regardless of prior standing.
    pub fn record_canary(&mut self, passed: bool) {
        if passed {
            self.canary_pass += 1;
            self.score += (1.0 - self.score) * CLIMB;
        } else {
            self.canary_fail += 1;
            self.score *= CRATER;
        }
    }

    /// Record a clean paid job completion.
    pub fn record_job(&mut self) {
        self.jobs_ok += 1;
    }

    /// The provider's current ramp tier — a function of both trust *and* history
    /// depth, so a fresh identity can never be treated as a veteran.
    pub fn tier(&self) -> Tier {
        match () {
            _ if self.score >= 0.95 && self.canary_pass >= 100 => Tier::Veteran,
            _ if self.score >= 0.80 && self.canary_pass >= 25 => Tier::Trusted,
            _ if self.score >= 0.50 && self.canary_pass >= 5 => Tier::Building,
            _ => Tier::New,
        }
    }

    /// The ramp levers this provider is currently subject to.
    pub fn policy(&self) -> RampPolicy {
        RampPolicy::for_tier(self.tier())
    }
}

/// Trust tiers a provider climbs as it earns a clean record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    New,
    Building,
    Trusted,
    Veteran,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::New => "new",
            Tier::Building => "building",
            Tier::Trusted => "trusted",
            Tier::Veteran => "veteran",
        }
    }
}

/// The concrete ramp levers for a tier. Defaults are illustrative and meant to
/// be tuned per network economics (RFC-0006 §10) — the shape is what matters:
/// higher trust → bigger jobs, lighter audit, faster payout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RampPolicy {
    /// Largest job value (micro-USDC) a provider at this tier may be assigned —
    /// caps what a fresh/low-trust identity can steal in one shot.
    pub max_job_micro_usdc: u64,
    /// Fraction of this provider's jobs that are canaries (`0.0..=1.0`).
    pub canary_rate: f64,
    /// Seconds earnings are held in the challenge window before release.
    pub holdback_secs: i64,
}

impl RampPolicy {
    pub fn for_tier(tier: Tier) -> RampPolicy {
        match tier {
            // Small jobs, 1-in-5 audited, day-long holdback: cheap to vet, and
            // nothing worth a hit-and-run is reachable.
            Tier::New => RampPolicy {
                max_job_micro_usdc: 10_000, // $0.01
                canary_rate: 0.20,
                holdback_secs: 24 * 3600,
            },
            Tier::Building => RampPolicy {
                max_job_micro_usdc: 100_000, // $0.10
                canary_rate: 0.10,
                holdback_secs: 12 * 3600,
            },
            Tier::Trusted => RampPolicy {
                max_job_micro_usdc: 1_000_000, // $1.00
                canary_rate: 0.03,
                holdback_secs: 2 * 3600,
            },
            // Big jobs, lightly audited, fast payout — earned over a long clean
            // record; a single canary fail still craters them back to New.
            Tier::Veteran => RampPolicy {
                max_job_micro_usdc: 10_000_000, // $10.00
                canary_rate: 0.01,
                holdback_secs: 30 * 60,
            },
        }
    }

    /// Whether a provider at this tier may take a job of the given value.
    pub fn may_take(&self, job_micro_usdc: u64) -> bool {
        job_micro_usdc <= self.max_job_micro_usdc
    }
}

/// In-memory reputation registry keyed by provider node id. This is the shape
/// the directory (or an on-chain account, RFC-0006 §10) persists; the logic —
/// how verdicts move trust and gate the ramp — lives here and is unit-tested.
#[derive(Default)]
pub struct Registry {
    providers: HashMap<String, Reputation>,
}

impl Registry {
    pub fn get(&self, node_id: &str) -> Reputation {
        self.providers.get(node_id).cloned().unwrap_or_default()
    }

    /// Apply a canary verdict to a provider, returning its updated reputation.
    pub fn record_canary(&mut self, node_id: &str, passed: bool) -> Reputation {
        let r = self.providers.entry(node_id.to_string()).or_default();
        r.record_canary(passed);
        r.clone()
    }

    /// Fold a whole probe result (each canary item) into a provider's score.
    pub fn record_probe(&mut self, node_id: &str, probe: &crate::canary::ProbeResult) -> Reputation {
        let r = self.providers.entry(node_id.to_string()).or_default();
        for (_, passed, _) in &probe.items {
            r.record_canary(*passed);
        }
        r.clone()
    }

    pub fn record_job(&mut self, node_id: &str) {
        self.providers.entry(node_id.to_string()).or_default().record_job();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_identity_starts_at_the_bottom() {
        let r = Reputation::default();
        assert_eq!(r.tier(), Tier::New);
        // A new provider can only reach tiny jobs — Sybil / hit-and-run cap.
        assert!(r.policy().may_take(10_000));
        assert!(!r.policy().may_take(50_000));
        assert!((r.policy().canary_rate - 0.20).abs() < 1e-9);
    }

    #[test]
    fn clean_record_climbs_the_ramp() {
        let mut r = Reputation::default();
        for _ in 0..120 {
            r.record_canary(true);
        }
        assert_eq!(r.tier(), Tier::Veteran);
        // Veterans reach big jobs, light audit, fast payout.
        assert!(r.policy().may_take(10_000_000));
        assert!(r.policy().canary_rate <= 0.01 + 1e-9);
        assert!(r.policy().holdback_secs < RampPolicy::for_tier(Tier::New).holdback_secs);
    }

    #[test]
    fn one_caught_cheat_craters_a_veteran() {
        let mut r = Reputation::default();
        for _ in 0..120 {
            r.record_canary(true);
        }
        assert_eq!(r.tier(), Tier::Veteran);
        let before = r.score;
        r.record_canary(false); // caught cheating once
        assert!(r.score < before * 0.5); // sharp drop
        assert_ne!(r.tier(), Tier::Veteran); // demoted out of the top tier
    }

    #[test]
    fn tier_needs_sustained_history() {
        // Trust climbs slowly and gates on history depth, so it can't be faked
        // with a handful of passes.
        let mut r = Reputation::default();
        for _ in 0..6 {
            r.record_canary(true);
        }
        // 6 passes: score ~0.26 — still bottom tier, small jobs only.
        assert_eq!(r.tier(), Tier::New);

        for _ in 0..20 {
            r.record_canary(true); // 26 total
        }
        // Climbed to Building, but nowhere near Veteran (which needs 100 passes)
        // no matter how high the score — history depth is a hard gate.
        assert_eq!(r.tier(), Tier::Building);
        assert_ne!(r.tier(), Tier::Veteran);
    }

    #[test]
    fn registry_folds_a_probe_result() {
        let mut reg = Registry::default();
        let mut probe = crate::canary::ProbeResult::default();
        probe.items.push(("a".into(), true, "43".into()));
        probe.items.push(("b".into(), true, "Paris".into()));
        probe.items.push(("c".into(), false, "wrong".into()));
        let r = reg.record_probe("node-xyz", &probe);
        assert_eq!(r.canary_pass, 2);
        assert_eq!(r.canary_fail, 1);
    }
}
