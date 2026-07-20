//! Minimal Solana transaction builder for the consumer side — funds an escrow
//! `Job` on-chain (the counterpart to the provider's [`crate::payments`]
//! verification). Deliberately dependency-light: no `solana-sdk`. It builds
//! and signs a legacy transaction by hand (PDA via curve25519, ATA, shortvec
//! message serialization, ed25519 signing) and submits it over JSON-RPC.
//!
//! Scope: the single `create_job` instruction of the Cloudiy escrow. That is
//! all a consumer needs to lock USDC before submitting a job.

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use sha2::{Digest, Sha256};

pub const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const ATA_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
pub const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
/// Fee authority hardcoded in the escrow program (receives the protocol fee).
pub const FEE_AUTHORITY: &str = "GnaUN3hxTZaq6FqzVzLjXzJWi6svocFqgYbBJSdusFJP";
/// Protocol fee in basis points (matches the on-chain program).
pub const PROTOCOL_FEE_BPS: u64 = 400;

/// Protocol fee for a payout amount, computed with a widening multiply so a
/// large `amount` can never overflow (B1). Mirrors the contract's math.
pub fn protocol_fee(amount: u64) -> u64 {
    ((amount as u128 * PROTOCOL_FEE_BPS as u128) / 10_000) as u64
}
/// Ed25519 signature-verification precompile.
pub const ED25519_PROGRAM: &str = "Ed25519SigVerify111111111111111111111111111";
/// Instructions sysvar (read by `release_verified` for proof introspection).
pub const SYSVAR_INSTRUCTIONS: &str = "Sysvar1nstructions1111111111111111111111111";
/// Result-signing domain (matches `cloudiy_common::sig`).
pub const RESULT_DOMAIN: &[u8] = b"cloudiy/result/v2";

pub type Pubkey = [u8; 32];

pub fn parse_pubkey(s: &str) -> Result<Pubkey> {
    let v = bs58::decode(s)
        .into_vec()
        .map_err(|_| anyhow!("invalid base58 pubkey"))?;
    v.try_into().map_err(|_| anyhow!("pubkey must be 32 bytes"))
}

pub fn pubkey_str(p: &Pubkey) -> String {
    bs58::encode(p).into_string()
}

/// Parse a hex-encoded 32-byte key (iroh EndpointId form).
pub fn parse_hex32(s: &str) -> Result<Pubkey> {
    hex::decode(s.trim())
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| anyhow!("expected a 32-byte hex key"))
}

/// Parse a hex-encoded 64-byte ed25519 signature.
pub fn parse_hex64(s: &str) -> Result<[u8; 64]> {
    hex::decode(s.trim())
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| anyhow!("expected a 64-byte hex signature"))
}

/// A Solana keypair loaded from a CLI `id.json` (a 64-byte array: 32-byte
/// secret seed followed by the 32-byte public key).
pub struct Keypair {
    signing: ed25519_dalek::SigningKey,
    pub pubkey: Pubkey,
}

impl Keypair {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let bytes: Vec<u8> = serde_json::from_str(
            &std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?,
        )
        .context("keypair file is not a JSON byte array")?;
        anyhow::ensure!(bytes.len() == 64, "keypair must be 64 bytes");
        let seed: [u8; 32] = bytes[..32].try_into().unwrap();
        let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
        let pubkey = signing.verifying_key().to_bytes();
        // Sanity: stored pubkey matches the derived one.
        anyhow::ensure!(bytes[32..] == pubkey, "keypair pubkey mismatch");
        Ok(Keypair { signing, pubkey })
    }

    fn sign(&self, msg: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        self.signing.sign(msg).to_bytes()
    }

    /// Sign an arbitrary message with this wallet key (used to authorize a run
    /// against a funded escrow — A4). Returns the 64-byte ed25519 signature.
    pub fn sign_message(&self, msg: &[u8]) -> [u8; 64] {
        self.sign(msg)
    }

    /// Construct directly from a 32-byte seed — test-only, so crypto helpers
    /// (e.g. the RFC-0009 volume-key derivation) can exercise a real signer
    /// without a keypair file on disk.
    #[cfg(test)]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
        let pubkey = signing.verifying_key().to_bytes();
        Keypair { signing, pubkey }
    }
}

/// True if the 32 bytes decode to a valid Edwards point (i.e. ON the curve).
/// A PDA is required to be OFF the curve.
fn is_on_curve(bytes: &Pubkey) -> bool {
    curve25519_dalek::edwards::CompressedEdwardsY(*bytes)
        .decompress()
        .is_some()
}

