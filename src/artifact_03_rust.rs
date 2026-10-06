// programs/aegis-stream/src/lib.rs
use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

declare_id!("Aeg1sStream11111111111111111111111111111111");

#[program]
pub mod aegis_stream {
    use super::*;

    pub fn initialize_vault(
        ctx: Context<InitializeVault>,
        stream_rate_per_second: u64,
        cliff_timestamp: i64,
        end_timestamp: i64,
        deposit_amount: u64,
    ) -> Result<()> {
        let clock = Clock::get()?;
        require!(
            end_timestamp > cliff_timestamp,
            AegisError::InvalidTimestampBounds
        );
        require!(
            cliff_timestamp >= clock.unix_timestamp,
            AegisError::CliffInPast
        );
        require!(deposit_amount > 0, AegisError::ZeroDeposit);
        require!(stream_rate_per_second > 0, AegisError::ZeroStreamRate);

        let total_duration = (end_timestamp - cliff_timestamp) as u64;
        let minimum_required = stream_rate_per_second
            .checked_mul(total_duration)
            .ok_or(AegisError::NumericalOverflow)?;
        
        require!(
            deposit_amount >= minimum_required,
            AegisError::InsufficientDepositForRate
        );

        let vault = &mut ctx.accounts.vault_state;
        vault.depositor = ctx.accounts.depositor.key();
        vault.beneficiary = ctx.accounts.beneficiary.key();
        vault.arbiter = ctx.accounts.arbiter.key();
        vault.mint = ctx.accounts.mint.key();
        vault.vault_token_account = ctx.accounts.vault_token_account.key();
        vault.stream_rate_per_second = stream_rate_per_second;
        vault.cliff_timestamp = cliff_timestamp;
        vault.start_timestamp = clock.unix_timestamp;
        vault.end_timestamp = end_timestamp;
        vault.last_withdrawn_timestamp = cliff_timestamp;
        vault.total_deposited = deposit_amount;
        vault.total_withdrawn = 0;
        vault.status = VaultStatus::Active;
        vault.bump = ctx.bumps.vault_state;
        vault.authority_bump = ctx.bumps.vault_authority;

        // Transfer funds from depositor to vault token account
        let cpi_accounts = Transfer {
            from: ctx.accounts.depositor_token_account.to_account_info(),
            to: ctx.accounts.vault_token_account.to_account_info(),
            authority: ctx.accounts.depositor.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        token::transfer(CpiContext::new(cpi_program, cpi_accounts), deposit_amount)?;

        emit!(VaultInitializedEvent {
            vault: vault.key(),
            depositor: vault.depositor,
            beneficiary: vault.beneficiary,
            total_deposited: deposit_amount,
            start_time: vault.start_timestamp,
            end_time: vault.end_timestamp,
        });

        Ok(())
    }

    pub fn execute_release(ctx: Context<ExecuteRelease>) -> Result<()> {
        let clock = Clock::get()?;
        let vault = &mut ctx.accounts.vault_state;

        require!(vault.status == VaultStatus::Active, AegisError::VaultNotActive);
        require!(
            clock.unix_timestamp > vault.cliff_timestamp,
            AegisError::CliffNotReached
        );

        let calculation_limit_timestamp = std::cmp::min(clock.unix_timestamp, vault.end_timestamp);
        let eligible_seconds = (calculation_limit_timestamp - vault.last_withdrawn_timestamp) as u64;

        require!(eligible_seconds > 0, AegisError::NoTokensClaimable);

        let releasable_amount = eligible_seconds
            .checked_mul(vault.stream_rate_per_second)
            .ok_or(AegisError::NumericalOverflow)?;

        let remaining_in_vault = vault
            .total_deposited
            .checked_sub(vault.total_withdrawn)
            .ok_or(AegisError::NumericalOverflow)?;

        let payout_amount = std::cmp::min(releasable_amount, remaining_in_vault);
        require!(payout_amount > 0, AegisError::NoTokensClaimable);

        vault.total_withdrawn = vault
            .total_withdrawn
            .checked_add(payout_amount)
            .ok_or(AegisError::NumericalOverflow)?;
        vault.last_withdrawn_timestamp = calculation_limit_timestamp;

        if vault.total_withdrawn >= vault.total_deposited || calculation_limit_timestamp >= vault.end_timestamp {
            vault.status = VaultStatus::Completed;
        }

        // PDA Seeds transfer execution
        let depositor_key = vault.depositor;
        let beneficiary_key = vault.beneficiary;
        let authority_bump = vault.authority_bump;

        let authority_seeds: &[&[&[u8]]] = &[&[
            b"vault_authority",
            depositor_key.as_ref(),
            beneficiary_key.as_ref(),
            &[authority_bump],
        ]];

        let cpi_accounts = Transfer {
            from: ctx.accounts.vault_token_account.to_account_info(),
            to: ctx.accounts.beneficiary_token_account.to_account_info(),
            authority: ctx.accounts.vault_authority.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        token::transfer(
            CpiContext::new_with_signer(cpi_program, cpi_accounts, authority_seeds),
            payout_amount,
        )?;

        emit!(MilestoneReleasedEvent {
            vault: vault.key(),
            beneficiary: vault.beneficiary,
            amount: payout_amount,
            timestamp: clock.unix_timestamp,
            remaining_balance: vault.total_deposited - vault.total_withdrawn,
            is_completed: vault.status == VaultStatus::Completed,
        });

        Ok(())
    }

    pub fn trigger_dispute(ctx: Context<ManageDispute>) -> Result<()> {
        let vault = &mut ctx.accounts.vault_state;
        let signer = ctx.accounts.signer.key();

        require!(
            signer == vault.depositor || signer == vault.arbiter,
            AegisError::Unauthorized
        );
        require!(vault.status == VaultStatus::Active, AegisError::VaultNotActive);

        vault.status = VaultStatus::Disputed;

        emit!(DisputeOpenedEvent {
            vault: vault.key(),
            initiated_by: signer,
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }

    pub fn resolve_dispute(
        ctx: Context<ResolveDispute>,
        recipient_split_basis_points: u16,
    ) -> Result<()> {
        require!(
            recipient_split_basis_points <= 10_000,
            AegisError::InvalidBasisPoints
        );

        let vault = &mut ctx.accounts.vault_state;
        require!(
            vault.status == VaultStatus::Disputed,
            AegisError::VaultNotInDispute
        );

        let remaining_balance = ctx.accounts.vault_token_account.amount;
        let beneficiary_portion = (remaining_balance as u128)
            .checked_mul(recipient_split_basis_points as u128)
            .ok_or(AegisError::NumericalOverflow)?
            .checked_div(10_000)
            .ok_or(AegisError::NumericalOverflow)? as u64;

        let depositor_portion = remaining_balance
            .checked_sub(beneficiary_portion)
            .ok_or(AegisError::NumericalOverflow)?;

        let depositor_key = vault.depositor;
        let beneficiary_key = vault.beneficiary;
        let authority_bump = vault.authority_bump;

        let authority_seeds: &[&[&[u8]]] = &[&[
            b"vault_authority",
            depositor_key.as_ref(),
            beneficiary_key.as_ref(),
            &[authority_bump],
        ]];

        let cpi_program = ctx.accounts.token_program.to_account_info();

        if beneficiary_portion > 0 {
            let to_beneficiary = Transfer {
                from: ctx.accounts.vault_token_account.to_account_info(),
                to: ctx.accounts.beneficiary_token_account.to_account_info(),
                authority: ctx.accounts.vault_authority.to_account_info(),
            };
            token::transfer(
                CpiContext::new_with_signer(cpi_program.clone(), to_beneficiary, authority_seeds),
                beneficiary_portion,
            )?;
        }

        if depositor_portion > 0 {
            let to_depositor = Transfer {
                from: ctx.accounts.vault_token_account.to_account_info(),
                to: ctx.accounts.depositor_token_account.to_account_info(),
                authority: ctx.accounts.vault_authority.to_account_info(),
            };
            token::transfer(
                CpiContext::new_with_signer(cpi_program, to_depositor, authority_seeds),
                depositor_portion,
            )?;
        }

        vault.status = VaultStatus::Cancelled;
        vault.total_withdrawn = vault.total_deposited;

        emit!(DisputeResolvedEvent {
            vault: vault.key(),
            beneficiary_payout: beneficiary_portion,
            depositor_payout: depositor_portion,
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }
}

// -----------------------------------------------------------------------------
// ACCOUNT VALIDATION STRUCTS
// -----------------------------------------------------------------------------

#[derive(Accounts)]
pub struct InitializeVault<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    /// CHECK: Safe verified beneficiary public key
    pub beneficiary: UncheckedAccount<'info>,

    /// CHECK: Independent arbiter
    pub arbiter: UncheckedAccount<'info>,

    pub mint: Account<'info, Mint>,

    #[account(
        init,
        payer = depositor,
        space = 8 + VaultState::INIT_SPACE,
        seeds = [b"aegis_vault", depositor.key().as_ref(), beneficiary.key().as_ref()],
        bump
    )]
    pub vault_state: Account<'info, VaultState>,

    /// CHECK: PDA signer authority for token vault
    #[account(
        seeds = [b"vault_authority", depositor.key().as_ref(), beneficiary.key().as_ref()],
        bump
    )]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(
        init,
        payer = depositor,
        seeds = [b"token_escrow", vault_state.key().as_ref()],
        bump,
        token::mint = mint,
        token::authority = vault_authority
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = depositor_token_account.mint == mint.key() @ AegisError::InvalidMint,
        constraint = depositor_token_account.owner == depositor.key() @ AegisError::InvalidOwner
    )]
    pub depositor_token_account: Account<'info, TokenAccount>,

    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct ExecuteRelease<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,

