//! Declarative worker manifests (PROTOCOL.md §18) — the fix for the catalog
//! being hardcoded in `gateway.rs`.
//!
//! Today adding or correcting a model means editing Rust (`gateway.rs`) *and*
//! HTML (`web/os.html`) in lockstep — the last serious incoherence with
//! PROTOCOL.md, which preaches *protocol ≠ implementation* while the catalog
//! lives inside the implementation. A manifest is a small JSON file describing
//! one worker: a third party can add a worker by dropping a manifest, no PR to
//! the core.
//!
//! Design borrowed from a proven system (ODS: `manifest.yaml` + `compose.yaml`
//! validated by a versioned JSON Schema, with `schema_version` +
//! `compatibility{min,max}`): we adopt the same shape (§16.1) so it is
//! known-good, not invented. JSON (not YAML/TOML) to match the repo's existing
//! config files (`worker_digests.json`, `hosted_models.json`) and add no
//! dependency.
//!
//! The **license is enforced here too**: a manifest whose `license` is not on
//! the [`crate::license`] allowlist is rejected at load — the same build-time
//! trava, now applied to third-party manifests at runtime.

use crate::license::License;
use serde::Deserialize;

/// The manifest schema version this build speaks. Bump on a breaking change to
/// the manifest shape; the `compatibility` range each manifest declares is
/// checked against it (§16.1 / R16.2).
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Inclusive support range a document declares (§16.1). A manifest is loadable
/// iff `min <= CURRENT_SCHEMA_VERSION <= max`.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub struct Compatibility {
    pub min: u32,
    pub max: u32,
}

impl Compatibility {
    /// R16.2: is `version` within `[min, max]`?
    pub fn accepts(&self, version: u32) -> bool {
        self.min <= version && version <= self.max
    }
}

/// Resource floor a worker needs to run — used to guide placement and the
/// `cloudiy share` hardware hint (Item 4). All optional; absent = unconstrained.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Requirements {
    pub vram_gb: u64,
    pub memory_gb: u64,
}

/// Whether a worker's image actually exists and can be pulled today, vs is
/// announced-but-not-yet-published. A `planned` worker is legitimate — what is
/// forbidden is `planned` disguised as available (a Deploy button that 404s).
/// The image-existence verifier only enforces existence for `available` ones.
/// Defaults to `planned` on purpose: an entry is only `available` when someone
/// deliberately says so (and the verifier then holds them to it).
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Available,
    #[default]
    Planned,
}

impl Status {
    pub fn is_available(self) -> bool {
        self == Status::Available
    }
    pub fn label(self) -> &'static str {
        match self {
            Status::Available => "available",
            Status::Planned => "planned",
        }
    }
}

/// One worker's description. Field set adapted from ODS's service block.
#[derive(Debug, Clone, Deserialize)]
pub struct Worker {
    /// Catalog key (`sdxl`, `bge-m3`, …) — mirrors os.html's ENDPOINTS.
    pub id: String,
    /// OCI image ref (tag or, preferably, digest-pinned `repo@sha256:…`).
    pub image: String,
    /// Reviewed image digest (`sha256:…`) for supply-chain pinning. When set,
    /// `available` items are verified at this exact digest, not a mutable tag.
    #[serde(default)]
    pub digest: String,
    /// `available` (image published) vs `planned` (announced, not yet built).
    #[serde(default)]
    pub status: Status,
    /// `image` / `video` / `audio` / `embed` / `ocr` / `vision` / … .
    pub category: String,
    /// The model's weights license, as a [`License::label`] string. MUST be on
    /// the allowlist (validated at load).
    pub license: String,
    /// Human model name for cards/logs.
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub needs_gpu: bool,
    /// HTTP health path the gateway polls before routing (e.g. `/health`).
    #[serde(default)]
    pub health: String,
    /// HTTP inference path (e.g. `/sdapi/v1/txt2img`).
    #[serde(default)]
    pub api_path: String,
    /// TCP port the app serves its **web UI** on inside the VM (Jupyter 8888,
    /// code-server 8080, Grafana 3000, …). `0` = no web UI — the signal for the
    /// frontend to open the real Terminal instead of a panel. What a deploy
    /// gives access to is a property of the app, carried here.
    #[serde(default)]
    pub port: u16,
    /// Path the browser should open on that port (default `/`); e.g. Jupyter may
    /// want `/lab`. Only meaningful when `port != 0`.
    #[serde(default)]
    pub ui_path: String,
    /// Seconds to wait for the worker to become healthy on cold start.
    #[serde(default)]
    pub startup_timeout_secs: u64,
    /// GPU backends the worker supports (`["cuda"]`, `["rocm"]`, `[]` = CPU).
    #[serde(default)]
    pub gpu_backends: Vec<String>,
    #[serde(default)]
    pub requirements: Requirements,
}

/// A worker manifest file.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerManifest {
    pub schema_version: u32,
    pub compatibility: Compatibility,
    pub worker: Worker,
}

/// A validated manifest: its schema is compatible and its license is allowlisted.
#[derive(Debug, Clone)]
pub struct ValidatedWorker {
    pub worker: Worker,
    pub license: License,
}