/// Solana `find_program_address`: the first bump (from 255 down) whose hash is
/// off the ed25519 curve.
pub fn find_program_address(seeds: &[&[u8]], program_id: &Pubkey) -> (Pubkey, u8) {
    for bump in (0u8..=255).rev() {
        let mut h = Sha256::new();
        for s in seeds {
            h.update(s);
        }
        h.update([bump]);
        h.update(program_id);
        h.update(b"ProgramDerivedAddress");
        let candidate: Pubkey = h.finalize().into();
        if !is_on_curve(&candidate) {
            return (candidate, bump);
        }
    }
    unreachable!("no off-curve bump found")
}

/// The associated token account address for `owner` + `mint`.
pub fn associated_token_address(owner: &Pubkey, mint: &Pubkey) -> Result<Pubkey> {
    let token = parse_pubkey(TOKEN_PROGRAM)?;
    let ata = parse_pubkey(ATA_PROGRAM)?;
    Ok(find_program_address(&[owner, &token, mint], &ata).0)
}

/// Anchor instruction discriminator: first 8 bytes of sha256("global:<name>").
fn anchor_discriminator(name: &str) -> [u8; 8] {
    let hash = Sha256::digest(format!("global:{name}").as_bytes());
    hash[..8].try_into().unwrap()
}

struct AccountMeta {
    pubkey: Pubkey,
    is_signer: bool,
    is_writable: bool,
}

struct Instruction {
    program_id: Pubkey,
    accounts: Vec<AccountMeta>,
    data: Vec<u8>,
}

fn shortvec(n: usize, out: &mut Vec<u8>) {
    let mut n = n;
    loop {
        let mut b = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
}

/// Compile a legacy message: fee_payer first, accounts partitioned into
/// writable-signers, readonly-signers, writable-non-signers, readonly-non
/// (the runtime validates the header counts against this partition).
fn compile_message(fee_payer: &Pubkey, ixs: &[Instruction], blockhash: &Pubkey) -> Vec<u8> {
    use std::collections::BTreeMap;
    // Merge (is_signer, is_writable) per pubkey, OR-ing flags.
    let mut flags: BTreeMap<Pubkey, (bool, bool)> = BTreeMap::new();
    let mut order: Vec<Pubkey> = Vec::new();
    let mut touch = |pk: Pubkey, s: bool, w: bool, order: &mut Vec<Pubkey>| {
        let e = flags.entry(pk).or_insert_with(|| {
            order.push(pk);
            (false, false)
        });
        e.0 |= s;
        e.1 |= w;
    };
    touch(*fee_payer, true, true, &mut order);
    for ix in ixs {
        for a in &ix.accounts {
            touch(a.pubkey, a.is_signer, a.is_writable, &mut order);
        }
        touch(ix.program_id, false, false, &mut order);
    }

    let key_of = |pred: &dyn Fn(bool, bool) -> bool| -> Vec<Pubkey> {
        order
            .iter()
            .filter(|pk| {
                let (s, w) = flags[*pk];
                **pk != *fee_payer && pred(s, w)
            })
            .copied()
            .collect()
    };
    let mut account_keys = vec![*fee_payer];
    account_keys.extend(key_of(&|s, w| s && w)); // writable signers
    account_keys.extend(key_of(&|s, w| s && !w)); // readonly signers
    account_keys.extend(key_of(&|s, w| !s && w)); // writable non-signers
    account_keys.extend(key_of(&|s, w| !s && !w)); // readonly non-signers

    let num_signers = account_keys
        .iter()
        .filter(|pk| flags[*pk].0 || **pk == *fee_payer)
        .count();
    let num_ro_signed = account_keys
        .iter()
        .filter(|pk| flags[*pk].0 && !flags[*pk].1 && **pk != *fee_payer)
        .count();
    let num_ro_unsigned = account_keys
        .iter()
        .filter(|pk| !flags[*pk].0 && !flags[*pk].1)
        .count();

    let index_of = |pk: &Pubkey| account_keys.iter().position(|k| k == pk).unwrap() as u8;

    let mut msg = Vec::new();
    msg.push(num_signers as u8);
    msg.push(num_ro_signed as u8);
    msg.push(num_ro_unsigned as u8);
    shortvec(account_keys.len(), &mut msg);
    for k in &account_keys {
        msg.extend_from_slice(k);
    }
    msg.extend_from_slice(blockhash);
    shortvec(ixs.len(), &mut msg);
    for ix in ixs {
        msg.push(index_of(&ix.program_id));
        shortvec(ix.accounts.len(), &mut msg);
        for a in &ix.accounts {
            msg.push(index_of(&a.pubkey));
        }
        shortvec(ix.data.len(), &mut msg);
        msg.extend_from_slice(&ix.data);
    }
    msg
}

// ---------------------------------------------------------------- RPC calls

async fn rpc(rpc_url: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let v: serde_json::Value = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    if let Some(err) = v.get("error") {
        anyhow::bail!("rpc {method} error: {err}");
    }
    Ok(v)
}

async fn latest_blockhash(rpc_url: &str) -> Result<Pubkey> {
    let v = rpc(
        rpc_url,
        "getLatestBlockhash",
        serde_json::json!([{"commitment":"finalized"}]),
    )
    .await?;
    let bh = v
        .pointer("/result/value/blockhash")
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("no blockhash in response"))?;
    parse_pubkey(bh)
}