    #[account(
        mut,
        seeds = [b"aegis_vault", vault_state.depositor.as_ref(), vault_state.beneficiary.as_ref()],
        bump = vault_state.bump
    )]
    pub vault_state: Account<'info, VaultState>,

    /// CHECK: Verified against seeds
    #[account(
        seeds = [b"vault_authority", vault_state.depositor.as_ref(), vault_state.beneficiary.as_ref()],
        bump = vault_state.authority_bump
    )]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(
        mut,
        address = vault_state.vault_token_account @ AegisError::InvalidVaultTokenAccount
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = beneficiary_token_account.owner == vault_state.beneficiary @ AegisError::InvalidBeneficiaryTokenAccount,
        constraint = beneficiary_token_account.mint == vault_state.mint @ AegisError::InvalidMint
    )]
    pub beneficiary_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ManageDispute<'info> {
    pub signer: Signer<'info>,

    #[account(
        mut,
        seeds = [b"aegis_vault", vault_state.depositor.as_ref(), vault_state.beneficiary.as_ref()],
        bump = vault_state.bump
    )]
    pub vault_state: Account<'info, VaultState>,
}

#[derive(Accounts)]
pub struct ResolveDispute<'info> {
    #[account(
        constraint = arbiter.key() == vault_state.arbiter @ AegisError::OnlyArbiterPermitted
    )]
    pub arbiter: Signer<'info>,

    #[account(
        mut,
        seeds = [b"aegis_vault", vault_state.depositor.as_ref(), vault_state.beneficiary.as_ref()],
        bump = vault_state.bump
    )]
    pub vault_state: Account<'info, VaultState>,

    /// CHECK: PDA authority
    #[account(
        seeds = [b"vault_authority", vault_state.depositor.as_ref(), vault_state.beneficiary.as_ref()],
        bump = vault_state.authority_bump
    )]
    pub vault_authority: UncheckedAccount<'info>,

    #[account(
        mut,
        address = vault_state.vault_token_account @ AegisError::InvalidVaultTokenAccount
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = beneficiary_token_account.owner == vault_state.beneficiary @ AegisError::InvalidBeneficiaryTokenAccount,
        constraint = beneficiary_token_account.mint == vault_state.mint @ AegisError::InvalidMint
    )]
    pub beneficiary_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = depositor_token_account.owner == vault_state.depositor @ AegisError::InvalidDepositorTokenAccount,
        constraint = depositor_token_account.mint == vault_state.mint @ AegisError::InvalidMint
    )]
    pub depositor_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

