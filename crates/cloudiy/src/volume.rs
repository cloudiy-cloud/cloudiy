//! Persistent Volume v2 (RFC-0009) — the pure, testable core.
//!
//! Two independent pieces live here, both free of I/O so they can be unit-tested
//! without a network or a provider:
//!
//! - [`volume_key`]: derive a 32-byte symmetric key from the consumer's wallet,
//!   used as the restic repository password for client-encrypted snapshots.
//! - [`Manifest`]: parse a declarative `cloudiy.volume.toml` that rebuilds an
//!   environment from upstream sources instead of syncing a whole home ("cattle").
//!
//! The wiring that *uses* these (the restic sidecar, the tunnel sync) lives in
//! `vm.rs` behind `CLOUDIY_VOLUME_MODE=snapshot`; the default rclone path is
//! untouched.

use anyhow::{Context, Result};
use hkdf::Hkdf;
use serde::Deserialize;
use sha2::Sha256;

/// Domain string signed to derive the volume key. Versioned so a future key
/// model (RFC-0009 §3.3 rotation) can move to `…/v2` without colliding with v1
/// snapshots, and domain-separated so this signature can never be replayed as
/// any other protocol signature (result/announce/escrow-run all carry their
/// own domains).
pub const VOLUME_KEY_DOMAIN: &[u8] = b"cloudiy/volume-key/v1";

