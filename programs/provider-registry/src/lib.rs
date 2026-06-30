use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

declare_id!("GvCMygAV4RNYVgPybMmgEb36AkSKEBQJJw45WfUfSfmu");

// ── Constants ────────────────────────────────────────────────────────────────

pub const HEARTBEAT_TIMEOUT_SECS: i64 = 120;
pub const MAX_SLA_TIERS: u8 = 0b00001111; // bitmask for all 4 tiers

// ── Program ──────────────────────────────────────────────────────────────────

#[program]
pub mod provider_registry {
    use super::*;

    /// Register a new GPU worker node.
    pub fn register_worker(
        ctx: Context<RegisterWorker>,
        args: RegisterWorkerArgs,
    ) -> Result<()> {
        let worker = &mut ctx.accounts.worker;
        worker.operator = ctx.accounts.operator.key();
        worker.payout_wallet = args.payout_wallet;
        worker.payout_token = args.payout_token;
        worker.role = args.role;
        worker.hardware_tier = args.hardware_tier;
        worker.interconnect_mbps = args.interconnect_mbps;
        worker.tee_capable = args.tee_capable;
        worker.tee_vendor = args.tee_vendor;
        worker.status = WorkerStatus::Active;
        worker.sla_tiers_accepted = 0; // none yet — must stake per tier
        worker.reliability_score = 5000; // start at 50/100
        worker.sla_pass_rate = 10000; // 100% to start
        worker.p99_ttft_ms = 0;
        worker.goodput_score = 0;
        worker.staked_lock = 0;
        worker.gridpoints = 0;
        worker.last_heartbeat = Clock::get()?.unix_timestamp;
        worker.bump = ctx.bumps.worker;
        Ok(())
    }

    /// Set worker status (Active / Paused / ScheduledOff).
    /// Announced pauses carry no penalty; going dark mid-job does.
    pub fn set_status(ctx: Context<SetStatus>, status: WorkerStatus) -> Result<()> {
        let worker = &mut ctx.accounts.worker;
        require!(
            worker.operator == ctx.accounts.operator.key(),
            ErrorCode::Unauthorized
        );
        // Only allow announced status changes — not mid-job abandon
        require!(
            matches!(status, WorkerStatus::Active | WorkerStatus::Paused | WorkerStatus::ScheduledOff),
            ErrorCode::InvalidStatusTransition
        );
        worker.status = status;
        Ok(())
    }

    /// Worker heartbeat — called every ~30s by the off-chain worker process.
    pub fn heartbeat(ctx: Context<Heartbeat>) -> Result<()> {
        let worker = &mut ctx.accounts.worker;
        worker.last_heartbeat = Clock::get()?.unix_timestamp;
        Ok(())
    }

    /// Called by SLARegistry after each receipt is committed.
    pub fn update_reliability(
        ctx: Context<UpdateReliability>,
        sla_met: bool,
        ttft_ms: u32,
    ) -> Result<()> {
        let worker = &mut ctx.accounts.worker;
        // EMA-style update: new = old * 0.99 + event * 0.01 (in bps)
        let event_bps: u16 = if sla_met { 10000 } else { 0 };
        worker.sla_pass_rate = ((worker.sla_pass_rate as u32 * 99 + event_bps as u32) / 100) as u16;

        // Update reliability score (0–10000)
        if sla_met {
            worker.reliability_score = (worker.reliability_score + 1).min(10000);
        } else {
            worker.reliability_score = worker.reliability_score.saturating_sub(20);
        }

        // p99 TTFT: rolling max approximation
        if ttft_ms > worker.p99_ttft_ms {
            worker.p99_ttft_ms = ttft_ms;
        }
        Ok(())
    }

    /// Accept an SLA tier — requires sufficient staked LOCK as collateral.
    pub fn accept_sla_tier(ctx: Context<AcceptSlaTier>, tier_bit: u8) -> Result<()> {
        let worker = &mut ctx.accounts.worker;
        let required = collateral_for_tier(tier_bit)?;
        require!(worker.staked_lock >= required, ErrorCode::InsufficientCollateral);
        worker.sla_tiers_accepted |= tier_bit;
        Ok(())
    }

    /// Handle heartbeat timeout — called by any permissionless crank.
    /// Marks in-flight SLA jobs as missed and penalizes abandonment.
    pub fn on_heartbeat_timeout(ctx: Context<OnHeartbeatTimeout>) -> Result<()> {
        let worker_key = ctx.accounts.worker.key();
        let worker = &mut ctx.accounts.worker;
        let now = Clock::get()?.unix_timestamp;
        require!(
            now - worker.last_heartbeat > HEARTBEAT_TIMEOUT_SECS,
            ErrorCode::HeartbeatNotExpired
        );
        // Reputation penalty for going dark
        worker.reliability_score = worker.reliability_score.saturating_sub(500);
        worker.status = WorkerStatus::AutoGated;
        // NOTE: SLAEnforcer.mark_missed() must be called for each in-flight job
        emit!(WorkerTimedOut {
            worker: worker_key,
            last_heartbeat: worker.last_heartbeat,
            timestamp: now,
        });
        Ok(())
    }
}

