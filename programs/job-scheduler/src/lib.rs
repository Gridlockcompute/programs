use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    self, Mint, Token2022, TokenAccount, TransferChecked,
};

declare_id!("14ZQ7ubKgrWJRhcuzjmUj733fStgwUpERWXMj6pKuYcT");

// ── Constants ─────────────────────────────────────────────────────────────────

pub const MAX_JOB_DURATION_SECS: i64 = 300; // 5-minute SLA window max
pub const ESCROW_SEED: &[u8] = b"job_escrow";

// ── Program ──────────────────────────────────────────────────────────────────

#[program]
pub mod job_scheduler {
    use super::*;

    /// Customer opens a job: pays upfront into escrow.
    ///
    /// Payment stays in escrow until SLAEnforcer settles:
    ///   - SLA met  → FeeCollector gets payment (60/20/10/10 split)
    ///   - SLA miss → customer gets penalty back, remainder to FeeCollector
    pub fn open_job(ctx: Context<OpenJob>, args: OpenJobArgs) -> Result<()> {
        require!(
            args.sla_tier == "realtime"
                || args.sla_tier == "standard"
                || args.sla_tier == "batch"
                || args.sla_tier == "confidential",
            ErrorCode::InvalidSlaTier,
        );

        // Lock payment into escrow via customer signature
        token_interface::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from:      ctx.accounts.customer_wallet.to_account_info(),
                    mint:      ctx.accounts.lock_mint.to_account_info(),
                    to:        ctx.accounts.job_escrow.to_account_info(),
                    authority: ctx.accounts.customer.to_account_info(),
                },
            ),
            args.payment_amount,
            ctx.accounts.lock_mint.decimals,
        )?;

        let job = &mut ctx.accounts.job;
        job.job_id = args.job_id;
        job.customer = ctx.accounts.customer.key();
        job.worker = Pubkey::default(); // assigned after router picks worker
        job.sla_tier = args.sla_tier;
        job.payment_amount = args.payment_amount;
        job.confidential = args.confidential;
        job.status = JobStatus::Pending;
        job.opened_at = Clock::get()?.unix_timestamp;
        job.assigned_at = 0;
        job.settled_at = 0;
        job.bump = ctx.bumps.job;

        emit!(JobOpened {
            job_id: args.job_id,
            customer: ctx.accounts.customer.key(),
            sla_tier: job.sla_tier.clone(),
            payment_amount: args.payment_amount,
            confidential: args.confidential,
        });

        Ok(())
    }

    /// Router assigns a worker to a pending job.
    ///
    /// Only the Router keypair (off-chain service) may call this.
    /// Once assigned, the job enters Active status and the SLA clock starts.
    pub fn assign_worker(
        ctx: Context<AssignWorker>,
        job_id: [u8; 32],
        worker_pubkey: Pubkey,
    ) -> Result<()> {
        let job = &mut ctx.accounts.job;
        require!(job.job_id == job_id, ErrorCode::JobIdMismatch);
        require!(
            job.status == JobStatus::Pending,
            ErrorCode::InvalidJobStatus
        );

        job.worker = worker_pubkey;
        job.status = JobStatus::Active;
        job.assigned_at = Clock::get()?.unix_timestamp;

        emit!(JobAssigned {
            job_id,
            worker: worker_pubkey,
        });

        Ok(())
    }

    /// Settle a completed job — called after SLAEnforcer confirms the outcome.
    ///
    /// On SLA met:  escrow → FeeCollector
    /// On SLA miss: escrow minus penalty → FeeCollector; penalty already
    ///              transferred by SLAEnforcer via PermanentDelegate.
    pub fn settle_job(
        ctx: Context<SettleJob>,
        job_id: [u8; 32],
        sla_met: bool,
        penalty_deducted: u64,
    ) -> Result<()> {
        let job = &mut ctx.accounts.job;
        require!(job.job_id == job_id, ErrorCode::JobIdMismatch);
        require!(job.status == JobStatus::Active, ErrorCode::InvalidJobStatus);

        let transfer_amount = if sla_met {
            job.payment_amount
        } else {
            job.payment_amount.saturating_sub(penalty_deducted)
        };

        let scheduler_seeds: &[&[u8]] = &[b"job_scheduler", &[ctx.bumps.scheduler_authority]];
        let signer = &[scheduler_seeds];

        // Transfer escrow balance to FeeCollector vault
        if transfer_amount > 0 {
            token_interface::transfer_checked(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.key(),
                    TransferChecked {
                        from:      ctx.accounts.job_escrow.to_account_info(),
                        mint:      ctx.accounts.lock_mint.to_account_info(),
                        to:        ctx.accounts.fee_vault.to_account_info(),
                        authority: ctx.accounts.scheduler_authority.to_account_info(),
                    },
                    signer,
                ),
                transfer_amount,
                ctx.accounts.lock_mint.decimals,
            )?;
        }

        job.status = JobStatus::Settled;
        job.settled_at = Clock::get()?.unix_timestamp;

        emit!(JobSettled {
            job_id,
            sla_met,
            penalty_deducted,
            transfer_to_fee_collector: transfer_amount,
        });

        Ok(())
    }

    /// Expire a job that was never assigned or ran past the SLA window.
    /// Refunds full escrow to the customer; called permissionlessly.
    pub fn expire_job(ctx: Context<ExpireJob>) -> Result<()> {
        let job = &ctx.accounts.job;
        let now = Clock::get()?.unix_timestamp;
        require!(
            now - job.opened_at > MAX_JOB_DURATION_SECS,
            ErrorCode::JobNotExpired
        );
        require!(
            job.status == JobStatus::Pending || job.status == JobStatus::Active,
            ErrorCode::InvalidJobStatus
        );

        let scheduler_seeds: &[&[u8]] = &[b"job_scheduler", &[ctx.bumps.scheduler_authority]];
        let signer = &[scheduler_seeds];

        let refund = ctx.accounts.job_escrow.amount;
        if refund > 0 {
            token_interface::transfer_checked(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.key(),
                    TransferChecked {
                        from:      ctx.accounts.job_escrow.to_account_info(),
                        mint:      ctx.accounts.lock_mint.to_account_info(),
                        to:        ctx.accounts.customer_wallet.to_account_info(),
                        authority: ctx.accounts.scheduler_authority.to_account_info(),
                    },
                    signer,
                ),
                refund,
                ctx.accounts.lock_mint.decimals,
            )?;
        }

        emit!(JobExpired {
            job_id: job.job_id,
            refund,
        });

        Ok(())
    }
}