/// Derive the 32-byte volume-encryption key from a **deterministic** ed25519
/// signature over [`VOLUME_KEY_DOMAIN`], bound to `owner_id`.
///
/// ```text
/// sig = Ed25519_sign(wallet_sk, VOLUME_KEY_DOMAIN)          // 64 bytes, deterministic (RFC 8032)
/// key = HKDF-SHA256(ikm = sig, salt = VOLUME_KEY_DOMAIN, info = owner_id)
/// ```
///
/// The caller passes the signature (produced by `crate::solana::Keypair::
/// sign_message`) rather than the wallet key itself, so this function never
/// touches secret key material and stays trivially testable. Determinism is
/// load-bearing: ed25519-dalek is RFC 8032, so the same wallet always yields the
/// same signature and therefore the same key — a randomized signer would make
/// every session's key different and every restore fail.
pub fn volume_key_from_signature(signature: &[u8; 64], owner_id: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(VOLUME_KEY_DOMAIN), signature);
    let mut key = [0u8; 32];
    hk.expand(owner_id, &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

/// The restic repository password for this key: the derived 32 bytes as
/// lowercase hex, safe to pass through `--password-command` / an env var
/// (no NULs, no shell metacharacters).
pub fn restic_password(key: &[u8; 32]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

/// Derive the volume key straight from the consumer's wallet: sign the domain
/// (deterministically) and HKDF the signature. This is the one place the wallet
/// key is touched; the signature it produces never leaves the client under
/// Architecture B (RFC-0009 §3.2).
///
/// The interim provider-side path (`vm.rs`) instead derives from a handed-over
/// signature via [`volume_key_from_signature`]; this wallet-side entry point is
/// where the consumer-side sync (Architecture B) will derive locally. Kept and
/// tested now so that path lands against verified code.
#[cfg_attr(not(test), allow(dead_code))]
pub fn volume_key(wallet: &crate::solana::Keypair, owner_id: &[u8]) -> [u8; 32] {
    let sig = wallet.sign_message(VOLUME_KEY_DOMAIN);
    volume_key_from_signature(&sig, owner_id)
}

// ------------------------------------------------------------- manifest

/// A declarative environment (`cloudiy.volume.toml`, RFC-0009 §4). Rebuilds an
/// environment from upstream sources on any node, no volume sync required.
/// Every section is optional so a minimal manifest is just `[env]`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub env: EnvSection,
    #[serde(default)]
    pub packages: Packages,
    /// Path → source (`inline:…`, `git:…`, or a URL). Kept as raw strings here;
    /// the builder in `vm.rs` interprets the scheme.
    #[serde(default)]
    pub dotfiles: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub repos: std::collections::BTreeMap<String, Repo>,
    #[serde(default)]
    pub secrets: Option<Secrets>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnvSection {
    /// Base image; defaults to the VM default when absent.
    pub image: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Packages {
    #[serde(default)]
    pub apt: Vec<String>,
    #[serde(default)]
    pub pipx: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    pub url: String,
    #[serde(default = "default_ref")]
    pub r#ref: String,
}

fn default_ref() -> String {
    "main".to_string()
}

/// The single ciphertext blob of secrets, decrypted with the wallet key at build
/// time over the tunnel — never stored plaintext on the provider.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Secrets {
    /// Where the ciphertext lives (`restic:secrets`, or an inline `age:` blob).
    pub blob: String,
}

impl Manifest {
    /// Parse a manifest from TOML text. Rejects unknown keys so a typo fails
    /// loudly rather than silently doing nothing.
    pub fn parse(toml_text: &str) -> Result<Manifest> {
        toml::from_str(toml_text).context("parsing cloudiy.volume.toml")
    }

    /// Validate the shapes the builder relies on. Cheap invariants only; the
    /// builder does the network work.
    pub fn validate(&self) -> Result<()> {
        for (path, src) in &self.dotfiles {
            anyhow::ensure!(!path.is_empty(), "dotfile path must not be empty");
            anyhow::ensure!(
                src.starts_with("inline:") || src.starts_with("git:") || src.contains("://"),
                "dotfile {path:?} source must be inline:, git:, or a URL"
            );
        }
        for (dest, repo) in &self.repos {
            anyhow::ensure!(!dest.is_empty(), "repo destination must not be empty");
            anyhow::ensure!(
                repo.url.contains("://") || repo.url.starts_with("git@"),
                "repo {dest:?} url must be a URL or scp-style git remote"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- key derivation ----

    #[test]
    fn key_is_deterministic_in_signature_and_owner() {
        let sig = [7u8; 64];
        let a = volume_key_from_signature(&sig, b"owner-A");
        let b = volume_key_from_signature(&sig, b"owner-A");
        assert_eq!(a, b, "same signature + owner must give the same key");
    }

    #[test]
    fn key_depends_on_the_owner() {
        let sig = [7u8; 64];
        let a = volume_key_from_signature(&sig, b"owner-A");
        let b = volume_key_from_signature(&sig, b"owner-B");
        assert_ne!(a, b, "different owners must not share a volume key");
    }

    #[test]
    fn key_depends_on_the_signature() {
        let a = volume_key_from_signature(&[1u8; 64], b"owner");
        let b = volume_key_from_signature(&[2u8; 64], b"owner");
        assert_ne!(
            a, b,
            "a different wallet (signature) must give a different key"
        );
    }

    #[test]
    fn restic_password_is_hex_and_safe() {
        let key = [0xabu8; 32];
        let pw = restic_password(&key);
        assert_eq!(pw.len(), 64);
        assert!(pw.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(&pw[..4], "abab");
        // No NUL / whitespace / shell metacharacters.
        assert!(pw
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
    }

    #[test]
    fn wallet_derivation_is_deterministic_end_to_end() {
        // The load-bearing property (RFC-0009 §3.1): a real ed25519 signer must
        // produce the SAME volume key every time, or restores break. Two
        // independent keypairs from the same seed must agree, and a different
        // wallet must differ.
        let w1 = crate::solana::Keypair::from_seed([42u8; 32]);
        let w2 = crate::solana::Keypair::from_seed([42u8; 32]);
        let owner = b"node-abc";
        assert_eq!(
            volume_key(&w1, owner),
            volume_key(&w2, owner),
            "same wallet must derive the same volume key across sessions"
        );
        let other = crate::solana::Keypair::from_seed([43u8; 32]);
        assert_ne!(
            volume_key(&w1, owner),
            volume_key(&other, owner),
            "a different wallet must derive a different key"
        );
    }

    #[test]
    fn domain_is_the_versioned_v1_string() {
        // The derivation and any on-wallet signing must agree on this exact
        // domain; pin it so a careless edit breaks the test, not restores.
        assert_eq!(VOLUME_KEY_DOMAIN, b"cloudiy/volume-key/v1");
    }

    // ---- manifest ----

    #[test]
    fn minimal_manifest_parses() {
        let m = Manifest::parse("[env]\nimage = \"debian:12-slim\"\n").unwrap();
        assert_eq!(m.env.image.as_deref(), Some("debian:12-slim"));
        assert!(m.packages.apt.is_empty());
        assert!(m.validate().is_ok());
    }

    #[test]
    fn empty_manifest_is_valid() {
        let m = Manifest::parse("").unwrap();
        assert_eq!(m, Manifest::default());
        assert!(m.validate().is_ok());
    }

    #[test]
    fn full_manifest_round_trips_the_shapes() {
        let src = r#"
            [env]
            image = "debian:12-slim"

            [packages]
            apt = ["build-essential", "git"]
            pipx = ["poetry"]

            [dotfiles]
            ".gitconfig" = "inline:[user]\n  name = x"
            ".config/nvim/" = "git:https://github.com/user/nvim"

            [repos]
            "~/work/app" = { url = "https://github.com/org/app", ref = "dev" }

            [secrets]
            blob = "restic:secrets"
        "#;
        let m = Manifest::parse(src).unwrap();
        assert_eq!(m.packages.apt, vec!["build-essential", "git"]);
        assert_eq!(m.packages.pipx, vec!["poetry"]);
        assert_eq!(m.repos["~/work/app"].r#ref, "dev");
        assert_eq!(m.secrets.as_ref().unwrap().blob, "restic:secrets");
        assert!(m.validate().is_ok());
    }

    #[test]
    fn repo_ref_defaults_to_main() {
        let m = Manifest::parse("[repos]\n\"~/app\" = { url = \"https://github.com/org/app\" }\n")
            .unwrap();
        assert_eq!(m.repos["~/app"].r#ref, "main");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        // A typo (`pakages`) must fail loudly, not silently install nothing.
        let err = Manifest::parse("[pakages]\napt = []\n").unwrap_err();
        assert!(err.to_string().contains("cloudiy.volume.toml"));
    }

    #[test]
    fn a_bad_dotfile_source_is_rejected_by_validate() {
        let m = Manifest::parse("[dotfiles]\n\".x\" = \"/etc/passwd\"\n").unwrap();
        assert!(m.validate().is_err(), "a bare path is not a valid source");
    }

    #[test]
    fn a_bad_repo_url_is_rejected_by_validate() {
        let m = Manifest::parse("[repos]\n\"~/a\" = { url = \"not-a-url\" }\n").unwrap();
        assert!(m.validate().is_err());
    }
}