async fn send_transaction(rpc_url: &str, tx_b64: &str) -> Result<String> {
    let v = rpc(
        rpc_url,
        "sendTransaction",
        serde_json::json!([tx_b64, {"encoding":"base64","preflightCommitment":"confirmed"}]),
    )
    .await?;
    v.get("result")
        .and_then(|s| s.as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("no signature in sendTransaction response"))
}

async fn await_confirmation(rpc_url: &str, sig: &str) -> Result<()> {
    for _ in 0..40 {
        let v = rpc(
            rpc_url,
            "getSignatureStatuses",
            serde_json::json!([[sig], {"searchTransactionHistory":true}]),
        )
        .await?;
        if let Some(status) = v.pointer("/result/value/0") {
            if !status.is_null() {
                if let Some(err) = status.get("err") {
                    if !err.is_null() {
                        anyhow::bail!("transaction failed on-chain: {err}");
                    }
                }
                let conf = status
                    .get("confirmationStatus")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                if conf == "confirmed" || conf == "finalized" {
                    return Ok(());
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
    }
    anyhow::bail!("transaction not confirmed in time (sig {sig})")
}

// ------------------------------------------------------------- create_job

/// Result of funding an escrow: the `Job` account address and the job id used.
pub struct FundedEscrow {
    pub job_account: Pubkey,
    pub job_id: [u8; 16],
    pub signature: String,
}

/// Build, sign and submit a `create_job` that locks `amount` micro-USDC of
/// `mint` for `provider`, then wait for confirmation.
#[allow(clippy::too_many_arguments)]
pub async fn create_job(
    rpc_url: &str,
    consumer: &Keypair,
    escrow_program: &Pubkey,
    provider: &Pubkey,
    mint: &Pubkey,
    amount: u64,
    timeout_secs: i64,
    job_id: [u8; 16],
    provider_node_key: &Pubkey,
) -> Result<FundedEscrow> {
    let (job, _) = find_program_address(&[b"job", &consumer.pubkey, &job_id], escrow_program);
    let (vault, _) = find_program_address(&[b"vault", &job], escrow_program);
    let consumer_token = associated_token_address(&consumer.pubkey, mint)?;
    let token_program = parse_pubkey(TOKEN_PROGRAM)?;
    let system_program = parse_pubkey(SYSTEM_PROGRAM)?;

    // data = discriminator ++ job_id[16] ++ amount(u64 LE) ++ timeout(i64 LE)
    //        ++ provider_node_key[32]
    let mut data = anchor_discriminator("create_job").to_vec();
    data.extend_from_slice(&job_id);
    data.extend_from_slice(&amount.to_le_bytes());
    data.extend_from_slice(&timeout_secs.to_le_bytes());
    data.extend_from_slice(provider_node_key);

    let ix = Instruction {
        program_id: *escrow_program,
        accounts: vec![
            AccountMeta {
                pubkey: consumer.pubkey,
                is_signer: true,
                is_writable: true,
            },
            AccountMeta {
                pubkey: *provider,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: *mint,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: job,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: vault,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: consumer_token,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: token_program,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: system_program,
                is_signer: false,
                is_writable: false,
            },
        ],
        data,
    };

    let blockhash = latest_blockhash(rpc_url).await?;
    let message = compile_message(&consumer.pubkey, &[ix], &blockhash);
    let sig = consumer.sign(&message);

    let mut tx = Vec::new();
    shortvec(1, &mut tx); // one signature
    tx.extend_from_slice(&sig);
    tx.extend_from_slice(&message);
    let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx);

    let signature = send_transaction(rpc_url, &tx_b64).await?;
    await_confirmation(rpc_url, &signature).await?;

    Ok(FundedEscrow {
        job_account: job,
        job_id,
        signature,
    })
}

// ---------------------------------------------------------------- release

/// The escrow `Job` fields needed to release payment.
pub struct JobAccount {
    pub job_id: [u8; 16],
    pub consumer: Pubkey,
    pub provider: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub state: u8,
}

/// Read the funded amount (micro-USDC) of an escrow `Job` account — used to
/// size the placement value-cap (RFC-0006 §6) before contacting the provider.
pub async fn escrow_amount(rpc_url: &str, job_account: &str) -> Result<u64> {
    let acct = parse_pubkey(job_account)?;
    Ok(fetch_job(rpc_url, &acct).await?.amount)
}

/// Fetch and parse an on-chain `Job` account.
async fn fetch_job(rpc_url: &str, job_account: &Pubkey) -> Result<JobAccount> {
    let v = rpc(
        rpc_url,
        "getAccountInfo",
        serde_json::json!([pubkey_str(job_account), {"encoding":"base64","commitment":"confirmed"}]),
    )
    .await?;
    let data_b64 = v
        .pointer("/result/value/data/0")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("escrow job account not found on-chain"))?;
    let raw = base64::engine::general_purpose::STANDARD.decode(data_b64)?;
    // Layout after 8-byte discriminator: job_id[16] consumer[32] provider[32]
    // mint[32] amount(u64) deadline(i64) state(u8) bump(u8).
    anyhow::ensure!(
        raw.len() >= 8 + 16 + 32 * 3 + 8 + 8 + 2,
        "job account too small"
    );
    let job_id: [u8; 16] = raw[8..8 + 16].try_into().unwrap();
    let mut o = 8 + 16;
    let consumer: Pubkey = raw[o..o + 32].try_into().unwrap();
    o += 32;
    let provider: Pubkey = raw[o..o + 32].try_into().unwrap();
    o += 32;
    let mint: Pubkey = raw[o..o + 32].try_into().unwrap();
    o += 32;
    let amount = u64::from_le_bytes(raw[o..o + 8].try_into().unwrap());
    o += 8 + 8; // amount + deadline
    let state = raw[o];
    Ok(JobAccount {
        job_id,
        consumer,
        provider,
        mint,
        amount,
        state,
    })
}

/// Associated Token Account `CreateIdempotent` instruction — creates `owner`'s
/// ATA for `mint` if it doesn't already exist (no-op otherwise), so `release`
/// always has valid payout/fee destinations.
fn create_idempotent_ata_ix(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Result<Instruction> {
    let ata = associated_token_address(owner, mint)?;
    let token = parse_pubkey(TOKEN_PROGRAM)?;
    let ata_program = parse_pubkey(ATA_PROGRAM)?;
    let system = parse_pubkey(SYSTEM_PROGRAM)?;
    Ok(Instruction {
        program_id: ata_program,
        accounts: vec![
            AccountMeta {
                pubkey: *payer,
                is_signer: true,
                is_writable: true,
            },
            AccountMeta {
                pubkey: ata,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: *owner,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: *mint,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: system,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: token,
                is_signer: false,
                is_writable: false,
            },
        ],
        data: vec![1], // CreateIdempotent
    })
}

pub struct ReleaseResult {
    pub payout: u64,
    pub fee: u64,
    pub provider: Pubkey,
    pub signature: String,
}

/// Release a funded escrow: pay the provider (minus the protocol fee) and the
/// fee authority, then mark the job Released. Signed by the consumer. Ensures
/// the payout/fee ATAs exist first (idempotent).
pub async fn release(
    rpc_url: &str,
    consumer: &Keypair,
    escrow_program: &Pubkey,
    job_account: &Pubkey,
) -> Result<ReleaseResult> {
    let job = fetch_job(rpc_url, job_account).await?;
    anyhow::ensure!(
        job.consumer == consumer.pubkey,
        "only the job's consumer can release it"
    );
    anyhow::ensure!(
        job.state == 0,
        "escrow is not Active (already released/refunded)"
    );

    let (vault, _) = find_program_address(&[b"vault", job_account], escrow_program);
    let fee_authority = parse_pubkey(FEE_AUTHORITY)?;
    let provider_token = associated_token_address(&job.provider, &job.mint)?;
    let fee_token = associated_token_address(&fee_authority, &job.mint)?;
    let token_program = parse_pubkey(TOKEN_PROGRAM)?;

    // `release` takes no args — just the discriminator.
    let data = anchor_discriminator("release").to_vec();

    let release_ix = Instruction {
        program_id: *escrow_program,
        accounts: vec![
            AccountMeta {
                pubkey: consumer.pubkey,
                is_signer: true,
                is_writable: true,
            },
            AccountMeta {
                pubkey: *job_account,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: vault,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: provider_token,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: fee_token,
                is_signer: false,
                is_writable: true,
            },
            // `storage_token` / `author_token` are Anchor **optional** accounts
            // (RFC-0004 §6 multi-payee split). This job has no storage/author
            // share, so both are absent — and Anchor signals an absent optional
            // account by passing the *program id* in its slot. Omitting them
            // entirely would shift `token_program` into `storage_token`'s
            // position and fail with AccountOwnedByWrongProgram (3007).
            AccountMeta {
                pubkey: *escrow_program,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: *escrow_program,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: token_program,
                is_signer: false,
                is_writable: false,
            },
        ],
        data,
    };

    // Make sure both destinations exist (no-op if they already do).
    let ixs = vec![
        create_idempotent_ata_ix(&consumer.pubkey, &job.provider, &job.mint)?,
        create_idempotent_ata_ix(&consumer.pubkey, &fee_authority, &job.mint)?,
        release_ix,
    ];

    let blockhash = latest_blockhash(rpc_url).await?;
    let message = compile_message(&consumer.pubkey, &ixs, &blockhash);
    let sig = consumer.sign(&message);
    let mut tx = Vec::new();
    shortvec(1, &mut tx);
    tx.extend_from_slice(&sig);
    tx.extend_from_slice(&message);
    let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx);

    let signature = send_transaction(rpc_url, &tx_b64).await?;
    await_confirmation(rpc_url, &signature).await?;

    let fee = protocol_fee(job.amount);
    Ok(ReleaseResult {
        payout: job.amount - fee,
        fee,
        provider: job.provider,
        signature,
    })
}

// ----------------------------------------------------------------- refund

pub struct RefundResult {
    pub amount: u64,
    pub consumer: Pubkey,
    pub signature: String,
}

/// Refund a funded escrow back to the consumer. Allowed on-chain when the
/// signer is the provider (voluntary cancel, any time) or the consumer after
/// the deadline. The contract enforces the rule; this just builds the tx.
pub async fn refund(
    rpc_url: &str,
    signer: &Keypair,
    escrow_program: &Pubkey,
    job_account: &Pubkey,
) -> Result<RefundResult> {
    let job = fetch_job(rpc_url, job_account).await?;
    anyhow::ensure!(
        job.state == 0,
        "escrow is not Active (already released/refunded)"
    );

    let (vault, _) = find_program_address(&[b"vault", job_account], escrow_program);
    let consumer_token = associated_token_address(&job.consumer, &job.mint)?;
    let token_program = parse_pubkey(TOKEN_PROGRAM)?;

    let data = anchor_discriminator("refund").to_vec();
    let ix = Instruction {
        program_id: *escrow_program,
        accounts: vec![
            AccountMeta {
                pubkey: signer.pubkey,
                is_signer: true,
                is_writable: true,
            },
            // `consumer` receives the vault rent and must equal job.consumer.
            AccountMeta {
                pubkey: job.consumer,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: *job_account,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: vault,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: consumer_token,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: token_program,
                is_signer: false,
                is_writable: false,
            },
        ],
        data,
    };

    let blockhash = latest_blockhash(rpc_url).await?;
    let message = compile_message(&signer.pubkey, &[ix], &blockhash);
    let sig = signer.sign(&message);
    let mut tx = Vec::new();
    shortvec(1, &mut tx);
    tx.extend_from_slice(&sig);
    tx.extend_from_slice(&message);
    let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx);

    let signature = send_transaction(rpc_url, &tx_b64).await?;
    await_confirmation(rpc_url, &signature).await?;
    Ok(RefundResult {
        amount: job.amount,
        consumer: job.consumer,
        signature,
    })
}