// ── Accounts ─────────────────────────────────────────────────────────────────

#[derive(Accounts)]
#[instruction(args: OpenJobArgs)]
pub struct OpenJob<'info> {
    #[account(mut)]
    pub customer: Signer<'info>,

    #[account(
        init,
        payer = customer,
        space = ServingJob::LEN,
        seeds = [b"job", args.job_id.as_ref()],
        bump,
    )]
    pub job: Account<'info, ServingJob>,

    /// Escrow PDA that holds payment until settlement
    #[account(
        init,
        payer = customer,
        token::mint = lock_mint,
        token::authority = scheduler_authority,
        seeds = [ESCROW_SEED, args.job_id.as_ref()],
        bump,
    )]
    pub job_escrow: InterfaceAccount<'info, TokenAccount>,

    /// JobScheduler PDA — escrow authority
    #[account(seeds = [b"job_scheduler"], bump)]
    pub scheduler_authority: SystemAccount<'info>,

    #[account(mut)]
    pub customer_wallet: InterfaceAccount<'info, TokenAccount>,

    pub lock_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(job_id: [u8; 32])]
pub struct AssignWorker<'info> {
    /// Off-chain Router keypair
    pub router: Signer<'info>,

    #[account(mut, seeds = [b"job", job_id.as_ref()], bump = job.bump)]
    pub job: Account<'info, ServingJob>,
}

