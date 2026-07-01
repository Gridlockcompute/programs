use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    self, Mint, Token2022, TokenAccount, TransferChecked,
};

declare_id!("3H3yLvY7m7TaGkMSvvkvG9NQT5nDhVLNrZTfywiBaoLJ");

pub mod job_scheduler {
    use super::*;
    declare_id!("14ZQ7ubKgrWJRhcuzjmUj733fStgwUpERWXMj6pKuYcT");
}

// ── Program ──────────────────────────────────────────────────────────────────

#[program]
pub mod sla_enforcer {
    use super::*;

    pub fn settle_or_penalize(ctx: Context<SettleOrPenalize>, job_id: [u8; 32]) -> Result<()> {
        let receipt = load_receipt(&ctx.accounts.receipt.to_account_info())?;
        require!(receipt.finalized, ErrorCode::ReceiptNotFinalized);
        require!(receipt.job_id == job_id, ErrorCode::JobIdMismatch);

        let job = load_job(&ctx.accounts.job.to_account_info())?;
        require!(job.job_id == job_id, ErrorCode::JobIdMismatch);

        let mut penalty_deducted: u64 = 0;

        if receipt.sla_met {
            emit!(JobSettled {
                job_id,
                sla_met: true,
                penalty_amount: 0,
            });
            msg!("SLA met — releasing escrow to FeeCollector");
        } else {
            penalty_deducted = penalty_for_tier(&receipt.sla_tier, job.payment_amount)?;

            let actual_penalty = if ctx.accounts.worker_stake.amount >= penalty_deducted {
                penalty_deducted
            } else {
                let shortfall = penalty_deducted - ctx.accounts.worker_stake.amount;
                emit!(InsuranceTopUp { job_id, amount: shortfall });
                ctx.accounts.worker_stake.amount
            };

            if actual_penalty > 0 {
                let enforcer_seeds: &[&[u8]] = &[b"sla_enforcer", &[ctx.bumps.enforcer_authority]];
                let signer = &[enforcer_seeds];

                token_interface::transfer_checked(
                    CpiContext::new_with_signer(
                        ctx.accounts.token_program.key(),
                        TransferChecked {
                            from: ctx.accounts.worker_stake.to_account_info(),
                            mint: ctx.accounts.lock_mint.to_account_info(),
                            to: ctx.accounts.customer_wallet.to_account_info(),
                            authority: ctx.accounts.enforcer_authority.to_account_info(),
                        },
                        signer,
                    ),
                    actual_penalty,
                    ctx.accounts.lock_mint.decimals,
                )?;
            }

            emit!(JobSettled {
                job_id,
                sla_met: false,
                penalty_amount: actual_penalty,
            });

            msg!(
                "SLA MISSED — {} LOCK penalty transferred worker→customer",
                actual_penalty
            );
        }

        // Release escrow remainder to fee vault via JobScheduler CPI
        let enforcer_seeds: &[&[u8]] = &[b"sla_enforcer", &[ctx.bumps.enforcer_authority]];
        let signer = &[enforcer_seeds];

        let mut data = Vec::with_capacity(8 + 32 + 1 + 8);
        data.extend_from_slice(&settle_job_discriminator());
        data.extend_from_slice(&job_id);
        data.push(if receipt.sla_met { 1 } else { 0 });
        data.extend_from_slice(&penalty_deducted.to_le_bytes());

        let ix = anchor_lang::solana_program::instruction::Instruction {
            program_id: job_scheduler::ID,
            accounts: vec![
                AccountMeta::new(ctx.accounts.enforcer_authority.key(), true),
                AccountMeta::new_readonly(ctx.accounts.scheduler_authority.key(), false),
                AccountMeta::new(ctx.accounts.job.key(), false),
                AccountMeta::new(ctx.accounts.job_escrow.key(), false),
                AccountMeta::new(ctx.accounts.fee_vault.key(), false),
                AccountMeta::new_readonly(ctx.accounts.lock_mint.key(), false),
                AccountMeta::new_readonly(ctx.accounts.token_program.key(), false),
            ],
            data,
        };

        anchor_lang::solana_program::program::invoke_signed(
            &ix,
            &[
                ctx.accounts.enforcer_authority.to_account_info(),
                ctx.accounts.scheduler_authority.to_account_info(),
                ctx.accounts.job.to_account_info(),
                ctx.accounts.job_escrow.to_account_info(),
                ctx.accounts.fee_vault.to_account_info(),
                ctx.accounts.lock_mint.to_account_info(),
                ctx.accounts.token_program.to_account_info(),
            ],
            signer,
        )?;

        Ok(())
    }

    pub fn mark_missed(ctx: Context<MarkMissed>, job_id: [u8; 32]) -> Result<()> {
        let receipt_ai = ctx.accounts.receipt.to_account_info();
        let mut receipt = load_receipt(&receipt_ai)?;
        require!(receipt.job_id == job_id, ErrorCode::JobIdMismatch);
        receipt.sla_met = false;
        receipt.finalized = true;
        store_receipt(&receipt_ai, &receipt)?;
        emit!(JobAbandoned { job_id });
        Ok(())
    }
}