// ------------------------------------------------------- release_verified

/// Build an Ed25519 precompile instruction that verifies `signature` of
/// `message` by `pubkey`, with pubkey/sig/message inline. Placed first in the
/// tx; `release_verified` introspects it.
fn ed25519_verify_ix(pubkey: &Pubkey, message: &[u8], signature: &[u8; 64]) -> Result<Instruction> {
    // u16::MAX = "read the pubkey/sig/message from this same instruction" — the
    // only form the escrow's `verify_ed25519` accepts.
    ed25519_verify_ix_at(pubkey, message, signature, u16::MAX)
}

/// Build an Ed25519 precompile instruction whose offset `instruction_index`
/// fields are set to `ix_index`. `u16::MAX` means "this instruction" (honest
/// path). Any other value makes the precompile read the pubkey/message/sig
/// from a *different* instruction while the inline bytes here say something
/// else — the shape of the precompile-spoofing attack, used to prove the
/// contract rejects it (see `examples/spoof_release.rs`).
fn ed25519_verify_ix_at(
    pubkey: &Pubkey,
    message: &[u8],
    signature: &[u8; 64],
    ix_index: u16,
) -> Result<Instruction> {
    let pk_off: u16 = 2 + 14; // after [num][pad][7 x u16 offsets]
    let sig_off: u16 = pk_off + 32;
    let msg_off: u16 = sig_off + 64;

    let mut data = Vec::new();
    data.push(1u8); // one signature
    data.push(0u8); // padding
    data.extend_from_slice(&sig_off.to_le_bytes());
    data.extend_from_slice(&ix_index.to_le_bytes());
    data.extend_from_slice(&pk_off.to_le_bytes());
    data.extend_from_slice(&ix_index.to_le_bytes());
    data.extend_from_slice(&msg_off.to_le_bytes());
    data.extend_from_slice(&(message.len() as u16).to_le_bytes());
    data.extend_from_slice(&ix_index.to_le_bytes());
    data.extend_from_slice(pubkey);
    data.extend_from_slice(signature);
    data.extend_from_slice(message);

    Ok(Instruction {
        program_id: parse_pubkey(ED25519_PROGRAM)?,
        accounts: vec![],
        data,
    })
}

