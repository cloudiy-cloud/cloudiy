use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::load_instruction_at_checked;
use anchor_spl::token::{self, CloseAccount, Mint, Token, TokenAccount, Transfer};

declare_id!("9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN");

/// Protocol fee in basis points (4% — vs Akash's 20% USDC take rate).
pub const PROTOCOL_FEE_BPS: u64 = 400;
/// Upper bound on a job's timeout (30 days) — bounds how long funds can be
/// locked before the consumer can reclaim them.
pub const MAX_TIMEOUT_SECS: i64 = 30 * 24 * 60 * 60;
/// Authority that receives protocol fees.
pub const FEE_AUTHORITY: Pubkey = pubkey!("GnaUN3hxTZaq6FqzVzLjXzJWi6svocFqgYbBJSdusFJP");
/// The Ed25519 signature-verification precompile program.
pub const ED25519_PROGRAM_ID: Pubkey = pubkey!("Ed25519SigVerify111111111111111111111111111");
/// Domain separator for signed job results (matches `cloudiy_common::sig`).
pub const RESULT_DOMAIN: &[u8] = b"cloudiy/result/v2";

#[program]
pub mod cloudiy_escrow {
    use super::*;

    /// Consumer locks `amount` of USDC for a GPU job. Funds sit in a vault
    /// owned by the job PDA until released or refunded. Single-payee: the whole
    /// payout (minus the protocol fee) goes to the compute provider — exactly
    /// as before the multi-payee split existed.
    pub fn create_job(
        ctx: Context<CreateJob>,
        job_id: [u8; 16],
        amount: u64,
        timeout_secs: i64,
        provider_node_key: [u8; 32],
    ) -> Result<()> {
        // No storage/author split → default payees, 0 bps.
        init_job(
            ctx,
            job_id,
            amount,
            timeout_secs,
            provider_node_key,
            Pubkey::default(),
            0,
            Pubkey::default(),
            0,
        )
    }

    /// Like `create_job`, but records a multi-payee split (RFC-0004 §6): a
    /// storage provider and/or an app/model author each take a basis-point
    /// share, the protocol keeps its fixed fee, and the compute provider gets
    /// the remainder. A separate instruction (rather than a wider `create_job`)
    /// keeps the original signature — and every existing off-chain caller —
    /// working unchanged.
    #[allow(clippy::too_many_arguments)]
    pub fn create_job_split(
        ctx: Context<CreateJob>,
        job_id: [u8; 16],
        amount: u64,
        timeout_secs: i64,
        provider_node_key: [u8; 32],
        storage_payee: Pubkey,
        storage_bps: u16,
        author_payee: Pubkey,
        author_bps: u16,
    ) -> Result<()> {
        init_job(
            ctx,
            job_id,
            amount,
            timeout_secs,
            provider_node_key,
            storage_payee,
            storage_bps,
            author_payee,
            author_bps,
        )
    }

    /// Consumer confirms delivery: vault pays the provider minus the protocol
    /// fee, then vault and job accounts are closed.
    pub fn release(ctx: Context<Release>) -> Result<()> {
        let job = &ctx.accounts.job;
        require!(job.state == JobState::Active, EscrowError::NotActive);

        let job_id = job.job_id;
        let consumer = job.consumer;
        let bump = job.bump;
        let seeds: &[&[u8]] = &[b"job", consumer.as_ref(), job_id.as_ref(), &[bump]];
        let signer = &[seeds];

        let split = settle(
            &ctx.accounts.job,
            &ctx.accounts.vault,
            &ctx.accounts.token_program,
            signer,
            &ctx.accounts.provider_token,
            &ctx.accounts.fee_token,
            ctx.accounts.storage_token.as_ref(),
            ctx.accounts.author_token.as_ref(),
            ctx.accounts.consumer.to_account_info(),
        )?;

        let job = &mut ctx.accounts.job;
        job.state = JobState::Released;

        emit!(JobReleased {
            job: job.key(),
            job_id,
            payout: split.provider,
            fee: split.fee,
            storage: split.storage,
            author: split.author,
        });
        Ok(())
    }

