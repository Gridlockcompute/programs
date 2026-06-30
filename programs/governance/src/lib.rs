use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

declare_id!("8d4UT2HPvAkmX4JR367nyNVWNqawiqGjN1bBbwtvvFUB");

// ── Constants ─────────────────────────────────────────────────────────────────

pub const QUORUM_LOCK: u64    = 10_000_000 * 1_000_000_000; // 10M LOCK (9 decimals)
pub const PASS_BPS: u64       = 6000;  // 60% threshold
pub const VOTING_PERIOD: i64  = 3 * 86400; // 3 days
pub const TIME_LOCK_SECS: i64 = 2 * 86400; // 48 hours before execution
pub const MAX_TITLE_LEN: usize = 128;
pub const MAX_DESC_LEN: usize  = 1024;
pub const MAX_CALLS: usize     = 8; // max CPI calls in a proposal

// ── Program ──────────────────────────────────────────────────────────────────

#[program]
pub mod governance {
    use super::*;

    /// Initialize the governance state account (called once).
    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        let state = &mut ctx.accounts.governance_state;
        state.authority = ctx.accounts.authority.key();
        state.proposal_count = 0;
        state.bump = ctx.bumps.governance_state;
        Ok(())
    }

    /// Create a new governance proposal.
    ///
    /// The proposer must have at least 10,000 LOCK staked to avoid spam.
    /// Voting opens immediately and closes after VOTING_PERIOD (3 days).
    pub fn create_proposal(
        ctx: Context<CreateProposal>,
        args: CreateProposalArgs,
    ) -> Result<()> {
        require!(args.title.len() <= MAX_TITLE_LEN, ErrorCode::TitleTooLong);
        require!(args.description.len() <= MAX_DESC_LEN, ErrorCode::DescriptionTooLong);
        require!(args.calls.len() <= MAX_CALLS, ErrorCode::TooManyCalls);

        // Minimum staked LOCK to propose
        require!(
            ctx.accounts.proposer_stake.amount >= 10_000 * 1_000_000_000,
            ErrorCode::InsufficientStakeToPropose,
        );

        let state = &mut ctx.accounts.governance_state;
        let proposal = &mut ctx.accounts.proposal;
        let now = Clock::get()?.unix_timestamp;

        proposal.id = state.proposal_count;
        proposal.proposer = ctx.accounts.proposer.key();
        proposal.title = args.title;
        proposal.description = args.description;
        proposal.category = args.category;
        proposal.calls = args.calls;
        proposal.votes_for = 0;
        proposal.votes_against = 0;
        proposal.votes_abstain = 0;
        proposal.status = ProposalStatus::Active;
        proposal.created_at = now;
        proposal.voting_ends_at = now + VOTING_PERIOD;
        proposal.executed_at = 0;
        proposal.bump = ctx.bumps.proposal;

        state.proposal_count += 1;

        emit!(ProposalCreated {
            id: proposal.id,
            proposer: proposal.proposer,
            title: proposal.title.clone(),
            voting_ends_at: proposal.voting_ends_at,
        });

        Ok(())
    }

    /// Cast a vote on an active proposal.
    ///
    /// Voting power = staked LOCK balance in the voter's stake account.
    /// Each wallet can vote once per proposal — the VoteRecord PDA prevents double-voting.
    pub fn vote(
        ctx: Context<Vote>,
        proposal_id: u64,
        choice: VoteChoice,
    ) -> Result<()> {
        let proposal = &mut ctx.accounts.proposal;
        require!(proposal.id == proposal_id, ErrorCode::ProposalIdMismatch);
        require!(proposal.status == ProposalStatus::Active, ErrorCode::ProposalNotActive);

        let now = Clock::get()?.unix_timestamp;
        require!(now < proposal.voting_ends_at, ErrorCode::VotingEnded);

        let voting_power = ctx.accounts.voter_stake.amount;
        require!(voting_power > 0, ErrorCode::NoVotingPower);

        match choice {
            VoteChoice::For     => proposal.votes_for     += voting_power,
            VoteChoice::Against => proposal.votes_against += voting_power,
            VoteChoice::Abstain => proposal.votes_abstain += voting_power,
        }

        let record = &mut ctx.accounts.vote_record;
        record.voter = ctx.accounts.voter.key();
        record.proposal_id = proposal_id;
        record.choice = choice.clone();
        record.voting_power = voting_power;
        record.voted_at = now;
        record.bump = ctx.bumps.vote_record;

        emit!(VoteCast {
            proposal_id,
            voter: ctx.accounts.voter.key(),
            choice,
            voting_power,
        });

        Ok(())
    }

    /// Finalize voting after the voting period ends.
    ///
    /// Checks quorum (10M LOCK) and pass threshold (60% for).
    /// Permissionless — any account can call this to finalize.
    pub fn finalize_voting(ctx: Context<FinalizeVoting>, proposal_id: u64) -> Result<()> {
        let proposal = &mut ctx.accounts.proposal;
        require!(proposal.id == proposal_id, ErrorCode::ProposalIdMismatch);
        require!(proposal.status == ProposalStatus::Active, ErrorCode::ProposalNotActive);

        let now = Clock::get()?.unix_timestamp;
        require!(now >= proposal.voting_ends_at, ErrorCode::VotingStillOpen);

        let total_votes = proposal.votes_for + proposal.votes_against + proposal.votes_abstain;
        let quorum_met = total_votes >= QUORUM_LOCK;
        let pass_pct_bps = if total_votes > 0 {
            proposal.votes_for * 10_000 / total_votes
        } else {
            0
        };

        if quorum_met && pass_pct_bps >= PASS_BPS {
            proposal.status = ProposalStatus::Queued;
            emit!(ProposalQueued {
                id: proposal_id,
                executes_after: now + TIME_LOCK_SECS,
            });
        } else {
            proposal.status = ProposalStatus::Failed;
            emit!(ProposalFailed {
                id: proposal_id,
                quorum_met,
                pass_pct_bps,
            });
        }

        Ok(())
    }

    /// Execute a queued proposal after the time-lock expires.
    ///
    /// In practice, the CPI calls listed in proposal.calls are dispatched here.
    /// This stub logs the calls — actual CPI wiring depends on each target program's interface.
    pub fn execute_proposal(ctx: Context<ExecuteProposal>, proposal_id: u64) -> Result<()> {
        let proposal = &mut ctx.accounts.proposal;
        require!(proposal.id == proposal_id, ErrorCode::ProposalIdMismatch);
        require!(proposal.status == ProposalStatus::Queued, ErrorCode::ProposalNotQueued);

        let now = Clock::get()?.unix_timestamp;
        // Ensure 48h time-lock has passed since voting ended
        require!(
            now >= proposal.voting_ends_at + TIME_LOCK_SECS,
            ErrorCode::TimeLockNotExpired,
        );

        for call in &proposal.calls {
            msg!(
                "GovernanceProgram: executing CPI to {} instruction {}",
                call.program_id,
                call.instruction_tag,
            );
        }

        proposal.status = ProposalStatus::Executed;
        proposal.executed_at = now;

        emit!(ProposalExecuted { id: proposal_id, executed_at: now });

        Ok(())
    }

    /// Cancel a proposal before voting ends.
    ///
    /// Only the original proposer (or governance authority) can cancel.
    pub fn cancel_proposal(ctx: Context<CancelProposal>, proposal_id: u64) -> Result<()> {
        let proposal = &mut ctx.accounts.proposal;
        require!(proposal.id == proposal_id, ErrorCode::ProposalIdMismatch);
        require!(
            matches!(proposal.status, ProposalStatus::Active | ProposalStatus::Queued),
            ErrorCode::CannotCancel,
        );
        require!(
            proposal.proposer == ctx.accounts.authority.key()
                || ctx.accounts.authority.key() == ctx.accounts.governance_state.authority,
            ErrorCode::Unauthorized,
        );
        proposal.status = ProposalStatus::Cancelled;
        emit!(ProposalCancelled { id: proposal_id });
        Ok(())
    }
}