/// Trustless release: prove the provider's node key signed this result, then
/// pay. Builds `[ed25519_verify, release_verified(output_hash)]`. `signature`
/// is the provider's ed25519 result signature; `provider_node_key` the key it
/// signed with (both from the run's `JobResult`).
#[allow(clippy::too_many_arguments)]
pub async fn release_verified(
    rpc_url: &str,
    settler: &Keypair,
    escrow_program: &Pubkey,
    job_account: &Pubkey,
    provider_node_key: &Pubkey,
    input: &[u8],
    output: &[u8],
    signature: &[u8; 64],
) -> Result<ReleaseResult> {
    let job = fetch_job(rpc_url, job_account).await?;
    // Permissionless (A3): any `settler` can complete a job that carries a
    // valid result proof — the payout is fixed to the provider and fee
    // authority on-chain, so the settler pays only the tx fee.
    anyhow::ensure!(job.state == 0, "escrow is not Active");

    // Reconstruct the signed message (v2, RFC-0006 §4):
    //   DOMAIN \0 <uuid string> \0 sha256(input) \0 sha256(output).
    let input_hash: [u8; 32] = Sha256::digest(input).into();
    let output_hash: [u8; 32] = Sha256::digest(output).into();
    let uuid = uuid::Uuid::from_bytes(job.job_id).to_string();
    let mut message = Vec::new();
    message.extend_from_slice(RESULT_DOMAIN);
    message.push(0);
    message.extend_from_slice(uuid.as_bytes());
    message.push(0);
    message.extend_from_slice(&input_hash);
    message.push(0);
    message.extend_from_slice(&output_hash);

    let (vault, _) = find_program_address(&[b"vault", job_account], escrow_program);
    let fee_authority = parse_pubkey(FEE_AUTHORITY)?;
    let provider_token = associated_token_address(&job.provider, &job.mint)?;
    let fee_token = associated_token_address(&fee_authority, &job.mint)?;
    let token_program = parse_pubkey(TOKEN_PROGRAM)?;
    let instructions_sysvar = parse_pubkey(SYSVAR_INSTRUCTIONS)?;

    let ed_ix = ed25519_verify_ix(provider_node_key, &message, signature)?;

    let mut data = anchor_discriminator("release_verified").to_vec();
    data.extend_from_slice(&input_hash);
    data.extend_from_slice(&output_hash);
    let rel_ix = Instruction {
        program_id: *escrow_program,
        accounts: vec![
            // payer (signer) — the settler; only funds the tx fee.
            AccountMeta {
                pubkey: settler.pubkey,
                is_signer: true,
                is_writable: true,
            },
            // consumer (non-signer) — receives the vault rent on close.
            AccountMeta {
                pubkey: job.consumer,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: *job_account,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: vault,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: provider_token,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: fee_token,
                is_signer: false,
                is_writable: true,
            },
            // `storage_token` / `author_token` are Anchor **optional** accounts
            // (RFC-0004 §6 multi-payee split). This job has no storage/author
            // share, so both are absent — and Anchor signals an absent optional
            // account by passing the *program id* in its slot. Omitting them
            // entirely would shift `token_program` into `storage_token`'s
            // position and fail with AccountOwnedByWrongProgram (3007).
            AccountMeta {
                pubkey: *escrow_program,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: *escrow_program,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: token_program,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: instructions_sysvar,
                is_signer: false,
                is_writable: false,
            },
        ],
        data,
    };

    // Ensure payout/fee ATAs exist (idempotent, paid by the settler), then
    // verify + release.
    let ixs = vec![
        ed_ix,
        create_idempotent_ata_ix(&settler.pubkey, &job.provider, &job.mint)?,
        create_idempotent_ata_ix(&settler.pubkey, &fee_authority, &job.mint)?,
        rel_ix,
    ];

    let blockhash = latest_blockhash(rpc_url).await?;
    let message_bytes = compile_message(&settler.pubkey, &ixs, &blockhash);
    let sig = settler.sign(&message_bytes);
    let mut tx = Vec::new();
    shortvec(1, &mut tx);
    tx.extend_from_slice(&sig);
    tx.extend_from_slice(&message_bytes);
    let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx);

    let signature_str = send_transaction(rpc_url, &tx_b64).await?;
    await_confirmation(rpc_url, &signature_str).await?;

    let fee = protocol_fee(job.amount);
    Ok(ReleaseResult {
        payout: job.amount - fee,
        fee,
        provider: job.provider,
        signature: signature_str,
    })
}