impl WorkerManifest {
    /// Parse + validate a manifest from JSON. Rejects (a) a schema version this
    /// build can't speak (§16.2 fail-closed), and (b) a license off the
    /// allowlist (the trava, applied to third-party manifests).
    pub fn load(json: &str) -> Result<ValidatedWorker, String> {
        let m: WorkerManifest =
            serde_json::from_str(json).map_err(|e| format!("invalid manifest JSON: {e}"))?;

        // §16.2: the manifest's declared range must contain the version this
        // build actually speaks, AND the version it stamps must be within it.
        if !m.compatibility.accepts(CURRENT_SCHEMA_VERSION) {
            return Err(format!(
                "manifest for '{}' declares compatibility {{min:{}, max:{}}} which excludes this \
                 build's schema v{CURRENT_SCHEMA_VERSION} — refusing (fail closed)",
                m.worker.id, m.compatibility.min, m.compatibility.max
            ));
        }
        if !m.compatibility.accepts(m.schema_version) {
            return Err(format!(
                "manifest for '{}' stamps schema_version {} outside its own compatibility range",
                m.worker.id, m.schema_version
            ));
        }

        // The trava, for manifests: the license must parse and be allowlisted.
        let license = License::from_label(&m.worker.license).ok_or_else(|| {
            format!(
                "manifest for '{}' has unknown license {:?}",
                m.worker.id, m.worker.license
            )
        })?;
        if !license.is_allowed() {
            return Err(format!(
                "manifest for '{}' has non-allowlisted license {} — a closed/restricted model \
                 cannot be served by the network (see crate::license)",
                m.worker.id,
                license.label()
            ));
        }

        Ok(ValidatedWorker {
            worker: m.worker,
            license,
        })
    }
}

/// Load every `*.json` manifest in `dir` (a worker-manifest directory, e.g.
/// `CLOUDIY_WORKERS_DIR`). Invalid/rejected manifests are skipped with a warning
/// — one bad third-party manifest never takes the node down. Returns the
/// validated workers.
pub fn load_dir(dir: &std::path::Path) -> Vec<ValidatedWorker> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(json) => match WorkerManifest::load(&json) {
                Ok(w) => out.push(w),
                Err(e) => tracing::warn!("skipping manifest {}: {e}", path.display()),
            },
            Err(e) => tracing::warn!("cannot read manifest {}: {e}", path.display()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The proof migration: the `sdxl` catalog entry expressed as a manifest.
    const SDXL_MANIFEST: &str = include_str!("../manifests/sdxl.json");

    #[test]
    fn the_sdxl_proof_manifest_loads_and_matches_the_catalog() {
        let v = WorkerManifest::load(SDXL_MANIFEST).expect("sdxl manifest valid");
        assert_eq!(v.worker.id, "sdxl");
        assert_eq!(v.worker.image, "ghcr.io/w3-surfer/worker-sdxl:latest");
        assert_eq!(v.worker.category, "image");
        assert!(v.worker.needs_gpu);
        assert_eq!(v.license, License::OpenRailPlusPlusM);
        assert!(v.license.is_allowed());
        assert_eq!(v.worker.requirements.vram_gb, 8);
    }

    #[test]
    fn a_banned_license_manifest_is_rejected() {
        // AGPL (e.g. a YOLO worker) must never load — the runtime trava.
        let m = r#"{
            "schema_version": 1, "compatibility": {"min":1,"max":1},
            "worker": {"id":"yolo","image":"x","category":"detect","license":"AGPL-3.0"}
        }"#;
        let err = WorkerManifest::load(m).unwrap_err();
        assert!(err.contains("non-allowlisted"), "{err}");
    }

    #[test]
    fn the_musicgen_case_is_caught_automatically() {
        // MusicGen: code is MIT but the *weights* are CC-BY-NC-4.0, and the
        // network charges USDC (commercial) — a violation. A manifest carrying it
        // as CC-BY-NC is rejected by the same allowlist, no special-casing.
        let m = r#"{
            "schema_version": 1, "compatibility": {"min":1,"max":1},
            "worker": {"id":"musicgen","image":"ghcr.io/w3-surfer/worker-musicgen:latest",
                       "category":"audio","license":"CC-BY-NC","status":"available"}
        }"#;
        let err = WorkerManifest::load(m).unwrap_err();
        assert!(err.contains("non-allowlisted"), "{err}");
    }

    #[test]
    fn an_unknown_license_is_rejected() {
        let m = r#"{
            "schema_version": 1, "compatibility": {"min":1,"max":1},
            "worker": {"id":"x","image":"x","category":"image","license":"WTFPL"}
        }"#;
        assert!(WorkerManifest::load(m)
            .unwrap_err()
            .contains("unknown license"));
    }

    #[test]
    fn an_incompatible_schema_fails_closed() {
        // A manifest that only supports a future schema (min:2) is refused now.
        let m = r#"{
            "schema_version": 2, "compatibility": {"min":2,"max":2},
            "worker": {"id":"x","image":"x","category":"image","license":"MIT"}
        }"#;
        let err = WorkerManifest::load(m).unwrap_err();
        assert!(err.contains("fail closed"), "{err}");
    }

    #[test]
    fn compatibility_range_is_inclusive() {
        let c = Compatibility { min: 1, max: 3 };
        assert!(c.accepts(1) && c.accepts(2) && c.accepts(3));
        assert!(!c.accepts(0) && !c.accepts(4));
    }

    #[test]
    fn load_dir_keeps_valid_and_drops_rejected() {
        // End-to-end file→validated: a dir with one good manifest, one AGPL
        // (banned), one malformed, and a non-.json file. Only the good one loads;
        // a bad third-party manifest never takes the node down.
        let dir = std::env::temp_dir().join(format!("cloudiy-mf-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("good.json"), SDXL_MANIFEST).unwrap();
        std::fs::write(
            dir.join("banned.json"),
            r#"{"schema_version":1,"compatibility":{"min":1,"max":1},
                "worker":{"id":"yolo","image":"x","category":"detect","license":"AGPL-3.0"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("broken.json"), "{ not json").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();

        let loaded = load_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(loaded.len(), 1, "only the valid manifest loads");
        assert_eq!(loaded[0].worker.id, "sdxl");
    }
}
