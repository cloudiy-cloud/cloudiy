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

/// Load the node key, creating (and persisting with 0600 perms) a fresh one
/// on first run.
pub fn load_or_create_node_key() -> Result<iroh::SecretKey> {
    let path = node_key_path();
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