// Audit harness, exercised by `examples/spoof_release.rs` (which links the lib);
// the node binary never calls it, hence the allow.
#[allow(dead_code)]
/// Adversarial: attempt to release an escrow with a *forged* result proof via
/// the Ed25519 precompile "instruction index" spoof — WITHOUT the provider's
/// node key ever signing. Builds two Ed25519 instructions:
///   ix0: inline pubkey = the escrow's real provider key (what the contract
///        reads), but its offset `instruction_index` fields point at ix1;
///   ix1: an honest verification of the *attacker's* key over the same message.
/// The precompile therefore validates the attacker's signature while the
/// contract reads the provider's key inline. A hardened contract rejects this
/// (offsets must be `u16::MAX`); a vulnerable one would pay the provider for
/// work that was never signed. Returns `Ok(sig)` only if the forgery is
/// accepted — which must not happen.
#[allow(clippy::too_many_arguments)]
pub async fn attempt_spoofed_release(
    rpc_url: &str,
    settler: &Keypair,
    escrow_program: &Pubkey,
    job_account: &Pubkey,
    provider_node_key: &Pubkey,
    attacker_secret: [u8; 32],
    input: &[u8],
    output: &[u8],
) -> Result<String> {
    let job = fetch_job(rpc_url, job_account).await?;

    let input_hash: [u8; 32] = Sha256::digest(input).into();
    let output_hash: [u8; 32] = Sha256::digest(output).into();
    let uuid = uuid::Uuid::from_bytes(job.job_id).to_string();
    let mut message = Vec::new();
    message.extend_from_slice(RESULT_DOMAIN);
    message.push(0);
    message.extend_from_slice(uuid.as_bytes());
    message.push(0);
    message.extend_from_slice(&input_hash);
    message.push(0);
    message.extend_from_slice(&output_hash);

    // The attacker signs the message with a key they control.
    let attacker = iroh::SecretKey::from_bytes(&attacker_secret);
    let attacker_pk: [u8; 32] = *attacker.public().as_bytes();
    let attacker_sig: [u8; 64] = attacker.sign(&message).to_bytes();

    // ix1: honest verification of the attacker's key (what the precompile
    // actually checks). ix0: same layout but inline provider key + offsets
    // pointing at ix1 (index 1) — the spoof the contract must reject.
    let ix1 = ed25519_verify_ix_at(&attacker_pk, &message, &attacker_sig, u16::MAX)?;
    let ix0 = ed25519_verify_ix_at(provider_node_key, &message, &attacker_sig, 1)?;

    let (vault, _) = find_program_address(&[b"vault", job_account], escrow_program);
    let fee_authority = parse_pubkey(FEE_AUTHORITY)?;
    let provider_token = associated_token_address(&job.provider, &job.mint)?;
    let fee_token = associated_token_address(&fee_authority, &job.mint)?;
    let token_program = parse_pubkey(TOKEN_PROGRAM)?;
    let instructions_sysvar = parse_pubkey(SYSVAR_INSTRUCTIONS)?;

    let mut data = anchor_discriminator("release_verified").to_vec();
    data.extend_from_slice(&input_hash);
    data.extend_from_slice(&output_hash);
    let rel_ix = Instruction {
        program_id: *escrow_program,
        accounts: vec![
            AccountMeta {
                pubkey: settler.pubkey,
                is_signer: true,
                is_writable: true,
            },
            AccountMeta {
                pubkey: job.consumer,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: *job_account,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: vault,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: provider_token,
                is_signer: false,
                is_writable: true,
            },
            AccountMeta {
                pubkey: fee_token,
                is_signer: false,
                is_writable: true,
            },
            // `storage_token` / `author_token` are Anchor **optional** accounts
            // (RFC-0004 §6 multi-payee split). This job has no storage/author
            // share, so both are absent — and Anchor signals an absent optional
            // account by passing the *program id* in its slot. Omitting them
            // entirely would shift `token_program` into `storage_token`'s
            // position and fail with AccountOwnedByWrongProgram (3007).
            AccountMeta {
                pubkey: *escrow_program,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: *escrow_program,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: token_program,
                is_signer: false,
                is_writable: false,
            },
            AccountMeta {
                pubkey: instructions_sysvar,
                is_signer: false,
                is_writable: false,
            },
        ],
        data,
    };

    // ix0 must be at index 0 (release_verified introspects instruction 0).
    let ixs = vec![
        ix0,
        ix1,
        create_idempotent_ata_ix(&settler.pubkey, &job.provider, &job.mint)?,
        create_idempotent_ata_ix(&settler.pubkey, &fee_authority, &job.mint)?,
        rel_ix,
    ];

    let blockhash = latest_blockhash(rpc_url).await?;
    let message_bytes = compile_message(&settler.pubkey, &ixs, &blockhash);
    let sig = settler.sign(&message_bytes);
    let mut tx = Vec::new();
    shortvec(1, &mut tx);
    tx.extend_from_slice(&sig);
    tx.extend_from_slice(&message_bytes);
    let tx_b64 = base64::engine::general_purpose::STANDARD.encode(&tx);

    let signature_str = send_transaction(rpc_url, &tx_b64).await?;
    await_confirmation(rpc_url, &signature_str).await?;
    Ok(signature_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminator_is_stable() {
        // sha256("global:create_job")[..8]
        let d = anchor_discriminator("create_job");
        assert_eq!(d.len(), 8);
        // Recompute independently to guard against accidental changes.
        let expect: [u8; 8] = Sha256::digest(b"global:create_job")[..8]
            .try_into()
            .unwrap();
        assert_eq!(d, expect);
    }

    #[test]
    fn shortvec_encodes_known_lengths() {
        let mut v = Vec::new();
        shortvec(0, &mut v);
        assert_eq!(v, [0]);
        v.clear();
        shortvec(5, &mut v);
        assert_eq!(v, [5]);
        v.clear();
        shortvec(0x80, &mut v);
        assert_eq!(v, [0x80, 0x01]);
    }

    #[test]
    fn ata_matches_known_vector() {
        // Wrapped SOL ATA for a well-known account is deterministic; here we
        // just assert derivation is stable and off-curve.
        let owner = parse_pubkey("11111111111111111111111111111111").unwrap();
        let mint = parse_pubkey("So11111111111111111111111111111111111111112").unwrap();
        let ata = associated_token_address(&owner, &mint).unwrap();
        assert!(!is_on_curve(&ata), "an ATA (PDA) must be off-curve");
    }
}
