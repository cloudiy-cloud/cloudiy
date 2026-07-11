//! Persistent node identity.
//!
//! Every Cloudiy node has a stable ed25519 key stored at
//! `~/.config/cloudiy/node.key`. Its public half is the iroh `EndpointId`,
//! which is simultaneously the node's network address, its TLS identity and
//! (later) the key that signs job results. This key is deliberately separate
//! from the Solana wallet: compromising the always-online node key must not
//! expose funds.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub fn config_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config/cloudiy")
}

pub fn node_key_path() -> PathBuf {
    config_dir().join("node.key")
}

/// Directory nodes use their own identity, separate from the provider node
/// key, so one machine can run both roles simultaneously.
///
/// The Directory ID (public half) is the network's stable entry point, so the
/// operator must be able to control and reproduce it. If `CLOUDIY_DIRECTORY_KEY`
/// is set (64 hex chars = 32 bytes), it is used verbatim — the ID becomes a
/// managed secret you can back up in a vault / systemd `EnvironmentFile` and
/// restore on any host, instead of a fragile 0600 file. Otherwise it falls back
/// to load-or-create at `~/.config/cloudiy/directory.key`.
pub fn load_or_create_directory_key() -> Result<iroh::SecretKey> {
    if let Ok(hex) = std::env::var("CLOUDIY_DIRECTORY_KEY") {
        let hex = hex.trim();
        if !hex.is_empty() {
            let bytes: [u8; 32] = hex::decode(hex)
                .context("CLOUDIY_DIRECTORY_KEY must be hex")?
                .try_into()
                .map_err(|_| {
                    anyhow::anyhow!("CLOUDIY_DIRECTORY_KEY must be 32 bytes (64 hex chars)")
                })?;
            return Ok(iroh::SecretKey::from_bytes(&bytes));
        }
    }
    load_or_create_key(&config_dir().join("directory.key"))
}

/// Consumer-side identity — stable across invocations so a consumer's VMs,
/// sessions and tunnels are all owned by the same key, and distinct from the
/// provider `node.key` so one machine can run both roles at once.
pub fn load_or_create_client_key() -> Result<iroh::SecretKey> {
    load_or_create_key(&config_dir().join("client.key"))
}

/// Load the node key, creating (and persisting with 0600 perms) a fresh one
/// on first run.
pub fn load_or_create_node_key() -> Result<iroh::SecretKey> {
    load_or_create_key(&node_key_path())
}

fn load_or_create_key(path: &std::path::Path) -> Result<iroh::SecretKey> {
    let path = path.to_path_buf();
    if path.exists() {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("reading node key at {}", path.display()))?;
        let bytes: [u8; 32] = hex::decode(content.trim())
            .context("node.key is not valid hex")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("node.key must be exactly 32 bytes"))?;
        Ok(iroh::SecretKey::from_bytes(&bytes))
    } else {
        fs::create_dir_all(config_dir())?;
        let bytes: [u8; 32] = rand::random();
        fs::write(&path, hex::encode(bytes))
            .with_context(|| format!("writing node key to {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(iroh::SecretKey::from_bytes(&bytes))
    }
}