    /// Trustless release: pays the provider only if a preceding Ed25519
    /// instruction proves the provider's node key signed this job's result.
    /// The caller passes `input_hash = sha256(input)` and `output_hash =
    /// sha256(output)`; the contract reconstructs the signed message and checks
    /// the Ed25519 precompile verified it against `job.provider_node_key`. As of
    /// v2 (RFC-0006 §4) the signature binds the input too, so on-chain settlement
    /// enforces output-for-this-input, not just output-provenance.
    pub fn release_verified(
        ctx: Context<ReleaseVerified>,
        input_hash: [u8; 32],
        output_hash: [u8; 32],
    ) -> Result<()> {
        let job = &ctx.accounts.job;
        require!(job.state == JobState::Active, EscrowError::NotActive);

        // Reconstruct the exact message the node signs (see cloudiy_common::sig):
        //   RESULT_DOMAIN \0 <job_id as UUID string> \0 <input_hash> \0 <output_hash>
        let mut message = Vec::with_capacity(RESULT_DOMAIN.len() + 1 + 36 + 1 + 32 + 1 + 32);
        message.extend_from_slice(RESULT_DOMAIN);
        message.push(0);
        message.extend_from_slice(&uuid_string(&job.job_id));
        message.push(0);
        message.extend_from_slice(&input_hash);
        message.push(0);
        message.extend_from_slice(&output_hash);

        verify_ed25519(
            &ctx.accounts.instructions.to_account_info(),
            &job.provider_node_key,
            &message,
        )?;

        // Proof verified — apply the exact same split as `release`.
        let job_id = job.job_id;
        let consumer = job.consumer;
        let bump = job.bump;
        let seeds: &[&[u8]] = &[b"job", consumer.as_ref(), job_id.as_ref(), &[bump]];
        let signer = &[seeds];

        let split = settle(
            &ctx.accounts.job,
            &ctx.accounts.vault,
            &ctx.accounts.token_program,
            signer,
            &ctx.accounts.provider_token,
            &ctx.accounts.fee_token,
            ctx.accounts.storage_token.as_ref(),
            ctx.accounts.author_token.as_ref(),
            ctx.accounts.consumer.to_account_info(),
        )?;

        let job = &mut ctx.accounts.job;
        job.state = JobState::Released;

        emit!(JobReleased {
            job: job.key(),
            job_id,
            payout: split.provider,
            fee: split.fee,
            storage: split.storage,
            author: split.author,
        });
        Ok(())
    }

    /// Returns escrowed funds to the consumer. Allowed when:
    /// - the provider signs (voluntary cancel, any time), or
    /// - the consumer signs after the deadline has passed.
    pub fn refund(ctx: Context<Refund>) -> Result<()> {
        let job = &ctx.accounts.job;
        require!(job.state == JobState::Active, EscrowError::NotActive);

        let signer_key = ctx.accounts.signer.key();
        let now = Clock::get()?.unix_timestamp;
        let provider_cancel = signer_key == job.provider;
        let consumer_timeout = signer_key == job.consumer && now >= job.deadline;
        require!(
            provider_cancel || consumer_timeout,
            EscrowError::RefundNotAllowed
        );

        let job_id = job.job_id;
        let consumer = job.consumer;
        let amount = job.amount;
        let seeds: &[&[u8]] = &[b"job", consumer.as_ref(), job_id.as_ref(), &[job.bump]];
        let signer_seeds = &[seeds];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.consumer_token.to_account_info(),
                    authority: ctx.accounts.job.to_account_info(),
                },
                signer_seeds,
            ),
            amount,
        )?;
        token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            CloseAccount {
                account: ctx.accounts.vault.to_account_info(),
                destination: ctx.accounts.consumer.to_account_info(),
                authority: ctx.accounts.job.to_account_info(),
            },
            signer_seeds,
        ))?;

        let job = &mut ctx.accounts.job;
        job.state = JobState::Refunded;

        emit!(JobRefunded {
            job: job.key(),
            job_id,
            amount,
        });
        Ok(())
    }
}

