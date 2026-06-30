use anchor_lang::prelude::*;

declare_id!("5me7JG25p4NH1XCYtxWn9bU5sij8Xos1We5g47TbRxxM");

// ── Constants ─────────────────────────────────────────────────────────────────

/// Challenge window: finalize after this if unchallenged (2s on devnet for fast settlement).
pub const CHALLENGE_WINDOW_SECS: i64 = 2;

// ── Program ──────────────────────────────────────────────────────────────────

#[program]
pub mod sla_registry {
    use super::*;

    /// Commit a latency receipt on-chain after serving a job.
    /// Called by the Router immediately after TTFT is measured.
    pub fn commit_receipt(
        ctx: Context<CommitReceipt>,
        args: LatencyReceiptArgs,
    ) -> Result<()> {
        let rec = &mut ctx.accounts.receipt;
        rec.job_id = args.job_id;
        rec.sla_tier = args.sla_tier.clone();
        rec.ttft_ms = args.ttft_ms;
        rec.tpot_ms = args.tpot_ms;
        rec.router_sig = args.router_sig;
        rec.watcher_sig = None;
        rec.sla_met = args.sla_met;
        rec.confidential = args.confidential;
        rec.attestation_hash = args.attestation_hash;
        rec.committed_at = Clock::get()?.unix_timestamp;
        rec.finalized = false;
        rec.bump = ctx.bumps.receipt;

        emit!(ReceiptCommitted {
            job_id: args.job_id,
            sla_tier: args.sla_tier,
            ttft_ms: args.ttft_ms,
            sla_met: args.sla_met,
        });
        Ok(())
    }

    /// Watcher node submits an independent latency measurement.
    /// If it disagrees with the router by >50ms → dispute triggers.
    pub fn sample_verify(
        ctx: Context<SampleVerify>,
        job_id: [u8; 32],
        watcher_ttft: u32,
        watcher_tpot: u32,
    ) -> Result<()> {
        let rec = &mut ctx.accounts.receipt;
        require!(!rec.finalized, ErrorCode::AlreadyFinalized);
        require!(rec.job_id == job_id, ErrorCode::JobIdMismatch);

        let delta = (watcher_ttft as i64 - rec.ttft_ms as i64).unsigned_abs() as u32;
        if delta > 50 {
            // Dispute: router or watcher is lying — slash the dishonest party
            emit!(SlaDispute {
                job_id,
                router_ttft: rec.ttft_ms,
                watcher_ttft,
                delta_ms: delta,
            });
            return err!(ErrorCode::MeasurementDispute);
        }

        // Agreement — watcher confirms the measurement
        rec.watcher_sig = Some(ctx.accounts.watcher.key().to_bytes());
        // Re-evaluate SLA met with watcher-confirmed numbers
        let (target_ttft, target_tpot) = sla_targets(&rec.sla_tier)?;
        rec.sla_met = watcher_ttft <= target_ttft && watcher_tpot <= target_tpot;
        rec.finalized = true;

        emit!(ReceiptVerified {
            job_id,
            sla_met: rec.sla_met,
            watcher: ctx.accounts.watcher.key(),
        });
        Ok(())
    }

    /// Finalize a receipt that was not sampled after the challenge window.
    /// Any permissionless crank can call this.
    pub fn finalize_unchallenged(ctx: Context<FinalizeUnchallenged>) -> Result<()> {
        let rec = &mut ctx.accounts.receipt;
        require!(!rec.finalized, ErrorCode::AlreadyFinalized);
        let now = Clock::get()?.unix_timestamp;
        require!(
            now - rec.committed_at > CHALLENGE_WINDOW_SECS,
            ErrorCode::ChallengeWindowOpen
        );
        rec.finalized = true;
        Ok(())
    }
}

// ── Accounts ─────────────────────────────────────────────────────────────────

#[derive(Accounts)]
#[instruction(args: LatencyReceiptArgs)]
pub struct CommitReceipt<'info> {
    #[account(mut)]
    pub router: Signer<'info>,
    #[account(
        init,
        payer = router,
        space = ReceiptAccount::LEN,
        seeds = [b"receipt", args.job_id.as_ref()],
        bump,
    )]
    pub receipt: Account<'info, ReceiptAccount>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(job_id: [u8; 32])]
pub struct SampleVerify<'info> {
    pub watcher: Signer<'info>,
    #[account(mut, seeds = [b"receipt", job_id.as_ref()], bump = receipt.bump)]
    pub receipt: Account<'info, ReceiptAccount>,
}

#[derive(Accounts)]
pub struct FinalizeUnchallenged<'info> {
    pub caller: Signer<'info>,
    #[account(mut)]
    pub receipt: Account<'info, ReceiptAccount>,
}

// ── State ─────────────────────────────────────────────────────────────────────

#[account]
pub struct ReceiptAccount {
    pub job_id: [u8; 32],
    pub sla_tier: String,           // "realtime" | "standard" | "batch" | "confidential"
    pub ttft_ms: u32,
    pub tpot_ms: u32,
    pub router_sig: [u8; 64],
    pub watcher_sig: Option<[u8; 32]>,
    pub sla_met: bool,
    pub confidential: bool,
    pub attestation_hash: Option<[u8; 32]>,
    pub committed_at: i64,
    pub finalized: bool,
    pub bump: u8,
}

impl ReceiptAccount {
    pub const LEN: usize = 8     // discriminator
        + 32                     // job_id
        + 4 + 16                 // sla_tier string
        + 4 + 4                  // ttft_ms, tpot_ms
        + 64                     // router_sig
        + 1 + 32                 // watcher_sig (Option)
        + 1 + 1                  // sla_met, confidential
        + 1 + 32                 // attestation_hash (Option)
        + 8 + 1 + 1;             // committed_at, finalized, bump
}

// ── Args ──────────────────────────────────────────────────────────────────────

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct LatencyReceiptArgs {
    pub job_id: [u8; 32],
    pub sla_tier: String,
    pub ttft_ms: u32,
    pub tpot_ms: u32,
    pub router_sig: [u8; 64],
    pub sla_met: bool,
    pub confidential: bool,
    pub attestation_hash: Option<[u8; 32]>,
}

// ── Events ────────────────────────────────────────────────────────────────────

#[event]
pub struct ReceiptCommitted {
    pub job_id: [u8; 32],
    pub sla_tier: String,
    pub ttft_ms: u32,
    pub sla_met: bool,
}

#[event]
pub struct ReceiptVerified {
    pub job_id: [u8; 32],
    pub sla_met: bool,
    pub watcher: Pubkey,
}

#[event]
pub struct SlaDispute {
    pub job_id: [u8; 32],
    pub router_ttft: u32,
    pub watcher_ttft: u32,
    pub delta_ms: u32,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn sla_targets(tier: &str) -> Result<(u32, u32)> {
    match tier {
        "realtime"     => Ok((300, 60)),
        "standard"     => Ok((800, 120)),
        "batch"        => Ok((5000, 9999)),
        "confidential" => Ok((800, 120)),
        _ => err!(ErrorCode::UnknownSlaTier),
    }
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[error_code]
pub enum ErrorCode {
    #[msg("Receipt is already finalized")]
    AlreadyFinalized,
    #[msg("Job ID mismatch")]
    JobIdMismatch,
    #[msg("Router and watcher measurements disagree by more than 50ms — dispute")]
    MeasurementDispute,
    #[msg("Challenge window is still open")]
    ChallengeWindowOpen,
    #[msg("Unknown SLA tier")]
    UnknownSlaTier,
}
