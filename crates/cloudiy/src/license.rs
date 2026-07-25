//! License policy for models served over the network — an **executable** rule,
//! not a convention, so the closed/restricted-model problem cannot come back by
//! accident. Every catalog entry (`gateway::model_catalog`) carries a typed
//! [`License`], and a build-failing test
//! (`gateway::tests::every_served_model_has_an_allowed_license`) rejects any
//! entry whose license is not on the allowlist.
//!
//! ## What "served over the network" changes about licenses
//!
//! A Cloudiy provider *serves the model to third parties over a network*. That
//! is exactly the trigger of the licenses banned below — the restriction we care
//! about is not "can I use the weights" but "what happens when I expose them as
//! a remote service to strangers for money".
//!
//! ### AGPL-3.0 is banned (the non-obvious, architecture-specific one)
//!
//! The **AGPL-3.0 network clause** (§13) says: if you *modify* the software and
//! let users interact with it **over a network**, you must offer those users the
//! complete corresponding source of your running version. Serving a model
//! remotely is precisely that interaction. A provider who ran, say, **YOLO from
//! Ultralytics (v5 / v8 / v11 / YOLO26 — all AGPL-3.0)** as a Cloudiy endpoint
//! could be forced to **open-source the operator's own stack** around it. That
//! is an unacceptable, silent legal trap for anyone running `cloudiy share`, so
//! AGPL weights never enter the catalog. Use the permissively-licensed
//! alternative instead (for detection: **RT-DETR**, Apache-2.0).
//!
//! ### Also banned
//!
//! - **CC-BY-NC** (any non-commercial variant) — the network is a paid service,
//!   so non-commercial terms are violated by design (e.g. **NLLB**,
//!   **SeamlessM4T**; and the **Base/Large** variants of Depth Anything V2).
//! - **AI Pubs RAIL-M / other "commercial use requires a paid license"** terms —
//!   e.g. **Surya OCR**: commercial use needs a paid license, which a
//!   permissionless network cannot assume every operator holds.
//! - **Bespoke "Model License" agreements with use restrictions** that are not on
//!   the allowlist — e.g. **DeepSeek-Coder** (code repo is MIT, but the *weights*
//!   are under a separate DeepSeek Model License) and the **Stability AI
//!   Community License** (Stable Audio Open). These may well be fine for a given
//!   operator, but a network-wide default cannot assume so; add the specific
//!   license to the allowlist deliberately if you want it.

/// A license under which a model's **weights** are distributed. Only the
/// [`License::is_allowed`] set may be served by a conforming node; the banned
/// variants exist so the *reason* for each ban is documented in code and the
/// build-time check is auditable rather than a bare string comparison.
///
/// `dead_code` is allowed because this is a **policy vocabulary**: the banned
/// variants (and any allowed license not yet used by a catalog entry, e.g.
/// BSD-2-Clause) are constructed only by the policy tests — they exist to be
/// matched and documented, not necessarily served today.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum License {
    // ---- Allowed: permissive, commercial-friendly, no network clause --------
    Apache2_0,
    Mit,
    Bsd2Clause,
    Bsd3Clause,
    /// CreativeML Open RAIL++-M (SDXL): open weights with use-based restrictions
    /// (no illegal/harmful use), but commercially usable and no network/
    /// copyleft clause — accepted.
    OpenRailPlusPlusM,
    /// Llama Community License (Meta Llama 3.x): permissive **with a note** — an
    /// over-700M-monthly-active-users clause needs a separate license from Meta.
    /// Fine for the network; surfaced so operators at that scale know.
    LlamaCommunity,

    // ---- Banned: representable only so the ban is documented + tested -------
    /// AGPL-3.0 — the network-service copyleft trap (see module docs). Banned.
    Agpl3_0,
    /// Any Creative Commons NonCommercial variant — banned on a paid network.
    CcByNc,
    /// "commercial use requires a paid license" (AI Pubs RAIL-M, etc.) — banned.
    PaidCommercial,
    /// A bespoke model-weights license with restrictions, not individually
    /// allowlisted (DeepSeek Model License, Stability AI Community License, …).
    RestrictedModel,
}

impl License {
    /// Whether a model under this license may be **served over the network** by
    /// a conforming node. This is the allowlist the build-time check enforces.
    pub const fn is_allowed(self) -> bool {
        matches!(
            self,
            License::Apache2_0
                | License::Mit
                | License::Bsd2Clause
                | License::Bsd3Clause
                | License::OpenRailPlusPlusM
                | License::LlamaCommunity
        )
    }

    /// A human-readable SPDX-ish label for logs, `/info` and cards.
    pub const fn label(self) -> &'static str {
        match self {
            License::Apache2_0 => "Apache-2.0",
            License::Mit => "MIT",
            License::Bsd2Clause => "BSD-2-Clause",
            License::Bsd3Clause => "BSD-3-Clause",
            License::OpenRailPlusPlusM => "CreativeML-Open-RAIL++-M",
            License::LlamaCommunity => "Llama-Community",
            License::Agpl3_0 => "AGPL-3.0",
            License::CcByNc => "CC-BY-NC",
            License::PaidCommercial => "Paid-Commercial-License",
            License::RestrictedModel => "Restricted-Model-License",
        }
    }

    /// Parse a [`License::label`] back to a variant — the inverse of `label`,
    /// so a declarative manifest (§18) can carry the license as a string and the
    /// loader can enforce the allowlist on it. Unknown labels return `None`
    /// (rejected by the loader, same as a banned license).
    pub fn from_label(s: &str) -> Option<License> {
        let all = [
            License::Apache2_0,
            License::Mit,
            License::Bsd2Clause,
            License::Bsd3Clause,
            License::OpenRailPlusPlusM,
            License::LlamaCommunity,
            License::Agpl3_0,
            License::CcByNc,
            License::PaidCommercial,
            License::RestrictedModel,
        ];
        let s = s.trim();
        all.into_iter().find(|l| l.label().eq_ignore_ascii_case(s))
    }

    /// A caveat a consumer/operator should see, when the license is allowed but
    /// carries a condition (Llama's MAU clause), else `None`.
    pub const fn note(self) -> Option<&'static str> {
        match self {
            License::LlamaCommunity => {
                Some("commercial use is free below 700M monthly active users; above that needs a Meta license")
            }
            License::OpenRailPlusPlusM => {
                Some("use-based restrictions apply (no illegal/harmful use); commercial use is allowed")
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allowlist_is_exactly_the_six_permissive_licenses() {
        for l in [
            License::Apache2_0,
            License::Mit,
            License::Bsd2Clause,
            License::Bsd3Clause,
            License::OpenRailPlusPlusM,
            License::LlamaCommunity,
        ] {
            assert!(l.is_allowed(), "{} must be allowed", l.label());
        }
    }

    #[test]
    fn the_banned_licenses_are_rejected() {
        // These are the traps the policy exists to stop — especially AGPL's
        // network clause. If any becomes allowed, that is a policy regression.
        for l in [
            License::Agpl3_0,
            License::CcByNc,
            License::PaidCommercial,
            License::RestrictedModel,
        ] {
            assert!(!l.is_allowed(), "{} must stay banned", l.label());
        }
    }
}