#[derive(Accounts)]
#[instruction(job_id: [u8; 16])]
pub struct CreateJob<'info> {
    #[account(mut)]
    pub consumer: Signer<'info>,
    /// CHECK: provider is only stored as the payout destination authority.
    pub provider: UncheckedAccount<'info>,
    pub mint: Account<'info, Mint>,

    #[account(
        init,
        payer = consumer,
        space = 8 + Job::INIT_SPACE,
        seeds = [b"job", consumer.key().as_ref(), job_id.as_ref()],
        bump,
    )]
    pub job: Account<'info, Job>,

    #[account(
        init,
        payer = consumer,
        seeds = [b"vault", job.key().as_ref()],
        bump,
        token::mint = mint,
        token::authority = job,
    )]
    pub vault: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = consumer_token.mint == mint.key() @ EscrowError::MintMismatch,
        constraint = consumer_token.owner == consumer.key() @ EscrowError::OwnerMismatch,
    )]
    pub consumer_token: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Release<'info> {
    #[account(mut, constraint = consumer.key() == job.consumer @ EscrowError::OwnerMismatch)]
    pub consumer: Signer<'info>,

    #[account(
        mut,
        close = consumer,
        seeds = [b"job", job.consumer.as_ref(), job.job_id.as_ref()],
        bump = job.bump,
    )]
    pub job: Account<'info, Job>,

    #[account(
        mut,
        seeds = [b"vault", job.key().as_ref()],
        bump,
    )]
    pub vault: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = provider_token.mint == job.mint @ EscrowError::MintMismatch,
        constraint = provider_token.owner == job.provider @ EscrowError::OwnerMismatch,
    )]
    pub provider_token: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = fee_token.mint == job.mint @ EscrowError::MintMismatch,
        constraint = fee_token.owner == FEE_AUTHORITY @ EscrowError::OwnerMismatch,
    )]
    pub fee_token: Account<'info, TokenAccount>,

    /// Optional storage-provider payout (RFC-0004 §6). Required only when
    /// `job.storage_bps > 0`; constraints apply when present.
    #[account(
        mut,
        constraint = storage_token.mint == job.mint @ EscrowError::MintMismatch,
        constraint = storage_token.owner == job.storage_payee @ EscrowError::OwnerMismatch,
    )]
    pub storage_token: Option<Account<'info, TokenAccount>>,

    /// Optional author/publisher royalty payout. Required only when
    /// `job.author_bps > 0`; constraints apply when present.
    #[account(
        mut,
        constraint = author_token.mint == job.mint @ EscrowError::MintMismatch,
        constraint = author_token.owner == job.author_payee @ EscrowError::OwnerMismatch,
    )]
    pub author_token: Option<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ReleaseVerified<'info> {
    /// Any caller may settle a job that carries a valid result proof. The
    /// payout is fixed to the provider and fee authority and the vault rent
    /// returns to the consumer, so a permissionless settler can only *complete*
    /// the honest payment — never redirect a lamport (A3). This lets the
    /// provider (or a watcher) claim without the consumer having to sign.
    pub payer: Signer<'info>,

    /// CHECK: receives the vault's rent on close; must be the job's consumer.
    #[account(mut, constraint = consumer.key() == job.consumer @ EscrowError::OwnerMismatch)]
    pub consumer: UncheckedAccount<'info>,

    #[account(
        mut,
        close = consumer,
        seeds = [b"job", job.consumer.as_ref(), job.job_id.as_ref()],
        bump = job.bump,
    )]
    pub job: Account<'info, Job>,

    #[account(mut, seeds = [b"vault", job.key().as_ref()], bump)]
    pub vault: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = provider_token.mint == job.mint @ EscrowError::MintMismatch,
        constraint = provider_token.owner == job.provider @ EscrowError::OwnerMismatch,
    )]
    pub provider_token: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = fee_token.mint == job.mint @ EscrowError::MintMismatch,
        constraint = fee_token.owner == FEE_AUTHORITY @ EscrowError::OwnerMismatch,
    )]
    pub fee_token: Account<'info, TokenAccount>,

    /// Optional storage-provider payout (RFC-0004 §6). Required only when
    /// `job.storage_bps > 0`; constraints apply when present. Only the payees
    /// recorded on the job can be paid, so a permissionless settler can never
    /// redirect funds (A3).
    #[account(
        mut,
        constraint = storage_token.mint == job.mint @ EscrowError::MintMismatch,
        constraint = storage_token.owner == job.storage_payee @ EscrowError::OwnerMismatch,
    )]
    pub storage_token: Option<Account<'info, TokenAccount>>,

    /// Optional author/publisher royalty payout. Required only when
    /// `job.author_bps > 0`; constraints apply when present.
    #[account(
        mut,
        constraint = author_token.mint == job.mint @ EscrowError::MintMismatch,
        constraint = author_token.owner == job.author_payee @ EscrowError::OwnerMismatch,
    )]
    pub author_token: Option<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,

    /// CHECK: the instructions sysvar, read to introspect the Ed25519 proof.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
}