// ── Accounts ─────────────────────────────────────────────────────────────────

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        init,
        payer = authority,
        space = GovernanceState::LEN,
        seeds = [b"governance_state"],
        bump,
    )]
    pub governance_state: Account<'info, GovernanceState>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CreateProposal<'info> {
    #[account(mut)]
    pub proposer: Signer<'info>,

    #[account(mut, seeds = [b"governance_state"], bump = governance_state.bump)]
    pub governance_state: Account<'info, GovernanceState>,

    #[account(
        init,
        payer = proposer,
        space = ProposalAccount::LEN,
        seeds = [b"proposal", governance_state.proposal_count.to_le_bytes().as_ref()],
        bump,
    )]
    pub proposal: Account<'info, ProposalAccount>,

    /// Proposer's staked LOCK account — must have ≥ 10K LOCK
    pub proposer_stake: InterfaceAccount<'info, TokenAccount>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(proposal_id: u64)]
pub struct Vote<'info> {
    #[account(mut)]
    pub voter: Signer<'info>,

    #[account(mut, seeds = [b"proposal", proposal_id.to_le_bytes().as_ref()], bump = proposal.bump)]
    pub proposal: Account<'info, ProposalAccount>,

    /// PDA prevents double-voting: one VoteRecord per (voter, proposal)
    #[account(
        init,
        payer = voter,
        space = VoteRecord::LEN,
        seeds = [b"vote", proposal_id.to_le_bytes().as_ref(), voter.key().as_ref()],
        bump,
    )]
    pub vote_record: Account<'info, VoteRecord>,

    /// Voter's staked LOCK account — determines voting power
    pub voter_stake: InterfaceAccount<'info, TokenAccount>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(proposal_id: u64)]
pub struct FinalizeVoting<'info> {
    pub caller: Signer<'info>,
    #[account(mut, seeds = [b"proposal", proposal_id.to_le_bytes().as_ref()], bump = proposal.bump)]
    pub proposal: Account<'info, ProposalAccount>,
}

