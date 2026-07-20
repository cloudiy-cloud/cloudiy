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
use sha2::{Digest, Sha256};

pub struct EscrowJob {
    pub job_id: [u8; 16],
    pub consumer: [u8; 32],
    pub provider: [u8; 32],
    pub mint: [u8; 32],
    pub amount: u64,
    pub deadline: i64,
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
    let consumer: [u8; 32] = data[o..o + 32].try_into().unwrap();
    o += 32;
    let provider: [u8; 32] = data[o..o + 32].try_into().unwrap();
    o += 32;
    let mint: [u8; 32] = data[o..o + 32].try_into().unwrap();
    o += 32;
    let amount = u64::from_le_bytes(data[o..o + 8].try_into().unwrap());
    o += 8;
    let deadline = i64::from_le_bytes(data[o..o + 8].try_into().unwrap());
    o += 8;
    let state = data[o];
    Ok(EscrowJob {
        job_id,
        consumer,
        provider,
        mint,
        amount,
        deadline,
        state,
    })
}

/// Longest a run-auth signature stays valid past the consumer's chosen expiry
/// floor — bounds how long a captured signature could be replayed even if the
/// consumer set an absurdly distant expiry (MEDIUM-2 hardening).
pub const RUN_AUTH_MAX_WINDOW_SECS: i64 = 3600;

/// A2: does the escrow leave enough runway before its deadline to finish and be
/// released before the consumer could refund? Pure so the boundary is testable
/// without a live RPC (the surrounding `verify_escrow` needs one). `Ok` = enough
/// margin; `Err` carries the consumer-facing reason.
pub fn deadline_has_margin(deadline: i64, now: i64, min_remaining_secs: i64) -> Result<(), String> {
    if deadline - now < min_remaining_secs {
        return Err(format!(
            "escrow deadline too near ({}s left, need {}s) — refund risk",
            deadline - now,
            min_remaining_secs
        ));
    }
    Ok(())
}

/// MEDIUM-2 freshness: reject a run authorization that has already lapsed, or
/// whose expiry is set absurdly far out (which would widen the replay window
/// regardless of the deadline). Pure and testable.
pub fn auth_within_window(auth_expiry: i64, now: i64) -> Result<(), String> {
    if auth_expiry < now {
        return Err("run authorization has expired".to_string());
    }
    if auth_expiry.saturating_sub(now) > RUN_AUTH_MAX_WINDOW_SECS {
        return Err("run authorization expiry is too far in the future".to_string());
    }
    Ok(())
}

/// Domain-separated message the escrow's consumer signs to authorize a run
/// against their funded escrow. Binds the runner to the escrow owner (A4), the
/// exact `input` (RFC-0006 §4, so a captured sig can't be replayed against a
/// different prompt), and — as of `v3` — an `expiry` (unix seconds) so the
/// authorization can't be replayed after it lapses or after a `job_id` is
/// recycled via close+reuse (MEDIUM-2).
pub fn run_auth_message(job_id_bytes: &[u8; 16], input: &[u8], expiry_unix: i64) -> Vec<u8> {
    let input_hash = Sha256::digest(input);
    let mut m = Vec::with_capacity(24 + 16 + 1 + 32 + 1 + 8);
    m.extend_from_slice(b"cloudiy/escrow-run/v3");
    m.push(0);
    m.extend_from_slice(job_id_bytes);
    m.push(0);
    m.extend_from_slice(&input_hash);
    m.push(0);
    m.extend_from_slice(&expiry_unix.to_be_bytes());
    m
}

