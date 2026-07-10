//! Cloudiy VM manifest — a small, identity-keyed state account.
//!
//! Each wallet owns one manifest PDA (`["vm", owner]`) holding a tiny blob: the
//! apps it has installed, a rented machine, its VM name. The PDA is derived
//! from the owner and the owner must sign, so an identity can only write its
//! OWN manifest — no central operator, permissionless, keyless. This is where a
//! user's CloudiyOS state lives so the same wallet resumes on ANY browser or
//! device, without a central server.
//!
//! Large / regenerable data (model output samples, artifacts) stays off-chain;
//! only the small manifest lives here. If that grows, the manifest can hold a
//! pointer (and content hash) to an encrypted blob on decentralized storage
//! (e.g. Arweave), keeping this account small either way.

use anchor_lang::prelude::*;

// PLACEHOLDER program id — run `anchor keys sync` after the first `anchor build`
// to replace it with this program's real deploy key, then deploy to devnet.
declare_id!("4mSKMwnL58DEUBR2ghvXgmVaJSqTTfD3MwboK4dAy6gv");

/// Max serialized manifest size (bytes). The manifest is deliberately small so
/// the account stays cheap; the client keeps it well under this.
pub const MAX_MANIFEST: usize = 1024;

#[program]
pub mod cloudiy_vm {
    use super::*;

    /// Create or update the caller's VM manifest. `data` is the client-encoded
    /// (and optionally client-encrypted) manifest bytes. Because the PDA is
    /// derived from `owner` and `owner` signs, only the identity itself can
    /// write its manifest; nobody — including whoever runs an RPC — can forge
    /// or overwrite it.
    pub fn set_manifest(ctx: Context<SetManifest>, data: Vec<u8>) -> Result<()> {
        require!(data.len() <= MAX_MANIFEST, VmError::TooLarge);
        let m = &mut ctx.accounts.manifest;
        m.owner = ctx.accounts.owner.key();
        // Monotonic version lets clients resolve concurrent writes from two
        // devices (last/highest version wins).
        m.version = m.version.saturating_add(1);
        m.updated_at = Clock::get()?.unix_timestamp;
        m.bump = ctx.bumps.manifest;
        m.data = data;
        Ok(())
    }

    /// Reclaim the manifest account's rent (closes it) — the owner opting out.
    pub fn close_manifest(_ctx: Context<CloseManifest>) -> Result<()> {
        Ok(())
    }
}

#[derive(Accounts)]
pub struct SetManifest<'info> {
    #[account(
        init_if_needed,
        payer = owner,
        // 8 discriminator + 32 owner + 4 version + 8 updated_at + 1 bump
        // + 4 vec len + MAX_MANIFEST bytes.
        space = 8 + 32 + 4 + 8 + 1 + 4 + MAX_MANIFEST,
        seeds = [b"vm", owner.key().as_ref()],
        bump,
    )]
    pub manifest: Account<'info, VmManifest>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CloseManifest<'info> {
    #[account(
        mut,
        close = owner,
        seeds = [b"vm", owner.key().as_ref()],
        bump = manifest.bump,
        has_one = owner,
    )]
    pub manifest: Account<'info, VmManifest>,
    #[account(mut)]
    pub owner: Signer<'info>,
}

#[account]
pub struct VmManifest {
    /// The identity that owns this manifest (must match the PDA seed + signer).
    pub owner: Pubkey,
    /// Monotonic write counter for last-write-wins across devices.
    pub version: u32,
    pub updated_at: i64,
    pub bump: u8,
    /// Client-encoded manifest bytes (optionally client-encrypted).
    pub data: Vec<u8>,
}

#[error_code]
pub enum VmError {
    #[msg("manifest exceeds the maximum size")]
    TooLarge,
}
