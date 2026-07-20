//! Solana network configuration — the single place cluster, RPC, USDC mint and
//! escrow program live for the node, the CLI and the Rust SDK.
//!
//! The point is that moving devnet → mainnet is *configuration*, not a hunt for
//! constants scattered across crates. Everything money-related resolves from one
//! [`SolanaConfig`], with a fixed precedence:
//!
//! ```text
//! explicit override (CLI flag)  →  environment  →  cluster default
//! ```
//!
//! Defaults are unchanged: with nothing set you get devnet, exactly as before.
//!
//! **Mainnet is deliberately not fully baked in.** The escrow program has never
//! been deployed to mainnet, so [`Cluster::Mainnet`] has no program id and
//! resolution fails with an explicit message rather than pointing real USDC at
//! an address that holds no program. After a mainnet deploy, filling in
//! [`MAINNET_ESCROW_PROGRAM`] is the one-line change that makes mainnet a pure
//! flag flip — see `docs/MAINNET-RUNBOOK.md`.

use anyhow::{bail, Result};

/// Devnet escrow program (deployed, in use today).
pub const DEVNET_ESCROW_PROGRAM: &str = "9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN";
/// Devnet USDC **test** mint — not Circle's, mintable for testing.
pub const DEVNET_USDC_MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
pub const DEVNET_RPC_URL: &str = "https://api.devnet.solana.com";

/// Circle's canonical mainnet USDC mint.
pub const MAINNET_USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub const MAINNET_RPC_URL: &str = "https://api.mainnet-beta.solana.com";
/// Mainnet escrow program — `None` until the program is actually deployed
/// there. Set this after the mainnet deploy (see the runbook); until then
/// mainnet users must pass the program id explicitly, which is the honest
/// failure mode: we never guess an address that real money would flow to.
pub const MAINNET_ESCROW_PROGRAM: Option<&str> = None;

/// Which Solana cluster the money layer talks to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Cluster {
    #[default]
    Devnet,
    Mainnet,
}

impl Cluster {
    pub fn parse(s: &str) -> Result<Cluster> {
        match s.trim().to_ascii_lowercase().as_str() {
            "devnet" | "dev" => Ok(Cluster::Devnet),
            "mainnet" | "mainnet-beta" | "main" => Ok(Cluster::Mainnet),
            other => bail!("unknown cluster {other:?} — expected 'devnet' or 'mainnet'"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Cluster::Devnet => "devnet",
            Cluster::Mainnet => "mainnet",
        }
    }

    pub fn is_mainnet(self) -> bool {
        matches!(self, Cluster::Mainnet)
    }

    pub fn default_rpc_url(self) -> &'static str {
        match self {
            Cluster::Devnet => DEVNET_RPC_URL,
            Cluster::Mainnet => MAINNET_RPC_URL,
        }
    }

    pub fn default_usdc_mint(self) -> &'static str {
        match self {
            Cluster::Devnet => DEVNET_USDC_MINT,
            Cluster::Mainnet => MAINNET_USDC_MINT,
        }
    }

    pub fn default_escrow_program(self) -> Option<&'static str> {
        match self {
            Cluster::Devnet => Some(DEVNET_ESCROW_PROGRAM),
            Cluster::Mainnet => MAINNET_ESCROW_PROGRAM,
        }
    }

    /// x402 network label carried in payment payloads.
    pub fn x402_network(self) -> &'static str {
        match self {
            Cluster::Devnet => "solana-devnet",
            Cluster::Mainnet => "solana",
        }
    }
}

/// Explicit (CLI-supplied) overrides. Anything `None` falls through to the
/// environment, then to the cluster default.
#[derive(Clone, Debug, Default)]
pub struct ConfigOverrides {
    pub cluster: Option<String>,
    pub rpc_url: Option<String>,
    pub usdc_mint: Option<String>,
    pub escrow_program: Option<String>,
}

/// Fully resolved money-layer configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolanaConfig {
    pub cluster: Cluster,
    pub rpc_url: String,
    pub usdc_mint: String,
    pub escrow_program: String,
    /// x402 network label matching `cluster`.
    pub x402_network: String,
}