/// Fetch and verify an escrow `Job` account. `Ok(amount)` means USDC is locked
/// on-chain for *this exact job*, payable to *this provider*, in the expected
/// mint, at >= `min_amount` micro-USDC, not yet released/refunded, with at
/// least `min_remaining_secs` before its deadline, and authorized by a
/// signature from the escrow's own consumer key. The returned `u64` is the
/// funded amount (used to size a VM's prepaid compute budget).
#[allow(clippy::too_many_arguments)]
pub async fn verify_escrow(
    rpc_url: &str,
    escrow_account: &str,
    program_id: &str,
    expected_provider: &str,
    expected_mint: &str,
    min_amount: u64,
    job_id_bytes: [u8; 16],
    min_remaining_secs: i64,
    consumer_sig_hex: Option<&str>,
    input: &[u8],
    auth_expiry: i64,
    now: i64,
) -> Result<u64, String> {
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

    let owner = acc
        .get("owner")
        .and_then(|o| o.as_str())
        .unwrap_or_default();
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
    // A2: refuse escrows too close to (or past) their deadline — otherwise the
    // consumer could run, then refund after the deadline, getting free work.
    deadline_has_margin(job.deadline, now, min_remaining_secs)?;
    // A4: the run must be authorized by the escrow's own consumer key, so a
    // third party can't spend someone else's funded escrow.
    let sig_hex = consumer_sig_hex.ok_or("missing consumer authorization signature")?;
    let sig_bytes = hex::decode(sig_hex).map_err(|_| "consumer sig is not hex".to_string())?;
    let sig = iroh::Signature::try_from(sig_bytes.as_slice())
        .map_err(|_| "consumer sig is not 64 bytes".to_string())?;
    let consumer_key = iroh::EndpointId::from_bytes(&job.consumer)
        .map_err(|_| "escrow consumer is not a valid ed25519 key".to_string())?;
    // Freshness (MEDIUM-2): reject a lapsed authorization, and refuse one whose
    // expiry is set absurdly far out (bounds the replay window regardless).
    auth_within_window(auth_expiry, now)?;
    // Verifying over `run_auth_message(job_id, input, expiry)` also enforces
    // input integrity (the sig only validates if `input` hashes to what the
    // consumer signed) and binds the authorization to this time window.
    consumer_key
        .verify(&run_auth_message(&job_id_bytes, input, auth_expiry), &sig)
        .map_err(|_| "consumer authorization signature is invalid".to_string())?;
    Ok(job.amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_margin_boundary() {
        let now = 1_000_000;
        let need = 420; // WORKLOAD_TIMEOUT_SECS + 120
                        // Exactly enough runway passes; one second short fails.
        assert!(deadline_has_margin(now + need, now, need).is_ok());
        assert!(deadline_has_margin(now + need - 1, now, need).is_err());
        // A deadline already in the past fails (negative runway).
        let err = deadline_has_margin(now - 10, now, need).unwrap_err();
        assert!(err.contains("deadline too near"), "{err}");
        assert!(err.contains("need 420s"), "{err}");
    }

    #[test]
    fn auth_window_bounds() {
        let now = 1_000_000;
        // A fresh, in-window expiry (the client uses now+900) passes.
        assert!(auth_within_window(now + 900, now).is_ok());
        // Exactly at the cap passes; one second past it fails.
        assert!(auth_within_window(now + RUN_AUTH_MAX_WINDOW_SECS, now).is_ok());
        assert_eq!(
            auth_within_window(now + RUN_AUTH_MAX_WINDOW_SECS + 1, now).unwrap_err(),
            "run authorization expiry is too far in the future"
        );
        // A lapsed authorization fails.
        assert_eq!(
            auth_within_window(now - 1, now).unwrap_err(),
            "run authorization has expired"
        );
        // Exactly at `now` is still valid (not yet lapsed).
        assert!(auth_within_window(now, now).is_ok());
    }

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
        // deadline is written below (kept 0 here); consumer is fixed [7;32].
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
        assert_eq!(job.consumer, [7u8; 32]);
        assert_eq!(job.deadline, 0);
    }

    #[test]
    fn rejects_truncated_account() {
        assert!(parse_job(&[0u8; 20]).is_err());
    }

    #[test]
    fn run_auth_signature_binds_to_the_consumer_key() {
        // A4: a signature over the run-auth message by the escrow's consumer
        // key verifies; the same message signed by a different key does not.
        let job_id = [9u8; 16];
        let input = b"the consumer's exact prompt";
        let expiry = 1_800_000_900_i64;
        let msg = run_auth_message(&job_id, input, expiry);

        let consumer = iroh::SecretKey::from_bytes(&[3u8; 32]);
        let attacker = iroh::SecretKey::from_bytes(&[4u8; 32]);
        let good = consumer.sign(&msg);
        let bad = attacker.sign(&msg);

        let key = consumer.public();
        assert!(key.verify(&msg, &good).is_ok());
        assert!(key.verify(&msg, &bad).is_err());
        // A different job id must not validate under the same signature.
        assert!(key
            .verify(&run_auth_message(&[8u8; 16], input, expiry), &good)
            .is_err());
        // RFC-0006 §4: a different input must not validate either — binds the
        // run authorization to the exact prompt.
        assert!(key
            .verify(
                &run_auth_message(&job_id, b"a swapped prompt", expiry),
                &good
            )
            .is_err());
        // MEDIUM-2: a different expiry must not validate — binds the time window.
        assert!(key
            .verify(&run_auth_message(&job_id, input, expiry + 1), &good)
            .is_err());
    }
}