/// Shared body of `create_job` / `create_job_split`: validate, record the job
/// (including any multi-payee split), and lock the funds in the vault.
#[allow(clippy::too_many_arguments)]
fn init_job(
    ctx: Context<CreateJob>,
    job_id: [u8; 16],
    amount: u64,
    timeout_secs: i64,
    provider_node_key: [u8; 32],
    storage_payee: Pubkey,
    storage_bps: u16,
    author_payee: Pubkey,
    author_bps: u16,
) -> Result<()> {
    require!(amount > 0, EscrowError::InvalidAmount);
    require!(timeout_secs >= 60, EscrowError::TimeoutTooShort);
    // Cap the timeout so a job can't lock funds indefinitely (and so the
    // deadline add below can't be pushed anywhere near i64 overflow).
    require!(
        timeout_secs <= MAX_TIMEOUT_SECS,
        EscrowError::TimeoutTooLong
    );

    // The fee + storage + author shares must leave a strictly positive slice
    // for the compute provider (checked in bps here; the release-time math is
    // the authority on exact lamports).
    let split_bps = PROTOCOL_FEE_BPS
        .checked_add(storage_bps as u64)
        .and_then(|v| v.checked_add(author_bps as u64))
        .ok_or(EscrowError::MathOverflow)?;
    require!(split_bps < 10_000, EscrowError::SplitTooLarge);
    // A payee with a share must be a real account.
    require!(
        storage_bps == 0 || storage_payee != Pubkey::default(),
        EscrowError::MissingPayee
    );
    require!(
        author_bps == 0 || author_payee != Pubkey::default(),
        EscrowError::MissingPayee
    );

    let job = &mut ctx.accounts.job;
    job.job_id = job_id;
    job.consumer = ctx.accounts.consumer.key();
    job.provider = ctx.accounts.provider.key();
    job.mint = ctx.accounts.mint.key();
    job.amount = amount;
    job.deadline = Clock::get()?
        .unix_timestamp
        .checked_add(timeout_secs)
        .ok_or(EscrowError::MathOverflow)?;
    job.state = JobState::Active;
    job.bump = ctx.bumps.job;
    // The provider's iroh node key — the identity that signs job results.
    // `release_verified` checks a result signature against this key.
    job.provider_node_key = provider_node_key;
    job.storage_payee = storage_payee;
    job.storage_bps = storage_bps;
    job.author_payee = author_payee;
    job.author_bps = author_bps;

    token::transfer(
        CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.consumer_token.to_account_info(),
                to: ctx.accounts.vault.to_account_info(),
                authority: ctx.accounts.consumer.to_account_info(),
            },
        ),
        amount,
    )?;

    emit!(JobCreated {
        job: job.key(),
        job_id,
        consumer: job.consumer,
        provider: job.provider,
        amount,
        deadline: job.deadline,
    });
    Ok(())
}

/// The four shares of a settled job (micro-USDC). Invariant:
/// `provider + fee + storage + author == job.amount`.
struct JobSplit {
    provider: u64,
    fee: u64,
    storage: u64,
    author: u64,
}

