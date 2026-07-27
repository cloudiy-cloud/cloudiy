//! Provider payout **address** — not a wallet (RFC-0014).
//!
//! Receiving USDC needs only the public key; the private key only ever *spends*,
//! so an always-online node that accepts strangers' code must never hold one.
//! This mirrors the node key (`crates/common/src/keys.rs`): auto-managed identity
//! that controls no money. The address lives at `~/.config/cloudiy/payout` as a
//! bare base58 string. A legacy `~/.config/solana/id.json` is still accepted as a
//! source (we read only its public half), for compatibility.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};

use crate::solana::{is_on_curve, parse_pubkey, pubkey_str, Pubkey};

/// Message-domain for the browser payout-binding signature (item 4). Separate
/// from result/announce/volume-key/escrow-run so a signature for one purpose can
/// never be replayed as another.
pub const PAYOUT_BIND_DOMAIN: &[u8] = b"cloudiy/payout-bind/v1";

/// Announced as the payout address only in explicit `--no-payout` (donate) mode.
/// `core` maps it to "no on-chain payout" (`solana_pubkey: None`).
pub const DONATE_SENTINEL: &str = "<no-payout>";

/// Brand green (`#ccff33`) — the exact sequence `os.html` uses. Empty when the
/// output isn't a TTY or `NO_COLOR` is set, so piped/redirected output stays clean.
fn brand() -> &'static str {
    if std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        "\x1b[38;2;204;255;51m"
    } else {
        ""
    }
}
fn reset() -> &'static str {
    if std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        "\x1b[0m"
    } else {
        ""
    }
}

pub fn config_path() -> PathBuf {
    cloudiy_common::config_dir().join("payout")
}

/// Validate a base58 string as a **payout destination**: 32 bytes AND on the
/// ed25519 curve. An off-curve key is a program address (PDA) and cannot own a
/// token account — paying it would burn the funds, so this is a funds-safety
/// check, not hygiene.
pub fn validate_payout_address(s: &str) -> Result<Pubkey> {
    let pk = parse_pubkey(s.trim()).map_err(|_| anyhow!("not a valid base58 Solana address"))?;
    anyhow::ensure!(
        is_on_curve(&pk),
        "address is off-curve — that is a program address (PDA), which cannot receive payments"
    );
    Ok(pk)
}

/// The configured payout address, or `None`. Priority: the dedicated
/// `~/.config/cloudiy/payout` file (address only), then a legacy
/// `~/.config/solana/id.json` (compat — only its public half is read). A stored
/// value that no longer validates is ignored (treated as unset).
pub fn load() -> Option<String> {
    if let Ok(s) = std::fs::read_to_string(config_path()) {
        let s = s.trim();
        if let Ok(pk) = validate_payout_address(s) {
            return Some(pubkey_str(&pk));
        }
    }
    cloudiy_common::load_pubkey()
        .ok()
        .and_then(|s| validate_payout_address(&s).ok())
        .map(|pk| pubkey_str(&pk))
}

/// Persist a validated payout address to `~/.config/cloudiy/payout`.
pub fn save(address: &str) -> Result<()> {
    let pk = validate_payout_address(address)?;
    let dir = cloudiy_common::config_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(config_path(), format!("{}\n", pubkey_str(&pk)))
        .with_context(|| format!("writing {}", config_path().display()))?;
    Ok(())
}

/// Resolve the payout address for `share`, failing loud rather than ever
/// announcing a placeholder. Order: explicit flag/env → stored config →
/// interactive prompt (TTY only) → error with instructions. `--no-payout`
/// (donate) is handled by the caller before this is reached.
pub fn resolve(flag: Option<String>, env: Option<String>) -> Result<String> {
    if let Some(a) = flag.or(env) {
        let pk = validate_payout_address(&a)?;
        save(&pubkey_str(&pk))?;
        return Ok(pubkey_str(&pk));
    }
    if let Some(a) = load() {
        return Ok(a);
    }
    if std::io::stdin().is_terminal() {
        return interactive_setup();
    }
    bail!(
        "no payout address configured. Set one of:\n  \
         • --payout <ADDRESS>\n  \
         • CLOUDIY_PAYOUT_ADDRESS=<ADDRESS>\n  \
         • write the base58 address to {}\n\
         To run without payments (donate compute), pass --no-payout.",
        config_path().display()
    );
}