#[derive(Accounts)]
#[instruction(proposal_id: u64)]
pub struct ExecuteProposal<'info> {
    pub caller: Signer<'info>,
    #[account(seeds = [b"governance_state"], bump = governance_state.bump)]
    pub governance_state: Account<'info, GovernanceState>,
    #[account(mut, seeds = [b"proposal", proposal_id.to_le_bytes().as_ref()], bump = proposal.bump)]
    pub proposal: Account<'info, ProposalAccount>,
}

#[derive(Accounts)]
#[instruction(proposal_id: u64)]
pub struct CancelProposal<'info> {
    pub authority: Signer<'info>,
    #[account(seeds = [b"governance_state"], bump = governance_state.bump)]
    pub governance_state: Account<'info, GovernanceState>,
    #[account(mut, seeds = [b"proposal", proposal_id.to_le_bytes().as_ref()], bump = proposal.bump)]
    pub proposal: Account<'info, ProposalAccount>,
}

// ── State ─────────────────────────────────────────────────────────────────────

#[account]
pub struct GovernanceState {
    pub authority: Pubkey,
    pub proposal_count: u64,
    pub bump: u8,
}

impl GovernanceState {
    pub const LEN: usize = 8 + 32 + 8 + 1;
}

#[account]
pub struct ProposalAccount {
    pub id: u64,
    pub proposer: Pubkey,
    pub title: String,       // max 128
    pub description: String, // max 1024
    pub category: ProposalCategory,
    pub calls: Vec<GovernanceCall>,
    pub votes_for: u64,
    pub votes_against: u64,
    pub votes_abstain: u64,
    pub status: ProposalStatus,
    pub created_at: i64,
    pub voting_ends_at: i64,
    pub executed_at: i64,
    pub bump: u8,
}

impl ProposalAccount {
    pub const LEN: usize = 8      // discriminator
        + 8                        // id
        + 32                       // proposer
        + 4 + 128                  // title
        + 4 + 1024                 // description
        + 1                        // category
        + 4 + MAX_CALLS * (32 + 4 + 128)  // calls vec
        + 8 + 8 + 8                // votes
        + 1                        // status
        + 8 + 8 + 8                // timestamps
        + 1;                       // bump
}

#[account]
pub struct VoteRecord {
    pub voter: Pubkey,
    pub proposal_id: u64,
    pub choice: VoteChoice,
    pub voting_power: u64,
    pub voted_at: i64,
    pub bump: u8,
}

impl VoteRecord {
    pub const LEN: usize = 8 + 32 + 8 + 1 + 8 + 8 + 1;
}

// ── Enums ─────────────────────────────────────────────────────────────────────

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum ProposalStatus {
    Active,
    Queued,
    Executed,
    Failed,
    Cancelled,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum VoteChoice {
    For,
    Against,
    Abstain,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq)]
pub enum ProposalCategory {
    Fee,
    Sla,
    Emission,
    Upgrade,
    Treasury,
    Emergency,
}

// ── Args ──────────────────────────────────────────────────────────────────────

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct CreateProposalArgs {
    pub title: String,
    pub description: String,
    pub category: ProposalCategory,
    pub calls: Vec<GovernanceCall>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct GovernanceCall {
    pub program_id: Pubkey,
    pub instruction_tag: u32,
    pub data: Vec<u8>, // up to 128 bytes of instruction data
}

// ── Events ────────────────────────────────────────────────────────────────────

#[event]
pub struct ProposalCreated {
    pub id: u64,
    pub proposer: Pubkey,
    pub title: String,
    pub voting_ends_at: i64,
}

#[event]
pub struct VoteCast {
    pub proposal_id: u64,
    pub voter: Pubkey,
    pub choice: VoteChoice,
    pub voting_power: u64,
}

#[event]
pub struct ProposalQueued {
    pub id: u64,
    pub executes_after: i64,
}

#[event]
pub struct ProposalFailed {
    pub id: u64,
    pub quorum_met: bool,
    pub pass_pct_bps: u64,
}

#[event]
pub struct ProposalExecuted {
    pub id: u64,
    pub executed_at: i64,
}

#[event]
pub struct ProposalCancelled {
    pub id: u64,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[error_code]
pub enum ErrorCode {
    #[msg("Proposal title exceeds 128 characters")]
    TitleTooLong,
    #[msg("Proposal description exceeds 1024 characters")]
    DescriptionTooLong,
    #[msg("Too many CPI calls in proposal (max 8)")]
    TooManyCalls,
    #[msg("Insufficient staked LOCK to create proposal (need 10,000 LOCK)")]
    InsufficientStakeToPropose,
    #[msg("Proposal ID mismatch")]
    ProposalIdMismatch,
    #[msg("Proposal is not active")]
    ProposalNotActive,
    #[msg("Proposal is not queued for execution")]
    ProposalNotQueued,
    #[msg("Voting period has ended")]
    VotingEnded,
    #[msg("Voting period is still open")]
    VotingStillOpen,
    #[msg("No staked LOCK — no voting power")]
    NoVotingPower,
    #[msg("Time-lock has not expired yet (48 hours required)")]
    TimeLockNotExpired,
    #[msg("Proposal cannot be cancelled in its current status")]
    CannotCancel,
    #[msg("Unauthorized")]
    Unauthorized,
}