pub const ENV_CLUSTER: &str = "CLOUDIY_CLUSTER";
pub const ENV_RPC_URL: &str = "CLOUDIY_RPC_URL";
pub const ENV_USDC_MINT: &str = "CLOUDIY_USDC_MINT";
pub const ENV_ESCROW_PROGRAM: &str = "CLOUDIY_ESCROW_PROGRAM";

impl SolanaConfig {
    /// Resolve against the process environment.
    pub fn resolve(overrides: &ConfigOverrides) -> Result<SolanaConfig> {
        Self::resolve_with(overrides, |k| std::env::var(k).ok())
    }

    /// Resolve against a caller-supplied environment. Keeps precedence testable
    /// without mutating the process env (which races across parallel tests).
    pub fn resolve_with(
        overrides: &ConfigOverrides,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<SolanaConfig> {
        // An empty env var is treated as unset — an exported-but-blank variable
        // should not silently win over the default.
        let from_env = |key: &str| env(key).filter(|v| !v.trim().is_empty());
        let pick = |over: &Option<String>, key: &str| -> Option<String> {
            over.clone()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| from_env(key))
        };

        let cluster = match pick(&overrides.cluster, ENV_CLUSTER) {
            Some(s) => Cluster::parse(&s)?,
            None => Cluster::default(),
        };

        let rpc_url = pick(&overrides.rpc_url, ENV_RPC_URL)
            .unwrap_or_else(|| cluster.default_rpc_url().to_string());
        let usdc_mint = pick(&overrides.usdc_mint, ENV_USDC_MINT)
            .unwrap_or_else(|| cluster.default_usdc_mint().to_string());
        let escrow_program = match pick(&overrides.escrow_program, ENV_ESCROW_PROGRAM) {
            Some(p) => p,
            None => match cluster.default_escrow_program() {
                Some(p) => p.to_string(),
                None => bail!(
                    "no escrow program is baked in for {cluster} — the Cloudiy escrow has not \
                     been deployed there yet. Pass --escrow-program <id> or set {ENV_ESCROW_PROGRAM}. \
                     See docs/MAINNET-RUNBOOK.md.",
                    cluster = cluster.as_str()
                ),
            },
        };

        Ok(SolanaConfig {
            cluster,
            rpc_url,
            usdc_mint,
            escrow_program,
            x402_network: cluster.x402_network().to_string(),
        })
    }

    /// The devnet defaults, infallible — for contexts with no configuration
    /// surface (test fixtures, payload builders that only need a network label).
    pub fn devnet() -> SolanaConfig {
        SolanaConfig {
            cluster: Cluster::Devnet,
            rpc_url: DEVNET_RPC_URL.to_string(),
            usdc_mint: DEVNET_USDC_MINT.to_string(),
            escrow_program: DEVNET_ESCROW_PROGRAM.to_string(),
            x402_network: Cluster::Devnet.x402_network().to_string(),
        }
    }

    /// Environment-only resolution, falling back to devnet if the environment is
    /// unusable. For callers that have no flags to offer (the SDK's payload
    /// builders) but should still follow `CLOUDIY_CLUSTER`.
    pub fn from_env_or_devnet() -> SolanaConfig {
        Self::resolve(&ConfigOverrides::default()).unwrap_or_else(|_| SolanaConfig::devnet())
    }

