use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    self, Mint, Token2022, TokenAccount, TransferChecked,
};

declare_id!("6GoaeiUQC8DaLXSDjd6CPACZZ1rM4xL4VCzxu1iC5xoU");

// ── Constants ─────────────────────────────────────────────────────────────────

pub const STAKER_BPS:   u64 = 6000; // 60%
pub const WORKER_BPS:   u64 = 2000; // 20%
pub const BURN_BPS:     u64 = 1000; // 10%
pub const TREASURY_BPS: u64 = 1000; // 10%

pub const INTEREST_APY_BPS: u64 = 800; // 8% APY on staked LOCK
pub const TRANSFER_FEE_BPS: u64 = 10;  // 0.1% transfer hook
pub const UNSTAKE_COOLDOWN_SECS: i64 = 7 * 86400;

// ── Program ──────────────────────────────────────────────────────────────────

#[program]
pub mod fee_collector {
    use super::*;

    /// Collect and split fees from a completed job.
    ///
    /// Revenue split: 60% stakers / 20% serving worker / 10% burn / 10% treasury
    /// Called by JobScheduler after SLAEnforcer confirms settlement.
    pub fn distribute_fees(ctx: Context<DistributeFees>, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::ZeroAmount);

        let staker_share   = amount * STAKER_BPS   / 10_000;
        let worker_share   = amount * WORKER_BPS   / 10_000;
        let burn_share     = amount * BURN_BPS     / 10_000;
        let treasury_share = amount.saturating_sub(staker_share + worker_share + burn_share);

        let collector_seeds: &[&[u8]] = &[b"fee_collector", &[ctx.bumps.collector_authority]];
        let signer = &[collector_seeds];