// -----------------------------------------------------------------------------
// STATE STRUCTURES & ERROR CODES
// -----------------------------------------------------------------------------

#[account]
#[derive(InitSpace)]
pub struct VaultState {
    pub depositor: Pubkey,
    pub beneficiary: Pubkey,
    pub arbiter: Pubkey,
    pub mint: Pubkey,
    pub vault_token_account: Pubkey,
    pub stream_rate_per_second: u64,
    pub cliff_timestamp: i64,
    pub start_timestamp: i64,
    pub end_timestamp: i64,
    pub last_withdrawn_timestamp: i64,
    pub total_deposited: u64,
    pub total_withdrawn: u64,
    pub status: VaultStatus,
    pub bump: u8,
    pub authority_bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum VaultStatus {
    Active,
    Disputed,
    Completed,
    Cancelled,
}

#[event]
pub struct VaultInitializedEvent {
    pub vault: Pubkey,
    pub depositor: Pubkey,
    pub beneficiary: Pubkey,
    pub total_deposited: u64,
    pub start_time: i64,
    pub end_time: i64,
}

#[event]
pub struct MilestoneReleasedEvent {
    pub vault: Pubkey,
    pub beneficiary: Pubkey,
    pub amount: u64,
    pub timestamp: i64,
    pub remaining_balance: u64,
    pub is_completed: bool,
}

#[event]
pub struct DisputeOpenedEvent {
    pub vault: Pubkey,
    pub initiated_by: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct DisputeResolvedEvent {
    pub vault: Pubkey,
    pub beneficiary_payout: u64,
    pub depositor_payout: u64,
    pub timestamp: i64,
}

#[error_code]
pub enum AegisError {
    #[msg("Cliff timestamp cannot be in the past")]
    CliffInPast,
    #[msg("End timestamp must be strictly greater than cliff timestamp")]
    InvalidTimestampBounds,
    #[msg("Deposit amount must be greater than zero")]
    ZeroDeposit,
    #[msg("Stream rate must be greater than zero")]
    ZeroStreamRate,
    #[msg("Deposit amount is less than total required by stream rate over duration")]
    InsufficientDepositForRate,
    #[msg("Mathematical calculation overflowed")]
    NumericalOverflow,
    #[msg("Vault is not in active status")]
    VaultNotActive,
    #[msg("Stream cliff timestamp has not been reached")]
    CliffNotReached,
    #[msg("Zero tokens are currently claimable based on stream progress")]
    NoTokensClaimable,
    #[msg("Caller lacks authorization for this operation")]
    Unauthorized,
    #[msg("Vault is not in disputed status")]
    VaultNotInDispute,
    #[msg("Split basis points cannot exceed 10000 (100%)")]
    InvalidBasisPoints,
    #[msg("Only the designated arbiter can execute dispute resolution")]
    OnlyArbiterPermitted,
    #[msg("Invalid token mint supplied")]
    InvalidMint,
    #[msg("Token account does not belong to expected owner")]
    InvalidOwner,
    #[msg("Invalid vault token account supplied")]
    InvalidVaultTokenAccount,
    #[msg("Invalid beneficiary token account supplied")]
    InvalidBeneficiaryTokenAccount,
    #[msg("Invalid depositor token account supplied")]
    InvalidDepositorTokenAccount,
}