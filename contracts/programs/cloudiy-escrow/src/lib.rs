use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Mint, Token, TokenAccount, Transfer};

declare_id!("9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN");

/// Protocol fee in basis points (4% — vs Akash's 20% USDC take rate).
pub const PROTOCOL_FEE_BPS: u64 = 400;
/// Authority that receives protocol fees.
pub const FEE_AUTHORITY: Pubkey = pubkey!("GnaUN3hxTZaq6FqzVzLjXzJWi6svocFqgYbBJSdusFJP");

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
}