/// Single source of truth for the payout: compute the split, pay each recorded
/// payee (skipping zero shares), and close the vault to the consumer. Shared by
/// `release` and `release_verified` so their money logic can never diverge.
///
/// Payees and bps come from the on-chain `job`, never from the caller, so a
/// permissionless settler can complete the honest payout but can never redirect
/// a lamport (preserves the A3 safety of `release_verified`). The provider's
/// share is the remainder, so it absorbs all rounding dust and nothing is lost
/// or created.
#[allow(clippy::too_many_arguments)]
fn settle<'info>(
    job: &Account<'info, Job>,
    vault: &Account<'info, TokenAccount>,
    token_program: &Program<'info, Token>,
    signer: &[&[&[u8]]],
    provider_token: &Account<'info, TokenAccount>,
    fee_token: &Account<'info, TokenAccount>,
    storage_token: Option<&Account<'info, TokenAccount>>,
    author_token: Option<&Account<'info, TokenAccount>>,
    close_dest: AccountInfo<'info>,
) -> Result<JobSplit> {
    let amount = job.amount;
    let cut = |bps: u64| -> Result<u64> {
        amount
            .checked_mul(bps)
            .and_then(|v| v.checked_div(10_000))
            .ok_or_else(|| error!(EscrowError::MathOverflow))
    };
    let fee = cut(PROTOCOL_FEE_BPS)?;
    let storage = cut(job.storage_bps as u64)?;
    let author = cut(job.author_bps as u64)?;
    // Provider gets the remainder — the split can never lose or mint lamports.
    let provider = amount
        .checked_sub(fee)
        .and_then(|v| v.checked_sub(storage))
        .and_then(|v| v.checked_sub(author))
        .ok_or(EscrowError::MathOverflow)?;
    require!(provider > 0, EscrowError::SplitTooLarge);

    // A payee with a recorded share must have supplied its token account.
    require!(
        job.storage_bps == 0 || storage_token.is_some(),
        EscrowError::MissingPayee
    );
    require!(
        job.author_bps == 0 || author_token.is_some(),
        EscrowError::MissingPayee
    );

    // One transfer per non-zero share, all authorized by the job PDA.
    let pay = |to: AccountInfo<'info>, amt: u64| -> Result<()> {
        if amt == 0 {
            return Ok(());
        }
        token::transfer(
            CpiContext::new_with_signer(
                token_program.to_account_info(),
                Transfer {
                    from: vault.to_account_info(),
                    to,
                    authority: job.to_account_info(),
                },
                signer,
            ),
            amt,
        )
    };
    pay(provider_token.to_account_info(), provider)?;
    pay(fee_token.to_account_info(), fee)?;
    if let Some(t) = storage_token {
        pay(t.to_account_info(), storage)?;
    }
    if let Some(t) = author_token {
        pay(t.to_account_info(), author)?;
    }

    // Vault emptied → close it, returning its rent to the consumer.
    token::close_account(CpiContext::new_with_signer(
        token_program.to_account_info(),
        CloseAccount {
            account: vault.to_account_info(),
            destination: close_dest,
            authority: job.to_account_info(),
        },
        signer,
    ))?;

    Ok(JobSplit {
        provider,
        fee,
        storage,
        author,
    })
}

/// Format 16 raw bytes as a lowercase UUID string (8-4-4-4-12).
fn uuid_string(bytes: &[u8; 16]) -> [u8; 36] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 36];
    let mut oi = 0;
    for (i, b) in bytes.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            out[oi] = b'-';
            oi += 1;
        }
        out[oi] = HEX[(b >> 4) as usize];
        out[oi + 1] = HEX[(b & 0x0f) as usize];
        oi += 2;
    }
    out
}

/// Confirm the transaction's first instruction is an Ed25519 precompile
/// verification of `expected_message` by `expected_pubkey`. The precompile
/// itself checks the signature is valid; we check *who* and *what* it signed.
fn verify_ed25519(
    instructions_sysvar: &AccountInfo,
    expected_pubkey: &[u8; 32],
    expected_message: &[u8],
) -> Result<()> {
    let ix = load_instruction_at_checked(0, instructions_sysvar)
        .map_err(|_| error!(EscrowError::MissingSignature))?;
    require_keys_eq!(
        ix.program_id,
        ED25519_PROGRAM_ID,
        EscrowError::MissingSignature
    );

    let d = &ix.data;
    // Ed25519 precompile layout:
    //   [num_sigs u8][pad u8]
    //   [ SignatureOffsets: sig_off u16, sig_ix_idx u16, pk_off u16,
    //     pk_ix_idx u16, msg_off u16, msg_size u16, msg_ix_idx u16 ]   (14 bytes)
    //   [ inline pubkey / signature / message ]
    require!(d.len() >= 16 && d[0] == 1, EscrowError::BadSignature);
    let u16_at = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]);
    let pk_off = u16_at(6) as usize;
    let msg_off = u16_at(10) as usize;
    let msg_size = u16_at(12) as usize;

    // CRITICAL: the three instruction-index fields must all reference *this*
    // instruction (u16::MAX sentinel). Otherwise the precompile verifies the
    // pubkey/message from a *different* instruction (attacker-controlled, with
    // a valid signature by a key they own), while this contract reads the
    // inline bytes below and is fooled into accepting them — a forged proof
    // (Ed25519 precompile "instruction index" spoofing). Pinning them to
    // u16::MAX guarantees the precompile checked the very bytes we compare.
    const THIS_IX: u16 = u16::MAX;
    require!(
        u16_at(4) == THIS_IX && u16_at(8) == THIS_IX && u16_at(14) == THIS_IX,
        EscrowError::BadSignature
    );

    require!(pk_off + 32 <= d.len(), EscrowError::BadSignature);
    require!(msg_off + msg_size <= d.len(), EscrowError::BadSignature);
    require!(
        &d[pk_off..pk_off + 32] == expected_pubkey,
        EscrowError::WrongSigner
    );
    require!(
        &d[msg_off..msg_off + msg_size] == expected_message,
        EscrowError::BadSignature
    );
    Ok(())
}