    pub fn is_mainnet(&self) -> bool {
        self.cluster.is_mainnet()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn default_is_devnet_unchanged() {
        let c = SolanaConfig::resolve_with(&ConfigOverrides::default(), no_env).unwrap();
        assert_eq!(c.cluster, Cluster::Devnet);
        assert_eq!(c.rpc_url, DEVNET_RPC_URL);
        assert_eq!(c.usdc_mint, DEVNET_USDC_MINT);
        assert_eq!(c.escrow_program, DEVNET_ESCROW_PROGRAM);
        assert_eq!(c.x402_network, "solana-devnet");
        assert!(!c.is_mainnet());
    }

    #[test]
    fn env_overrides_the_default() {
        let env = env_from(&[(ENV_RPC_URL, "https://my-devnet.example")]);
        let c = SolanaConfig::resolve_with(&ConfigOverrides::default(), env).unwrap();
        assert_eq!(c.rpc_url, "https://my-devnet.example");
        // Everything not overridden still comes from the cluster default.
        assert_eq!(c.usdc_mint, DEVNET_USDC_MINT);
    }

    #[test]
    fn cli_override_beats_env() {
        let env = env_from(&[
            (ENV_RPC_URL, "https://from-env.example"),
            (ENV_USDC_MINT, "EnvMint"),
        ]);
        let overrides = ConfigOverrides {
            rpc_url: Some("https://from-flag.example".into()),
            ..Default::default()
        };
        let c = SolanaConfig::resolve_with(&overrides, env).unwrap();
        assert_eq!(c.rpc_url, "https://from-flag.example", "flag wins");
        assert_eq!(c.usdc_mint, "EnvMint", "env still applies where no flag");
    }

    #[test]
    fn cluster_from_env_switches_every_default_together() {
        let env = env_from(&[
            (ENV_CLUSTER, "mainnet"),
            // Mainnet has no baked-in program, so supply one.
            (
                ENV_ESCROW_PROGRAM,
                "SomeMainnetProgram1111111111111111111111111",
            ),
        ]);
        let c = SolanaConfig::resolve_with(&ConfigOverrides::default(), env).unwrap();
        assert_eq!(c.cluster, Cluster::Mainnet);
        assert_eq!(c.rpc_url, MAINNET_RPC_URL);
        assert_eq!(c.usdc_mint, MAINNET_USDC_MINT, "canonical Circle mint");
        assert_eq!(c.x402_network, "solana");
        assert!(c.is_mainnet());
    }

    #[test]
    fn mainnet_without_a_program_id_is_a_clear_error() {
        let env = env_from(&[(ENV_CLUSTER, "mainnet")]);
        let err = SolanaConfig::resolve_with(&ConfigOverrides::default(), env)
            .expect_err("mainnet has no deployed program yet");
        let msg = err.to_string();
        // The message must say *why* and *what to do*, not just fail.
        assert!(msg.contains("mainnet"), "{msg}");
        assert!(msg.contains("not been deployed"), "{msg}");
        assert!(msg.contains(ENV_ESCROW_PROGRAM), "{msg}");
        assert!(msg.contains("MAINNET-RUNBOOK"), "{msg}");
    }

    #[test]
    fn blank_values_do_not_win_over_defaults() {
        // An exported-but-empty var (`export CLOUDIY_RPC_URL=`) must not blank
        // out the RPC endpoint.
        let env = env_from(&[(ENV_RPC_URL, "   ")]);
        let overrides = ConfigOverrides {
            usdc_mint: Some(String::new()),
            ..Default::default()
        };
        let c = SolanaConfig::resolve_with(&overrides, env).unwrap();
        assert_eq!(c.rpc_url, DEVNET_RPC_URL);
        assert_eq!(c.usdc_mint, DEVNET_USDC_MINT);
    }

    #[test]
    fn cluster_parsing_accepts_aliases_and_rejects_junk() {
        assert_eq!(Cluster::parse("devnet").unwrap(), Cluster::Devnet);
        assert_eq!(Cluster::parse("  DevNet ").unwrap(), Cluster::Devnet);
        assert_eq!(Cluster::parse("mainnet-beta").unwrap(), Cluster::Mainnet);
        assert_eq!(Cluster::parse("MAINNET").unwrap(), Cluster::Mainnet);
        assert!(Cluster::parse("testnet").is_err());
        assert!(Cluster::parse("").is_err());
    }

    #[test]
    fn an_explicit_program_override_applies_on_devnet_too() {
        // Local validator / a redeployed program under test.
        let overrides = ConfigOverrides {
            escrow_program: Some("LocalProgram11111111111111111111111111111111".into()),
            ..Default::default()
        };
        let c = SolanaConfig::resolve_with(&overrides, no_env).unwrap();
        assert_eq!(
            c.escrow_program,
            "LocalProgram11111111111111111111111111111111"
        );
        assert_eq!(c.cluster, Cluster::Devnet);
    }
}