        // ── 60% → staker pool (interest-bearing vault, distributed via epoch) ──
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from:      ctx.accounts.fee_vault.to_account_info(),
                    mint:      ctx.accounts.lock_mint.to_account_info(),
                    to:        ctx.accounts.staker_pool.to_account_info(),
                    authority: ctx.accounts.collector_authority.to_account_info(),
                },
                signer,
            ),
            staker_share,
            ctx.accounts.lock_mint.decimals,
        )?;

        // ── 20% → serving worker ───────────────────────────────────────────────
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from:      ctx.accounts.fee_vault.to_account_info(),
                    mint:      ctx.accounts.lock_mint.to_account_info(),
                    to:        ctx.accounts.worker_payout.to_account_info(),
                    authority: ctx.accounts.collector_authority.to_account_info(),
                },
                signer,
            ),
            worker_share,
            ctx.accounts.lock_mint.decimals,
        )?;

        // ── 10% → treasury ────────────────────────────────────────────────────
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from:      ctx.accounts.fee_vault.to_account_info(),
                    mint:      ctx.accounts.lock_mint.to_account_info(),
                    to:        ctx.accounts.treasury.to_account_info(),
                    authority: ctx.accounts.collector_authority.to_account_info(),
                },
                signer,
            ),
            treasury_share,
            ctx.accounts.lock_mint.decimals,
        )?;

        // ── 10% → burn address (zero-address token account) ───────────────────
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from:      ctx.accounts.fee_vault.to_account_info(),
                    mint:      ctx.accounts.lock_mint.to_account_info(),
                    to:        ctx.accounts.burn_vault.to_account_info(),
                    authority: ctx.accounts.collector_authority.to_account_info(),
                },
                signer,
            ),
            burn_share,
            ctx.accounts.lock_mint.decimals,
        )?;

        emit!(FeesDistributed {
            amount,
            staker_share,
            worker_share,
            treasury_share,
            burn_share,
        });

        Ok(())
    }

    /// Distribute staking rewards at the end of an epoch (7 days).
    ///
    /// Iterates over all staker accounts and credits pro-rata LOCK.
    /// APY = 8% paid weekly (8% / 52 weeks ≈ 0.1538% per epoch).
    pub fn distribute_epoch_rewards(ctx: Context<DistributeEpochRewards>) -> Result<()> {
        let state = &mut ctx.accounts.epoch_state;
        let now = Clock::get()?.unix_timestamp;
        require!(
            now - state.last_epoch_at >= 7 * 86400,
            ErrorCode::EpochNotReady
        );

        // Transfer pro-rata epoch reward from staker pool to individual staker
        let epoch_apy_bps = INTEREST_APY_BPS / 52; // weekly
        let reward = ctx.accounts.staker_vault.amount * epoch_apy_bps / 10_000;

        let collector_seeds: &[&[u8]] = &[b"fee_collector", &[ctx.bumps.collector_authority]];
        let signer = &[collector_seeds];

        if reward > 0 {
            token_interface::transfer_checked(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.key(),
                    TransferChecked {
                        from:      ctx.accounts.staker_pool.to_account_info(),
                        mint:      ctx.accounts.lock_mint.to_account_info(),
                        to:        ctx.accounts.staker_vault.to_account_info(),
                        authority: ctx.accounts.collector_authority.to_account_info(),
                    },
                    signer,
                ),
                reward,
                ctx.accounts.lock_mint.decimals,
            )?;
        }

        state.last_epoch_at = now;
        state.epoch_count = state.epoch_count.saturating_add(1);

        emit!(EpochRewardPaid {
            staker: ctx.accounts.staker_vault.owner,
            reward,
            epoch: state.epoch_count,
        });

        Ok(())
    }

    /// Begin unstake cooldown — locks the amount until claim_unstake.
    pub fn request_unstake(ctx: Context<RequestUnstake>, amount: u64) -> Result<()> {
        require!(amount > 0, ErrorCode::ZeroAmount);
        require!(
            ctx.accounts.staker_vault.amount >= amount,
            ErrorCode::InsufficientStake
        );

        let position = &mut ctx.accounts.stake_position;
        if position.owner != Pubkey::default() && position.owner != ctx.accounts.owner.key() {
            return Err(ErrorCode::Unauthorized.into());
        }
        require!(position.pending_unstake == 0, ErrorCode::UnstakeAlreadyPending);

        let now = Clock::get()?.unix_timestamp;
        position.owner = ctx.accounts.owner.key();
        position.pending_unstake = amount;
        position.unstake_available_at = now + UNSTAKE_COOLDOWN_SECS;
        position.bump = ctx.bumps.stake_position;

        emit!(UnstakeRequested {
            owner: position.owner,
            amount,
            available_at: position.unstake_available_at,
        });

        Ok(())
    }

    /// After cooldown, withdraw pending unstake from the staker vault PDA.
    pub fn claim_unstake(ctx: Context<ClaimUnstake>) -> Result<()> {
        let position = &mut ctx.accounts.stake_position;
        require!(position.pending_unstake > 0, ErrorCode::NoPendingUnstake);

        let now = Clock::get()?.unix_timestamp;
        require!(now >= position.unstake_available_at, ErrorCode::CooldownActive);

        let amount = position.pending_unstake;
        let owner_key = position.owner;
        let bump = ctx.bumps.staker_vault_authority;
        let vault_seeds: &[&[u8]] = &[b"staker_vault", owner_key.as_ref(), &[bump]];
        let signer = &[vault_seeds];

        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.staker_vault.to_account_info(),
                    mint: ctx.accounts.lock_mint.to_account_info(),
                    to: ctx.accounts.owner_lock_ata.to_account_info(),
                    authority: ctx.accounts.staker_vault_authority.to_account_info(),
                },
                signer,
            ),
            amount,
            ctx.accounts.lock_mint.decimals,
        )?;

        position.pending_unstake = 0;
        position.unstake_available_at = 0;

        emit!(UnstakeClaimed {
            owner: owner_key,
            amount,
        });

        Ok(())
    }
}

// ── Accounts ─────────────────────────────────────────────────────────────────

#[derive(Accounts)]
pub struct DistributeFees<'info> {
    /// FeeCollector PDA — authority over fee_vault
    #[account(seeds = [b"fee_collector"], bump)]
    pub collector_authority: SystemAccount<'info>,

    /// Vault that receives inbound job fees before splitting
    #[account(mut)]
    pub fee_vault: InterfaceAccount<'info, TokenAccount>,

    /// 60% staker reward pool
    #[account(mut)]
    pub staker_pool: InterfaceAccount<'info, TokenAccount>,

    /// 20% goes directly to the worker's payout wallet
    #[account(mut)]
    pub worker_payout: InterfaceAccount<'info, TokenAccount>,

    /// 10% treasury
    #[account(mut)]
    pub treasury: InterfaceAccount<'info, TokenAccount>,

    /// 10% burn (effectively a black-hole token account)
    #[account(mut)]
    pub burn_vault: InterfaceAccount<'info, TokenAccount>,

    pub lock_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Program<'info, Token2022>,
}

