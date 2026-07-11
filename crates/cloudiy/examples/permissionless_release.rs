//! Live devnet proof for audit finding A3: `release_verified` is permissionless.
//!
//! Funds a fresh escrow as the consumer, produces a valid result signature with
//! a provider node key we control, then settles it with a *settler* keypair —
//! which may differ from the consumer. A successful settle by a non-consumer
//! proves the on-chain program no longer requires the consumer's signature and
//! that the redeployed 8-account layout (payer + separate consumer slot) is live.
//!
//! Usage:
//!   cargo run -p cloudiy --example permissionless_release -- \
//!     --consumer <path> --settler <path> [--rpc <url>] [--mint <pubkey>] \
//!     --provider <pubkey> [--amount <micro>]

use cloudiy::solana::{self, Keypair};
use sha2::{Digest, Sha256};

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
    let amount: u64 = arg("--amount")
        .map(|s| s.parse().unwrap())
        .unwrap_or(1_000_000);

    let program = solana::parse_pubkey(PROGRAM)?;
    let mint = solana::parse_pubkey(&mint_s)?;
    let provider = solana::parse_pubkey(&provider_s)?;
    let consumer = Keypair::load(std::path::Path::new(&consumer_path))?;
    let settler = Keypair::load(std::path::Path::new(&settler_path))?;

    // A provider node key we hold, so we can produce a genuine result signature.
    let node_secret = iroh::SecretKey::from_bytes(&[42u8; 32]);
    let node_pubkey: [u8; 32] = *node_secret.public().as_bytes();

    let job_id = *uuid::Uuid::new_v4().as_bytes();
    let job_uuid = uuid::Uuid::from_bytes(job_id).to_string();
    println!("consumer  = {}", solana::pubkey_str(&consumer.pubkey));
    println!("settler   = {}", solana::pubkey_str(&settler.pubkey));
    println!("provider  = {}", solana::pubkey_str(&provider));
    println!("job_id    = {job_uuid}");
    println!("permissionless = {}", consumer.pubkey != settler.pubkey);

    println!("\n① funding escrow (consumer signs) …");
    let esc = solana::create_job(
        &rpc,
        &consumer,
        &program,
        &provider,
        &mint,
        amount,
        600,
        job_id,
        &node_pubkey,
    )
    .await?;
    println!(
        "   escrow = {}  tx {}",
        solana::pubkey_str(&esc.job_account),
        esc.signature
    );

    // Craft a signed result exactly as a provider node would (v2: input-bound).
    let input = b"permissionless-proof-input";
    let output = b"permissionless-proof-output";
    let input_hash: [u8; 32] = Sha256::digest(input).into();
    let output_hash: [u8; 32] = Sha256::digest(output).into();
    let mut message = Vec::new();
    message.extend_from_slice(solana::RESULT_DOMAIN);
    message.push(0);
    message.extend_from_slice(job_uuid.as_bytes());
    message.push(0);
    message.extend_from_slice(&input_hash);
    message.push(0);
    message.extend_from_slice(&output_hash);
    let signature: [u8; 64] = node_secret.sign(&message).to_bytes();

    println!("\n② settling via release_verified (settler signs, NOT the consumer) …");
    let r = solana::release_verified(
        &rpc,
        &settler,
        &program,
        &esc.job_account,
        &node_pubkey,
        input,
        output,
        &signature,
    )
    .await?;
    println!(
        "   ✅ released — provider {} got {} micro-USDC (fee {})",
        solana::pubkey_str(&r.provider),
        r.payout,
        r.fee
    );
    println!("   tx {}", r.signature);
    if consumer.pubkey != settler.pubkey {
        println!(
            "\nA3 PROVEN: a non-consumer settler released the escrow with a valid result proof."
        );
    } else {
        println!(
            "\nRedeploy + new 8-account layout confirmed on-chain (settler == consumer here)."
        );
        println!(
            "Re-run with a funded --settler distinct from --consumer for the permissionless proof."
        );
    }
    Ok(())
}