/// First-run prompt: one question, validated for real, saved. Offers the
/// browser-pairing path for someone who doesn't have their address handy.
fn interactive_setup() -> Result<String> {
    let (g, r) = (brand(), reset());
    println!(
        "\n{g}Cloudiy — where should your earnings go?{r}\n\
         Your node needs a Solana ADDRESS to receive USDC. It stores only the public\n\
         address — never your private key. (Paste an address from Phantom, a Ledger,\n\
         or an exchange deposit address.)\n"
    );
    loop {
        print!("Payout address (or type 'wallet' to connect a wallet in the browser): ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            bail!("input closed before a payout address was given");
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.eq_ignore_ascii_case("wallet") {
            match browser_pairing() {
                Ok(addr) => return Ok(addr),
                Err(e) => {
                    println!(
                        "  browser pairing didn't complete ({e}). Try again or paste an address."
                    );
                    continue;
                }
            }
        }
        match validate_payout_address(line) {
            Ok(pk) => {
                let addr = pubkey_str(&pk);
                save(&addr)?;
                println!("{g}✓ payout address saved:{r} {addr}");
                return Ok(addr);
            }
            Err(e) => println!("  {e}. Try again."),
        }
    }
}

/// Browser pairing (item 4), node side. Prints a CloudiyOS URL + code; the user
/// connects a wallet, signs `PAYOUT_BIND_DOMAIN || code`, and pastes back a
/// binding token. We verify the signature and return the confirmed address.
fn browser_pairing() -> Result<String> {
    let (g, r) = (brand(), reset());
    let code = cloudiy_common::generate_access_code();
    println!(
        "\n{g}Connect your wallet:{r}\n  \
         1. Open: https://cloudiy.cloud/pair#code={code}\n  \
         2. Connect the wallet you already use and approve the signature.\n  \
         3. Paste the confirmation token it shows you here.\n"
    );
    print!("Confirmation token: ");
    std::io::stdout().flush().ok();
    let mut token = String::new();
    if std::io::stdin().read_line(&mut token)? == 0 {
        bail!("input closed before a token was pasted");
    }
    let addr = verify_binding_token(token.trim(), &code)?;
    save(&addr)?;
    println!("{g}✓ wallet connected — payout address:{r} {addr}");
    Ok(addr)
}

/// The message a wallet signs to bind an address to a pairing session.
fn binding_message(code: &str) -> Vec<u8> {
    let mut m = Vec::with_capacity(PAYOUT_BIND_DOMAIN.len() + 1 + code.len());
    m.extend_from_slice(PAYOUT_BIND_DOMAIN);
    m.push(b'|');
    m.extend_from_slice(code.as_bytes());
    m
}

/// Parse and verify a binding token `base58(address(32) || signature(64))`:
/// the signature must be a valid ed25519 signature of `PAYOUT_BIND_DOMAIN||code`
/// by `address`, and `address` must be an on-curve payout destination. Returns
/// the confirmed base58 address.
pub fn verify_binding_token(token: &str, code: &str) -> Result<String> {
    let raw = bs58::decode(token.trim())
        .into_vec()
        .map_err(|_| anyhow!("confirmation token is not valid base58"))?;
    anyhow::ensure!(
        raw.len() == 96,
        "confirmation token has the wrong length (expected address + signature)"
    );
    let address: Pubkey = raw[..32].try_into().unwrap();
    let sig: [u8; 64] = raw[32..].try_into().unwrap();

    // The signer must be a real, receivable address (on-curve), not a PDA.
    anyhow::ensure!(
        is_on_curve(&address),
        "the connected address is off-curve and cannot receive payments"
    );

    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let vk = VerifyingKey::from_bytes(&address)
        .map_err(|_| anyhow!("the connected address is not a valid ed25519 key"))?;
    vk.verify(&binding_message(code), &Signature::from_bytes(&sig))
        .map_err(|_| anyhow!("signature does not match the connected address (or the code)"))?;
    Ok(pubkey_str(&address))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solana::Keypair;

    /// A guaranteed on-curve base58 address: a real ed25519 public key.
    fn oncurve() -> String {
        pubkey_str(&Keypair::from_seed([1u8; 32]).pubkey)
    }

    #[test]
    fn validate_accepts_on_curve_rejects_junk() {
        let addr = oncurve();
        assert!(validate_payout_address(&addr).is_ok());
        // Not base58 / wrong length.
        assert!(validate_payout_address("not an address").is_err());
        assert!(validate_payout_address("").is_err());
        // Whitespace is tolerated.
        assert!(validate_payout_address(&format!("  {addr}  ")).is_ok());
    }

    #[test]
    fn validate_rejects_off_curve_pda() {
        // A program-derived address is off-curve by construction — must be rejected.
        let (pda, _bump) = crate::solana::find_program_address(
            &[b"any-seed"],
            &Keypair::from_seed([2u8; 32]).pubkey,
        );
        assert!(
            validate_payout_address(&pubkey_str(&pda)).is_err(),
            "an off-curve PDA must be rejected as a payout destination"
        );
    }

    #[test]
    fn binding_token_round_trips_and_rejects_tampering() {
        let seed = [7u8; 32];
        let kp = Keypair::from_seed(seed);
        let code = "test-code-123";
        let sig = kp.sign_message(&binding_message(code));

        // Well-formed token verifies to the signer's address.
        let mut raw = Vec::new();
        raw.extend_from_slice(&kp.pubkey);
        raw.extend_from_slice(&sig);
        let token = bs58::encode(&raw).into_string();
        assert_eq!(
            verify_binding_token(&token, code).unwrap(),
            pubkey_str(&kp.pubkey)
        );

        // A different code → signature no longer matches (replay/session guard).
        assert!(verify_binding_token(&token, "other-code").is_err());

        // Flip a signature byte → rejected.
        let mut bad = raw.clone();
        bad[40] ^= 0xff;
        assert!(verify_binding_token(&bs58::encode(&bad).into_string(), code).is_err());

        // Wrong length / not base58 → rejected, no panic.
        assert!(verify_binding_token("short", code).is_err());
        assert!(verify_binding_token("!!!!", code).is_err());
    }

    #[test]
    fn binding_domain_is_versioned_and_distinct() {
        assert_eq!(PAYOUT_BIND_DOMAIN, b"cloudiy/payout-bind/v1");
        // Must not collide with the other domains.
        assert_ne!(PAYOUT_BIND_DOMAIN, crate::solana::RESULT_DOMAIN);
        assert_ne!(PAYOUT_BIND_DOMAIN, crate::volume::VOLUME_KEY_DOMAIN);
    }
}
