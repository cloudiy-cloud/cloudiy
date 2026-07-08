//! Zero-config discovery bootstrap.
//!
//! The whole network hinges on one thing: a first-time user must be able to
//! find *some* directory without being told a node id out-of-band. Because
//! iroh dials by `EndpointId` over relays, a directory running at a fixed
//! keypair is reachable from anywhere with no IP or DNS — so a single baked-in
//! id bootstraps discovery for everyone.
//!
//! Resolution order (first non-empty wins):
//!   1. explicit `--via` / `--directory` flags,
//!   2. the `CLOUDIY_DIRECTORY` env var (comma-separated ids),
//!   3. the compile-time default `DEFAULT_DIRECTORY`.
//!
//! Bake the default in at build time without touching source:
//!   CLOUDIY_DEFAULT_DIRECTORY=<endpoint-id> cargo build --release

/// Directory id compiled into the binary (empty when none was provided).
pub const DEFAULT_DIRECTORY: &str = match option_env!("CLOUDIY_DEFAULT_DIRECTORY") {
    Some(s) => s,
    None => "",
};

/// Env var that overrides the compiled default at runtime (comma-separated).
pub const DIRECTORY_ENV: &str = "CLOUDIY_DIRECTORY";

/// Resolve the effective directory list from explicit flags, then the env var,
/// then the compiled default. Empty only when nothing is configured anywhere.
pub fn resolve_directories(explicit: Vec<String>) -> Vec<String> {
    if !explicit.is_empty() {
        return explicit;
    }
    if let Ok(env) = std::env::var(DIRECTORY_ENV) {
        let ids = split_ids(&env);
        if !ids.is_empty() {
            return ids;
        }
    }
    if !DEFAULT_DIRECTORY.is_empty() {
        return vec![DEFAULT_DIRECTORY.to_string()];
    }
    Vec::new()
}

fn split_ids(s: &str) -> Vec<String> {
    s.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_wins() {
        let out = resolve_directories(vec!["a".into(), "b".into()]);
        assert_eq!(out, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn splits_env_list() {
        assert_eq!(split_ids(" a , b ,,c "), vec!["a", "b", "c"]);
        assert!(split_ids("   ").is_empty());
    }

    #[test]
    fn empty_when_nothing_configured() {
        // No flags, and (in the test binary) no compiled default / env set.
        if std::env::var(DIRECTORY_ENV).is_err() && DEFAULT_DIRECTORY.is_empty() {
            assert!(resolve_directories(vec![]).is_empty());
        }
    }
}