#[derive(Accounts)]
#[instruction(job_id: [u8; 32], sla_met: bool, penalty_deducted: u64)]
pub struct SettleJob<'info> {
    /// SLA Enforcer PDA — signs via CPI when releasing escrow
    #[account(
        seeds = [b"sla_enforcer"],
        bump,
    )]
    pub enforcer_authority: Signer<'info>,

    #[account(seeds = [b"job_scheduler"], bump)]
    pub scheduler_authority: SystemAccount<'info>,

    #[account(mut, seeds = [b"job", job_id.as_ref()], bump = job.bump)]
    pub job: Account<'info, ServingJob>,

    #[account(mut, seeds = [ESCROW_SEED, job_id.as_ref()], bump)]
    pub job_escrow: InterfaceAccount<'info, TokenAccount>,

    /// FeeCollector's inbound vault
    #[account(mut)]
    pub fee_vault: InterfaceAccount<'info, TokenAccount>,

    pub lock_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Program<'info, Token2022>,
}

#[derive(Accounts)]
pub struct ExpireJob<'info> {
    pub caller: Signer<'info>,

    #[account(seeds = [b"job_scheduler"], bump)]
    pub scheduler_authority: SystemAccount<'info>,

    #[account(mut)]
    pub job: Account<'info, ServingJob>,

    #[account(mut)]
    pub job_escrow: InterfaceAccount<'info, TokenAccount>,

    #[account(mut)]
    pub customer_wallet: InterfaceAccount<'info, TokenAccount>,

    pub lock_mint: InterfaceAccount<'info, Mint>,
    pub token_program: Program<'info, Token2022>,
}

// ── State ─────────────────────────────────────────────────────────────────────

#[account]
pub struct ServingJob {
    pub job_id: [u8; 32],
    pub customer: Pubkey,
    pub worker: Pubkey,
    pub sla_tier: String,
    pub payment_amount: u64,
    pub confidential: bool,
    pub status: JobStatus,
    pub opened_at: i64,
    pub assigned_at: i64,
    pub settled_at: i64,
    pub bump: u8,
}

impl ServingJob {
    pub const LEN: usize = 8     // discriminator
        + 32                     // job_id
        + 32 + 32                // customer, worker
        + 4 + 16                 // sla_tier string (max 16 chars)
        + 8                      // payment_amount
        + 1                      // confidential
        + 1                      // status
        + 8 + 8 + 8              // opened_at, assigned_at, settled_at
        + 1;                     // bump
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum JobStatus {
    Pending,   // escrow funded, awaiting worker assignment
    Active,    // worker assigned, inference running
    Settled,   // SLAEnforcer settled the outcome
    Expired,   // timed out, customer refunded
}

// ── Args ──────────────────────────────────────────────────────────────────────

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct OpenJobArgs {
    pub job_id: [u8; 32],
    pub sla_tier: String,
    pub payment_amount: u64,
    pub confidential: bool,
}

// ── Events ────────────────────────────────────────────────────────────────────

#[event]
pub struct JobOpened {
    pub job_id: [u8; 32],
    pub customer: Pubkey,
    pub sla_tier: String,
    pub payment_amount: u64,
    pub confidential: bool,
}

#[event]
pub struct JobAssigned {
    pub job_id: [u8; 32],
    pub worker: Pubkey,
}

#[event]
pub struct JobSettled {
    pub job_id: [u8; 32],
    pub sla_met: bool,
    pub penalty_deducted: u64,
    pub transfer_to_fee_collector: u64,
}

#[event]
pub struct JobExpired {
    pub job_id: [u8; 32],
    pub refund: u64,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[error_code]
pub enum ErrorCode {
    #[msg("Invalid SLA tier — must be realtime, standard, batch, or confidential")]
    InvalidSlaTier,
    #[msg("Job ID mismatch")]
    JobIdMismatch,
    #[msg("Invalid job status for this operation")]
    InvalidJobStatus,
    #[msg("Job has not yet expired")]
    JobNotExpired,
}
