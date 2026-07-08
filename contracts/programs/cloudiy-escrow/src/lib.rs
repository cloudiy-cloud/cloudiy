use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::load_instruction_at_checked;
use anchor_spl::token::{self, CloseAccount, Mint, Token, TokenAccount, Transfer};

declare_id!("9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN");

/// Protocol fee in basis points (4% — vs Akash's 20% USDC take rate).
pub const PROTOCOL_FEE_BPS: u64 = 400;
/// Authority that receives protocol fees.
pub const FEE_AUTHORITY: Pubkey = pubkey!("GnaUN3hxTZaq6FqzVzLjXzJWi6svocFqgYbBJSdusFJP");
/// The Ed25519 signature-verification precompile program.
pub const ED25519_PROGRAM_ID: Pubkey = pubkey!("Ed25519SigVerify111111111111111111111111111");
/// Domain separator for signed job results (matches `cloudiy_common::sig`).
pub const RESULT_DOMAIN: &[u8] = b"cloudiy/result/v1";

#[program]
pub mod cloudiy_escrow {
    use super::*;

    /// Consumer locks `amount` of USDC for a GPU job. Funds sit in a vault
    /// owned by the job PDA until released or refunded.
    pub fn create_job(
        ctx: Context<CreateJob>,
        job_id: [u8; 16],
        amount: u64,
        timeout_secs: i64,
        provider_node_key: [u8; 32],
    ) -> Result<()> {
        require!(amount > 0, EscrowError::InvalidAmount);
        require!(timeout_secs >= 60, EscrowError::TimeoutTooShort);

        let job = &mut ctx.accounts.job;
        job.job_id = job_id;
        job.consumer = ctx.accounts.consumer.key();
        job.provider = ctx.accounts.provider.key();
        job.mint = ctx.accounts.mint.key();
        job.amount = amount;
        job.deadline = Clock::get()?.unix_timestamp + timeout_secs;
        job.state = JobState::Active;
        job.bump = ctx.bumps.job;
        // The provider's iroh node key — the identity that signs job results.
        // `release_verified` checks a result signature against this key.
        job.provider_node_key = provider_node_key;

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

    /// Consumer confirms delivery: vault pays the provider minus the protocol
    /// fee, then vault and job accounts are closed.
    pub fn release(ctx: Context<Release>) -> Result<()> {
        let job = &ctx.accounts.job;
        require!(job.state == JobState::Active, EscrowError::NotActive);

        let fee = job
            .amount
            .checked_mul(PROTOCOL_FEE_BPS)
            .and_then(|v| v.checked_div(10_000))
            .ok_or(EscrowError::MathOverflow)?;
        let payout = job.amount.checked_sub(fee).ok_or(EscrowError::MathOverflow)?;

        let job_id = job.job_id;
        let consumer = job.consumer;
        let seeds: &[&[u8]] = &[b"job", consumer.as_ref(), job_id.as_ref(), &[job.bump]];
        let signer = &[seeds];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.provider_token.to_account_info(),
                    authority: ctx.accounts.job.to_account_info(),
                },
                signer,
            ),
            payout,
        )?;
        if fee > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.vault.to_account_info(),
                        to: ctx.accounts.fee_token.to_account_info(),
                        authority: ctx.accounts.job.to_account_info(),
                    },
                    signer,
                ),
                fee,
            )?;
        }
        token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            CloseAccount {
                account: ctx.accounts.vault.to_account_info(),
                destination: ctx.accounts.consumer.to_account_info(),
                authority: ctx.accounts.job.to_account_info(),
            },
            signer,
        ))?;

        let job = &mut ctx.accounts.job;
        job.state = JobState::Released;

        emit!(JobReleased {
            job: job.key(),
            job_id,
            payout,
            fee,
        });
        Ok(())
    }

    /// Trustless release: pays the provider only if a preceding Ed25519
    /// instruction proves the provider's node key signed this job's result.
    /// The consumer passes `output_hash = sha256(output)`; the contract
    /// reconstructs the signed message and checks the Ed25519 precompile
    /// verified it against `job.provider_node_key`.
    pub fn release_verified(ctx: Context<ReleaseVerified>, output_hash: [u8; 32]) -> Result<()> {
        let job = &ctx.accounts.job;
        require!(job.state == JobState::Active, EscrowError::NotActive);

        // Reconstruct the exact message the node signs (see cloudiy_common::sig):
        //   RESULT_DOMAIN \0 <job_id as UUID string> \0 <output_hash>
        let mut message = Vec::with_capacity(RESULT_DOMAIN.len() + 1 + 36 + 1 + 32);
        message.extend_from_slice(RESULT_DOMAIN);
        message.push(0);
        message.extend_from_slice(&uuid_string(&job.job_id));
        message.push(0);
        message.extend_from_slice(&output_hash);

        verify_ed25519(
            &ctx.accounts.instructions.to_account_info(),
            &job.provider_node_key,
            &message,
        )?;

        // Same payout as `release`, now gated by proof of a signed result.
        let fee = job
            .amount
            .checked_mul(PROTOCOL_FEE_BPS)
            .and_then(|v| v.checked_div(10_000))
            .ok_or(EscrowError::MathOverflow)?;
        let payout = job.amount.checked_sub(fee).ok_or(EscrowError::MathOverflow)?;

        let job_id = job.job_id;
        let consumer = job.consumer;
        let seeds: &[&[u8]] = &[b"job", consumer.as_ref(), job_id.as_ref(), &[job.bump]];
        let signer = &[seeds];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.provider_token.to_account_info(),
                    authority: ctx.accounts.job.to_account_info(),
                },
                signer,
            ),
            payout,
        )?;
        if fee > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.vault.to_account_info(),
                        to: ctx.accounts.fee_token.to_account_info(),
                        authority: ctx.accounts.job.to_account_info(),
                    },
                    signer,
                ),
                fee,
            )?;
        }
        token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            CloseAccount {
                account: ctx.accounts.vault.to_account_info(),
                destination: ctx.accounts.consumer.to_account_info(),
                authority: ctx.accounts.job.to_account_info(),
            },
            signer,
        ))?;

        let job = &mut ctx.accounts.job;
        job.state = JobState::Released;

        emit!(JobReleased {
            job: job.key(),
            job_id,
            payout,
            fee,
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
        require!(provider_cancel || consumer_timeout, EscrowError::RefundNotAllowed);

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

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ReleaseVerified<'info> {
    #[account(mut, constraint = consumer.key() == job.consumer @ EscrowError::OwnerMismatch)]
    pub consumer: Signer<'info>,

    #[account(
        mut,
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

    pub token_program: Program<'info, Token>,

    /// CHECK: the instructions sysvar, read to introspect the Ed25519 proof.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
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
    require_keys_eq!(ix.program_id, ED25519_PROGRAM_ID, EscrowError::MissingSignature);

    let d = &ix.data;
    // [num_sigs u8][pad u8][offsets: 7 x u16 = 14 bytes][ inline pubkey/sig/msg ]
    require!(d.len() >= 16 && d[0] == 1, EscrowError::BadSignature);
    let u16_at = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]) as usize;
    let pk_off = u16_at(6);
    let msg_off = u16_at(10);
    let msg_size = u16_at(12);

    require!(pk_off + 32 <= d.len(), EscrowError::BadSignature);
    require!(msg_off + msg_size <= d.len(), EscrowError::BadSignature);
    require!(&d[pk_off..pk_off + 32] == expected_pubkey, EscrowError::WrongSigner);
    require!(&d[msg_off..msg_off + msg_size] == expected_message, EscrowError::BadSignature);
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
    pub payout: u64,
    pub fee: u64,
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
}