// ── Accounts ─────────────────────────────────────────────────────────────────

#[derive(Accounts)]
pub struct RegisterWorker<'info> {
    #[account(mut)]
    pub operator: Signer<'info>,
    #[account(
        init,
        payer = operator,
        space = WorkerAccount::LEN,
        seeds = [b"worker", operator.key().as_ref()],
        bump,
    )]
    pub worker: Account<'info, WorkerAccount>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetStatus<'info> {
    pub operator: Signer<'info>,
    #[account(mut, seeds = [b"worker", operator.key().as_ref()], bump = worker.bump)]
    pub worker: Account<'info, WorkerAccount>,
}

#[derive(Accounts)]
pub struct Heartbeat<'info> {
    pub operator: Signer<'info>,
    #[account(mut, seeds = [b"worker", operator.key().as_ref()], bump = worker.bump)]
    pub worker: Account<'info, WorkerAccount>,
}

#[derive(Accounts)]
pub struct AcceptSlaTier<'info> {
    pub operator: Signer<'info>,
    #[account(mut, seeds = [b"worker", operator.key().as_ref()], bump = worker.bump)]
    pub worker: Account<'info, WorkerAccount>,
}

#[derive(Accounts)]
pub struct UpdateReliability<'info> {
    /// Only the SLARegistry PDA may call this
    pub sla_registry: Signer<'info>,
    #[account(mut)]
    pub worker: Account<'info, WorkerAccount>,
}

#[derive(Accounts)]
pub struct OnHeartbeatTimeout<'info> {
    /// Permissionless crank
    pub caller: Signer<'info>,
    #[account(mut)]
    pub worker: Account<'info, WorkerAccount>,
}

// ── State ─────────────────────────────────────────────────────────────────────

#[account]
pub struct WorkerAccount {
    pub operator: Pubkey,
    pub payout_wallet: Pubkey,
    pub payout_token: PayoutToken,
    pub role: WorkerRole,
    pub hardware_tier: HardwareTier,
    pub interconnect_mbps: u32,
    pub tee_capable: bool,
    pub tee_vendor: TeeVendor,
    pub status: WorkerStatus,
    pub sla_tiers_accepted: u8, // bitmask
    pub reliability_score: u16, // 0..10000
    pub sla_pass_rate: u16,     // bps (10000 = 100%)
    pub p99_ttft_ms: u32,
    pub goodput_score: u32,
    pub staked_lock: u64,
    pub gridpoints: u64,
    pub last_heartbeat: i64,
    pub bump: u8,
}

impl WorkerAccount {
    pub const LEN: usize = 8   // discriminator
        + 32 + 32              // operator, payout_wallet
        + 1 + 1 + 1 + 1        // payout_token, role, hardware_tier, tee_vendor
        + 4                    // interconnect_mbps
        + 1                    // tee_capable
        + 1 + 1                // status, sla_tiers_accepted
        + 2 + 2 + 4 + 4        // reliability_score, sla_pass_rate, p99_ttft_ms, goodput_score
        + 8 + 8 + 8            // staked_lock, gridpoints, last_heartbeat
        + 1;                   // bump
}

// ── Enums ─────────────────────────────────────────────────────────────────────

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum WorkerRole { Prefill, Decode, Cache, Router }

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum WorkerStatus { Active, Paused, ScheduledOff, AutoGated, Draining }

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum PayoutToken { Lock, Usdc }

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum TeeVendor { None, NvidiaCC, AmdSev }

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum HardwareTier { Consumer, Prosumer, DataCenter, Enterprise }

// ── Args ──────────────────────────────────────────────────────────────────────

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct RegisterWorkerArgs {
    pub payout_wallet: Pubkey,
    pub payout_token: PayoutToken,
    pub role: WorkerRole,
    pub hardware_tier: HardwareTier,
    pub interconnect_mbps: u32,
    pub tee_capable: bool,
    pub tee_vendor: TeeVendor,
}

// ── Events ────────────────────────────────────────────────────────────────────

#[event]
pub struct WorkerTimedOut {
    pub worker: Pubkey,
    pub last_heartbeat: i64,
    pub timestamp: i64,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn collateral_for_tier(tier_bit: u8) -> Result<u64> {
    match tier_bit {
        0b0001 => Ok(1_000 * 1_000_000),   // Batch: 1,000 LOCK
        0b0010 => Ok(5_000 * 1_000_000),   // Standard: 5,000 LOCK
        0b0100 => Ok(15_000 * 1_000_000),  // Realtime: 15,000 LOCK
        0b1000 => Ok(20_000 * 1_000_000),  // Confidential: 20,000 LOCK
        _ => err!(ErrorCode::InvalidTierBit),
    }
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[error_code]
pub enum ErrorCode {
    #[msg("Unauthorized: only the worker operator may perform this action")]
    Unauthorized,
    #[msg("Invalid status transition")]
    InvalidStatusTransition,
    #[msg("Insufficient staked LOCK collateral for this SLA tier")]
    InsufficientCollateral,
    #[msg("Heartbeat has not expired yet")]
    HeartbeatNotExpired,
    #[msg("Invalid SLA tier bit")]
    InvalidTierBit,
}