#[derive(Accounts)]
pub struct DistributeEpochRewards<'info> {
    #[account(seeds = [b"fee_collector"], bump)]
    pub collector_authority: SystemAccount<'info>,

    #[account(
        init_if_needed,
        payer = payer,
        space = EpochState::LEN,
        seeds = [b"epoch_state"],
        bump,
    )]
    pub epoch_state: Account<'info, EpochState>,

    /// Shared staker pool vault (funded by distribute_fees)
    #[account(mut)]
    pub staker_pool: InterfaceAccount<'info, TokenAccount>,

    /// The individual staker's LOCK vault
    #[account(mut)]
    pub staker_vault: InterfaceAccount<'info, TokenAccount>,

    pub lock_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Program<'info, Token2022>,

    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RequestUnstake<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        init_if_needed,
        payer = owner,
        space = StakePosition::LEN,
        seeds = [b"stake_position", owner.key().as_ref()],
        bump,
    )]
    pub stake_position: Account<'info, StakePosition>,

    #[account(
        constraint = staker_vault.owner == staker_vault_authority.key() @ ErrorCode::InvalidStakerVault
    )]
    pub staker_vault: InterfaceAccount<'info, TokenAccount>,

    /// CHECK: PDA authority for the staker vault token account
    #[account(
        seeds = [b"staker_vault", owner.key().as_ref()],
        bump,
    )]
    pub staker_vault_authority: UncheckedAccount<'info>,

    pub lock_mint: InterfaceAccount<'info, Mint>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimUnstake<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"stake_position", owner.key().as_ref()],
        bump = stake_position.bump,
        constraint = stake_position.owner == owner.key() @ ErrorCode::Unauthorized,
    )]
    pub stake_position: Account<'info, StakePosition>,

    /// CHECK: PDA authority for the staker vault token account
    #[account(
        seeds = [b"staker_vault", owner.key().as_ref()],
        bump,
    )]
    pub staker_vault_authority: UncheckedAccount<'info>,

    #[account(
        mut,
        constraint = staker_vault.owner == staker_vault_authority.key() @ ErrorCode::InvalidStakerVault
    )]
    pub staker_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(mut)]
    pub owner_lock_ata: InterfaceAccount<'info, TokenAccount>,

    pub lock_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Program<'info, Token2022>,
}

// ── State ─────────────────────────────────────────────────────────────────────

#[account]
pub struct EpochState {
    pub last_epoch_at: i64,
    pub epoch_count: u64,
    pub bump: u8,
}

impl EpochState {
    pub const LEN: usize = 8 + 8 + 8 + 1;
}

#[account]
pub struct StakePosition {
    pub owner: Pubkey,
    pub pending_unstake: u64,
    pub unstake_available_at: i64,
    pub bump: u8,
}

impl StakePosition {
    pub const LEN: usize = 8 + 32 + 8 + 8 + 1;
}

// ── Events ────────────────────────────────────────────────────────────────────

#[event]
pub struct FeesDistributed {
    pub amount: u64,
    pub staker_share: u64,
    pub worker_share: u64,
    pub treasury_share: u64,
    pub burn_share: u64,
}

#[event]
pub struct EpochRewardPaid {
    pub staker: Pubkey,
    pub reward: u64,
    pub epoch: u64,
}

#[event]
pub struct UnstakeRequested {
    pub owner: Pubkey,
    pub amount: u64,
    pub available_at: i64,
}

#[event]
pub struct UnstakeClaimed {
    pub owner: Pubkey,
    pub amount: u64,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[error_code]
pub enum ErrorCode {
    #[msg("Fee amount must be greater than zero")]
    ZeroAmount,
    #[msg("Epoch not yet complete — 7 days must pass between distributions")]
    EpochNotReady,
    #[msg("Insufficient staked LOCK in vault")]
    InsufficientStake,
    #[msg("An unstake request is already pending")]
    UnstakeAlreadyPending,
    #[msg("No pending unstake to claim")]
    NoPendingUnstake,
    #[msg("Unstake cooldown has not finished")]
    CooldownActive,
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Invalid staker vault for this wallet")]
    InvalidStakerVault,
}