use anchor_lang::solana_program::instruction::AccountMeta;

fn settle_job_discriminator() -> [u8; 8] {
    [0xf6, 0x9b, 0xdd, 0x22, 0xa8, 0x46, 0xad, 0x48]
}

// ── Accounts ─────────────────────────────────────────────────────────────────

#[derive(Accounts)]
#[instruction(job_id: [u8; 32])]
pub struct SettleOrPenalize<'info> {
    #[account(mut, seeds = [b"sla_enforcer"], bump)]
    pub enforcer_authority: SystemAccount<'info>,

    /// CHECK: SLA Registry receipt PDA (owner verified; deserialized manually).
    #[account(
        mut,
        owner = sla_registry::ID @ ErrorCode::InvalidReceipt,
    )]
    pub receipt: UncheckedAccount<'info>,

    /// CHECK: JobScheduler job PDA (owner verified; deserialized manually).
    #[account(
        mut,
        owner = job_scheduler::ID @ ErrorCode::InvalidJob,
    )]
    pub job: UncheckedAccount<'info>,

    #[account(mut)]
    pub worker_stake: InterfaceAccount<'info, TokenAccount>,

    #[account(mut)]
    pub customer_wallet: InterfaceAccount<'info, TokenAccount>,

    #[account(
        seeds = [b"job_scheduler"],
        bump,
        seeds::program = job_scheduler::ID,
    )]
    pub scheduler_authority: SystemAccount<'info>,

    #[account(
        mut,
        seeds = [b"job_escrow", job_id.as_ref()],
        bump,
        seeds::program = job_scheduler::ID,
        token::mint = lock_mint,
        token::authority = scheduler_authority,
    )]
    pub job_escrow: InterfaceAccount<'info, TokenAccount>,

    #[account(mut)]
    pub fee_vault: InterfaceAccount<'info, TokenAccount>,

    pub lock_mint: InterfaceAccount<'info, Mint>,

    /// CHECK: JobScheduler program id
    #[account(address = job_scheduler::ID @ ErrorCode::InvalidJobScheduler)]
    pub job_scheduler_program: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token2022>,
}

#[derive(Accounts)]
pub struct MarkMissed<'info> {
    pub provider_registry: Signer<'info>,
    /// CHECK: SLA Registry receipt PDA (owner verified; deserialized manually).
    #[account(mut, owner = sla_registry::ID @ ErrorCode::InvalidReceipt)]
    pub receipt: UncheckedAccount<'info>,
}

pub mod sla_registry {
    use super::*;
    declare_id!("5me7JG25p4NH1XCYtxWn9bU5sij8Xos1We5g47TbRxxM");
}

// ── Mirrored state (must match source programs byte-for-byte) ────────────────

/// Devnet SLARegistry layout (pre attestation_hash / full Option fields).
struct LegacyReceiptAccount {
    job_id: [u8; 32],
    sla_tier: String,
    ttft_ms: u32,
    tpot_ms: u32,
    sla_met: bool,
    confidential: bool,
    committed_at: i64,
    finalized: bool,
    bump: u8,
}

fn parse_legacy_receipt(data: &[u8]) -> Result<LegacyReceiptAccount> {
    if data.len() < 138 {
        return err!(ErrorCode::InvalidReceipt);
    }
    let mut offset = 0usize;
    let mut job_id = [0u8; 32];
    job_id.copy_from_slice(&data[offset..offset + 32]);
    offset += 32;

    let tier_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    if offset + tier_len > data.len() {
        return err!(ErrorCode::InvalidReceipt);
    }
    let sla_tier = String::from_utf8(data[offset..offset + tier_len].to_vec())
        .map_err(|_| error!(ErrorCode::InvalidReceipt))?;
    offset += tier_len;

    if offset + 4 + 4 + 64 + 1 + 1 + 1 + 1 + 8 + 1 + 1 > data.len() {
        return err!(ErrorCode::InvalidReceipt);
    }
    let ttft_ms = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    offset += 4;
    let tpot_ms = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    offset += 4 + 64; // router_sig
    offset += 1; // legacy watcher tag
    let sla_met = data[offset] != 0;
    offset += 1;
    let confidential = data[offset] != 0;
    offset += 2; // legacy padding byte before committed_at
    let committed_at = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let finalized = data[offset] != 0;
    offset += 1;
    let bump = data[offset];

    Ok(LegacyReceiptAccount {
        job_id,
        sla_tier,
        ttft_ms,
        tpot_ms,
        sla_met,
        confidential,
        committed_at,
        finalized,
        bump,
    })
}

