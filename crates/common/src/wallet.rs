use anyhow::Result;
use std::path::PathBuf;

pub fn default_keypair_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config/solana/id.json")
}

pub fn load_pubkey() -> Result<String> {
    let path = default_keypair_path();
    if !path.exists() {
        anyhow::bail!("Solana keypair not found at {:?}", path);
    }

    // Minimal parser for [u8; 64] JSON array (Solana CLI format)
    let content = std::fs::read_to_string(&path)?;
    let bytes: Vec<u8> = serde_json::from_str(&content)?;

    if bytes.len() != 64 {
        anyhow::bail!("Invalid keypair length");
    }

    // Pubkey is the last 32 bytes
    let pubkey_bytes = &bytes[32..];
    let pubkey = bs58::encode(pubkey_bytes).into_string();

    Ok(pubkey)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keypair_path_points_at_solana_config() {
        let path = default_keypair_path();
        assert!(path.ends_with(".config/solana/id.json"));
    }

    #[test]
    fn load_pubkey_errors_when_missing() {
        // No keypair is configured in the test environment, so this must
        // return an error rather than panic.
        let path = default_keypair_path();
        if !path.exists() {
            assert!(load_pubkey().is_err());
        }
    }
}
