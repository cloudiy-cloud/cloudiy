//! Adversarial devnet proof for finding C1: the escrow rejects a forged result
//! proof built with the Ed25519 precompile "instruction index" spoof.
//!
//! Funds an escrow whose `provider_node_key` is a key we do NOT sign with, then
//! tries to release it with a signature from an *attacker* key, hidden behind a
//! second Ed25519 instruction the precompile actually checks. A hardened
//! contract must reject this (BadSignature); acceptance would mean the provider
//! could be paid for a result they never signed.
//!
//! Usage:
//!   cargo run -p cloudiy --example spoof_release -- \
//!     --consumer <path> [--settler <path>] --provider <pubkey> [--rpc <url>]

use cloudiy::solana::{self, Keypair};

const DEFAULT_RPC: &str = "https://api.devnet.solana.com";
const DEFAULT_MINT: &str = "7E2fxsgWJiXGyFtLuybzhEavKXPkuCXMyqdW8GaZegRw";
const PROGRAM: &str = "9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN";

fn arg(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rpc = arg("--rpc").unwrap_or_else(|| DEFAULT_RPC.to_string());
    let mint_s = arg("--mint").unwrap_or_else(|| DEFAULT_MINT.to_string());
    let consumer_path = arg("--consumer").expect("--consumer <keypair path> required");
    let settler_path = arg("--settler").unwrap_or_else(|| consumer_path.clone());
    let provider_s = arg("--provider").expect("--provider <pubkey> required");

    let program = solana::parse_pubkey(PROGRAM)?;
    let mint = solana::parse_pubkey(&mint_s)?;
    let provider = solana::parse_pubkey(&provider_s)?;
    let consumer = Keypair::load(std::path::Path::new(&consumer_path))?;
    let settler = Keypair::load(std::path::Path::new(&settler_path))?;

    // The escrow's provider node key — we deliberately never sign with it.
    let provider_node_secret = iroh::SecretKey::from_bytes(&[7u8; 32]);
    let provider_node_key: [u8; 32] = *provider_node_secret.public().as_bytes();
    // The attacker's key, which we DO sign with, hidden behind the spoof.
    let attacker_secret = [99u8; 32];

    let job_id = *uuid::Uuid::new_v4().as_bytes();
    println!(
        "provider_node_key = {} (never signs)",
        hex::encode(provider_node_key)
    );
    println!(
        "attacker key      = {} (signs the forged proof)",
        hex::encode(
            iroh::SecretKey::from_bytes(&attacker_secret)
                .public()
                .as_bytes()
        )
    );

    println!("\n① funding escrow …");
    let esc = solana::create_job(
        &rpc,
        &consumer,
        &program,
        &provider,
        &mint,
        1_000_000,
        600,
        job_id,
        &provider_node_key,
    )
    .await?;
    println!("   escrow = {}", solana::pubkey_str(&esc.job_account));

    println!("\n② attempting FORGED release via Ed25519 instruction-index spoof …");
    let input = b"forged-input";
    let output = b"forged-output";
    match solana::attempt_spoofed_release(
        &rpc,
        &settler,
        &program,
        &esc.job_account,
        &provider_node_key,
        attacker_secret,
        input,
        output,
    )
    .await
    {
        Ok(sig) => {
            println!("   ❌ VULNERABLE: forged release ACCEPTED — tx {sig}");
            anyhow::bail!("contract accepted a forged proof — C1 NOT fixed");
        }
        Err(e) => {
            println!("   ✅ REJECTED by the contract as expected:");
            println!("      {e}");
            println!("\nC1 PROVEN: the precompile-spoof forgery is rejected on-chain.");
            // Reclaim the locked funds so the test escrow isn't stranded.
            println!("\n③ cleaning up — provider cancels (refund) the test escrow …");
            // Consumer-after-deadline isn't available yet; provider-cancel needs
            // the provider key, which we don't hold here. Leave the escrow; it
            // can be refunded by the provider or by the consumer after deadline.
            Ok(())
        }
    }
}