fn legacy_to_receipt(legacy: LegacyReceiptAccount) -> ReceiptAccount {
    ReceiptAccount {
        job_id: legacy.job_id,
        sla_tier: legacy.sla_tier,
        ttft_ms: legacy.ttft_ms,
        tpot_ms: legacy.tpot_ms,
        router_sig: [0u8; 64],
        watcher_sig: None,
        sla_met: legacy.sla_met,
        confidential: legacy.confidential,
        attestation_hash: None,
        committed_at: legacy.committed_at,
        finalized: legacy.finalized,
        bump: legacy.bump,
    }
}

#[account]
pub struct ReceiptAccount {
    pub job_id: [u8; 32],
    pub sla_tier: String,
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

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum JobStatus {
    Pending,
    Active,
    Settled,
    Expired,
}

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

// ── Events ────────────────────────────────────────────────────────────────────

#[event]
pub struct JobSettled {
    pub job_id: [u8; 32],
    pub sla_met: bool,
    pub penalty_amount: u64,
}

#[event]
pub struct InsuranceTopUp {
    pub job_id: [u8; 32],
    pub amount: u64,
}

#[event]
pub struct JobAbandoned {
    pub job_id: [u8; 32],
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn penalty_for_tier(tier: &str, fee: u64) -> Result<u64> {
    let mult_bps: u64 = match tier {
        "realtime" => 200,
        "standard" => 100,
        "batch" => 25,
        "confidential" => 100,
        _ => return err!(ErrorCode::UnknownSlaTier),
    };
    Ok(fee * mult_bps / 100)
}

fn load_receipt(receipt: &AccountInfo) -> Result<ReceiptAccount> {
    let data = receipt.try_borrow_data()?;
    let body = &data[8..];
    let mut modern: &[u8] = body;
    if let Ok(rec) = ReceiptAccount::try_deserialize(&mut modern) {
        return Ok(rec);
    }
    Ok(legacy_to_receipt(parse_legacy_receipt(body)?))
}

fn store_receipt(receipt: &AccountInfo, state: &ReceiptAccount) -> Result<()> {
    let mut data = receipt.try_borrow_mut_data()?;
    let body = &mut data[8..];
    let legacy = parse_legacy_receipt(body)?;
    let mut offset = 32 + 4 + legacy.sla_tier.len() + 4 + 4 + 64 + 1;
    body[offset] = u8::from(state.sla_met);
    body[offset + 11] = u8::from(state.finalized);
    Ok(())
}

fn load_job(job: &AccountInfo) -> Result<ServingJob> {
    let data = job.try_borrow_data()?;
    let body = &data[8..];
    let mut modern: &[u8] = body;
    if let Ok(job) = ServingJob::try_deserialize(&mut modern) {
        return Ok(job);
    }
    parse_job_account(body)
}

fn parse_job_account(data: &[u8]) -> Result<ServingJob> {
    if data.len() < 32 + 32 + 32 + 4 + 8 + 1 + 1 + 8 + 8 + 8 + 1 {
        return err!(ErrorCode::InvalidJob);
    }
    let mut offset = 0usize;
    let mut job_id = [0u8; 32];
    job_id.copy_from_slice(&data[offset..offset + 32]);
    offset += 32;
    let customer = Pubkey::try_from(&data[offset..offset + 32])
        .map_err(|_| error!(ErrorCode::InvalidJob))?;
    offset += 32;
    let worker = Pubkey::try_from(&data[offset..offset + 32])
        .map_err(|_| error!(ErrorCode::InvalidJob))?;
    offset += 32;
    let tier_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    if offset + tier_len + 8 + 1 + 1 + 8 + 8 + 8 + 1 > data.len() {
        return err!(ErrorCode::InvalidJob);
    }
    let sla_tier = String::from_utf8(data[offset..offset + tier_len].to_vec())
        .map_err(|_| error!(ErrorCode::InvalidJob))?;
    offset += tier_len;
    let payment_amount = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let confidential = data[offset] != 0;
    offset += 1;
    let status = match data[offset] {
        0 => JobStatus::Pending,
        1 => JobStatus::Active,
        2 => JobStatus::Settled,
        3 => JobStatus::Expired,
        _ => return err!(ErrorCode::InvalidJob),
    };
    offset += 1;
    let opened_at = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let assigned_at = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let settled_at = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    offset += 8;
    let bump = data[offset];

    Ok(ServingJob {
        job_id,
        customer,
        worker,
        sla_tier,
        payment_amount,
        confidential,
        status,
        opened_at,
        assigned_at,
        settled_at,
        bump,
    })
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[error_code]
pub enum ErrorCode {
    #[msg("Receipt is not yet finalized")]
    ReceiptNotFinalized,
    #[msg("Job ID mismatch between receipt and job accounts")]
    JobIdMismatch,
    #[msg("Unknown SLA tier")]
    UnknownSlaTier,
    #[msg("Invalid receipt account owner")]
    InvalidReceipt,
    #[msg("Invalid job account owner")]
    InvalidJob,
    #[msg("Invalid job scheduler program")]
    InvalidJobScheduler,
}
