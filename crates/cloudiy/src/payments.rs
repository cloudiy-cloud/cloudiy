//! On-chain payment verification — confirm a funded, Active escrow `Job`
//! account exists for this job before executing. Deliberately lightweight:
//! Solana JSON-RPC (`getAccountInfo`) over HTTP + fixed-layout byte parsing,
//! so the node needs neither `solana-sdk` nor `borsh`. This turns the x402
//! quote from a promise into an enforced condition — a provider running with
//! `--require-payment` executes only against real locked USDC.
//!
//! Anchor `Job` layout (see contracts/programs/cloudiy-escrow): after the
//! 8-byte account discriminator — job_id [u8;16], consumer Pubkey, provider
//! Pubkey, mint Pubkey, amount u64 (LE), deadline i64, state u8, bump u8.

use base64::Engine;
use serde_json::json;

pub struct EscrowJob {
    pub job_id: [u8; 16],
    pub provider: [u8; 32],
    pub mint: [u8; 32],
    pub amount: u64,
    /// 0 = Active, 1 = Released, 2 = Refunded.
    pub state: u8,
}

const DISCRIMINATOR: usize = 8;
const JOB_LEN: usize = DISCRIMINATOR + 16 + 32 + 32 + 32 + 8 + 8 + 1 + 1;

/// Parse the raw account data of an Anchor `Job`.
pub fn parse_job(data: &[u8]) -> Result<EscrowJob, String> {
    if data.len() < JOB_LEN {
        return Err("escrow account too small to be a Job".into());
    }
    let mut o = DISCRIMINATOR;
    let job_id: [u8; 16] = data[o..o + 16].try_into().unwrap();
    o += 16;
    o += 32; // consumer (unused for verification)
    let provider: [u8; 32] = data[o..o + 32].try_into().unwrap();
    o += 32;
    let mint: [u8; 32] = data[o..o + 32].try_into().unwrap();
    o += 32;
    let amount = u64::from_le_bytes(data[o..o + 8].try_into().unwrap());
    o += 8;
    o += 8; // deadline
    let state = data[o];
    Ok(EscrowJob {
        job_id,
        provider,
        mint,
        amount,
        state,
    })
}

/// Fetch and verify an escrow `Job` account. `Ok(())` means USDC is locked
/// on-chain for *this exact job*, payable to *this provider*, in the expected
/// mint, at >= `min_amount` micro-USDC, and not yet released/refunded.
#[allow(clippy::too_many_arguments)]
pub async fn verify_escrow(
    rpc_url: &str,
    escrow_account: &str,
    program_id: &str,
    expected_provider: &str,
    expected_mint: &str,
    min_amount: u64,
    job_id_bytes: [u8; 16],
) -> Result<(), String> {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "getAccountInfo",
        "params": [escrow_account, {"encoding": "base64", "commitment": "confirmed"}]
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("rpc client: {e}"))?;
    let v: serde_json::Value = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("rpc request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("rpc decode failed: {e}"))?;

    let acc = v.pointer("/result/value");
    match acc {
        None | Some(serde_json::Value::Null) => {
            return Err("escrow account does not exist on-chain".into())
        }
        _ => {}
    }
    let acc = acc.unwrap();

    let owner = acc.get("owner").and_then(|o| o.as_str()).unwrap_or_default();
    if owner != program_id {
        return Err("account is not owned by the escrow program".into());
    }

    let data_b64 = acc
        .pointer("/data/0")
        .and_then(|s| s.as_str())
        .ok_or("escrow account has no base64 data")?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|_| "escrow data is not valid base64".to_string())?;
    let job = parse_job(&raw)?;

    if job.state != 0 {
        return Err("escrow is not Active (already released or refunded)".into());
    }
    if job.job_id != job_id_bytes {
        return Err("escrow job_id does not match this job".into());
    }
    if bs58::encode(job.provider).into_string() != expected_provider {
        return Err("escrow pays a different provider".into());
    }
    if bs58::encode(job.mint).into_string() != expected_mint {
        return Err("escrow is funded in the wrong token (mint mismatch)".into());
    }
    if job.amount < min_amount {
        return Err(format!(
            "escrow underfunded: {} < {} micro-USDC",
            job.amount, min_amount
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_job(
        job_id: [u8; 16],
        provider: [u8; 32],
        mint: [u8; 32],
        amount: u64,
        state: u8,
    ) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&[0u8; 8]); // discriminator
        d.extend_from_slice(&job_id);
        d.extend_from_slice(&[7u8; 32]); // consumer
        d.extend_from_slice(&provider);
        d.extend_from_slice(&mint);
        d.extend_from_slice(&amount.to_le_bytes());
        d.extend_from_slice(&0i64.to_le_bytes()); // deadline
        d.push(state);
        d.push(255); // bump
        d
    }

    #[test]
    fn parses_a_well_formed_job() {
        let raw = synthetic_job([1u8; 16], [2u8; 32], [3u8; 32], 12_345, 0);
        let job = parse_job(&raw).unwrap();
        assert_eq!(job.job_id, [1u8; 16]);
        assert_eq!(job.provider, [2u8; 32]);
        assert_eq!(job.mint, [3u8; 32]);
        assert_eq!(job.amount, 12_345);
        assert_eq!(job.state, 0);
    }

    #[test]
    fn rejects_truncated_account() {
        assert!(parse_job(&[0u8; 20]).is_err());
    }
}