#[derive(Accounts)]
pub struct Refund<'info> {
    pub signer: Signer<'info>,
    /// CHECK: receives vault rent; must be the job's consumer.
    #[account(mut, constraint = consumer.key() == job.consumer @ EscrowError::OwnerMismatch)]
    pub consumer: UncheckedAccount<'info>,

    #[account(
        mut,
        close = consumer,
        seeds = [b"job", job.consumer.as_ref(), job.job_id.as_ref()],
        bump = job.bump,
    )]
    pub job: Account<'info, Job>,

    #[account(
        mut,
        seeds = [b"vault", job.key().as_ref()],
        bump,
    )]
    pub vault: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = consumer_token.mint == job.mint @ EscrowError::MintMismatch,
        constraint = consumer_token.owner == job.consumer @ EscrowError::OwnerMismatch,
    )]
    pub consumer_token: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[account]
#[derive(InitSpace)]
pub struct Job {
    pub job_id: [u8; 16],
    pub consumer: Pubkey,
    pub provider: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub deadline: i64,
    pub state: JobState,
    pub bump: u8,
    /// Provider's iroh node key (ed25519) that signs job results. Appended
    /// last so the leading layout stays compatible with off-chain parsers.
    pub provider_node_key: [u8; 32],
    // --- Multi-payee split (RFC-0004 §6), appended after provider_node_key so
    // the leading layout stays byte-compatible. A job with no split leaves
    // these at their defaults (Pubkey::default() / 0 bps) and pays out exactly
    // as before: provider (amount − fee) + protocol fee.
    /// External storage provider's payout account owner (default = unused).
    pub storage_payee: Pubkey,
    /// Storage share in basis points (0 = unused).
    pub storage_bps: u16,
    /// App/model author royalty account owner (default = unused).
    pub author_payee: Pubkey,
    /// Author royalty in basis points (0 = unused).
    pub author_bps: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum JobState {
    Active,
    Released,
    Refunded,
}

#[event]
pub struct JobCreated {
    pub job: Pubkey,
    pub job_id: [u8; 16],
    pub consumer: Pubkey,
    pub provider: Pubkey,
    pub amount: u64,
    pub deadline: i64,
}

#[event]
pub struct JobReleased {
    pub job: Pubkey,
    pub job_id: [u8; 16],
    /// Compute provider's share (amount − fee − storage − author).
    pub payout: u64,
    pub fee: u64,
    /// Storage provider's share (0 when no storage split).
    pub storage: u64,
    /// Author/publisher royalty (0 when no author split).
    pub author: u64,
}

#[event]
pub struct JobRefunded {
    pub job: Pubkey,
    pub job_id: [u8; 16],
    pub amount: u64,
}

#[error_code]
pub enum EscrowError {
    #[msg("Amount must be greater than zero")]
    InvalidAmount,
    #[msg("Timeout must be at least 60 seconds")]
    TimeoutTooShort,
    #[msg("Timeout exceeds the maximum allowed")]
    TimeoutTooLong,
    #[msg("Job is not active")]
    NotActive,
    #[msg("Token account mint does not match job mint")]
    MintMismatch,
    #[msg("Token account owner mismatch")]
    OwnerMismatch,
    #[msg("Refund not allowed: only provider cancel or consumer after deadline")]
    RefundNotAllowed,
    #[msg("Math overflow")]
    MathOverflow,
    #[msg("Missing or invalid Ed25519 result-signature instruction")]
    MissingSignature,
    #[msg("Malformed Ed25519 signature instruction")]
    BadSignature,
    #[msg("Result signed by the wrong key")]
    WrongSigner,
    #[msg("Payment split leaves no share for the compute provider")]
    SplitTooLarge,
    #[msg("A payee with a non-zero share is missing its token account")]
    MissingPayee,
}
