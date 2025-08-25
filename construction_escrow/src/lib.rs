use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{self, Burn, FreezeAccount, Mint, MintTo, ThawAccount, Token, TokenAccount, Transfer},
};
// Optional Token-2022 support by feature flag. For PoC we stick to token2022 aliasing.
#[cfg(feature = "token2022")]
use anchor_spl::token_2022 as token;

declare_id!("programid");

/* =============================== Constants ================================ */

const MAX_ORACLES: usize = 8;
const MAX_MILESTONES: usize = 10;
const QUORUM_MIN: u8 = 1;

/* ================================ Program ================================= */

#[program]
pub mod construction_escrow {
    use super::*;

    /* ------------------------------ Config Ixs ------------------------------ */

    /// Initialize global market/config defaults.
    pub fn init_config(
        ctx: Context<InitConfig>,
        fee_bps: u16,
        insurance_bps: u16,
        retention_bps: u16,
        warranty_days: i64,
        quorum_m: u8,
    ) -> Result<()> {
        require!(quorum_m >= QUORUM_MIN, EscrowError::BadQuorum);
        let cfg = &mut ctx.accounts.config;
        cfg.authority = ctx.accounts.authority.key();
        cfg.treasury = ctx.accounts.treasury.key();
        cfg.insurance_treasury = ctx.accounts.insurance_treasury.key();
        cfg.fee_bps = fee_bps;
        cfg.insurance_bps = insurance_bps;
        cfg.retention_bps = retention_bps;
        cfg.warranty_days = warranty_days;
        cfg.quorum_m = quorum_m;
        cfg.arbiter = ctx.accounts.arbiter.key();
        cfg.pending_authority = Pubkey::default();
        cfg.bump = ctx.bumps.config;
        emit!(ConfigUpdated {
            fee_bps,
            insurance_bps,
            retention_bps,
            warranty_days,
            quorum_m
        });
        Ok(())
    }

    pub fn update_fee_splits(ctx: Context<ConfigAuthority>, fee_bps: u16, insurance_bps: u16) -> Result<()> {
        let cfg = &mut ctx.accounts.config;
        cfg.fee_bps = fee_bps;
        cfg.insurance_bps = insurance_bps;
        emit!(ConfigUpdated {
            fee_bps,
            insurance_bps,
            retention_bps: cfg.retention_bps,
            warranty_days: cfg.warranty_days,
            quorum_m: cfg.quorum_m
        });
        Ok(())
    }

    pub fn transfer_market_authority_propose(ctx: Context<ConfigAuthority>, new_auth: Pubkey) -> Result<()> {
        let cfg = &mut ctx.accounts.config;
        cfg.pending_authority = new_auth;
        emit!(ConfigAuthorityProposed { proposed: new_auth });
        Ok(())
    }

    pub fn transfer_market_authority_accept(ctx: Context<AcceptAuthority>) -> Result<()> {
        let cfg = &mut ctx.accounts.config;
        require!(cfg.pending_authority == ctx.accounts.new_authority.key(), EscrowError::BadAuthorityAccept);
        cfg.authority = cfg.pending_authority;
        cfg.pending_authority = Pubkey::default();
        emit!(ConfigAuthorityTransferred { new_authority: cfg.authority });
        Ok(())
    }

    /* ------------------------------ Create Escrow -------------------------- */

    /// Create escrow and move buyer funds (quote tokens) into PDA vault.
    /// `oracles` length <= MAX_ORACLES; quorum_m >= 1.
    /// `price_snapshot_1e6` lets you store optional USD notional (6dp). Set to 0 if unused.
    pub fn create_escrow(
        ctx: Context<CreateEscrow>,
        project_id: u64,
        amount: u64,
        ix_nonce: u64,
        oracles: Vec<Pubkey>,
        quorum_m: u8,
        price_snapshot_1e6: u64,
        nft_enabled: bool,
    ) -> Result<()> {
        require!(amount > 0, EscrowError::ZeroAmount);
        require!(quorum_m >= QUORUM_MIN, EscrowError::BadQuorum);
        require!(oracles.len() <= MAX_ORACLES, EscrowError::TooManyOracles);

        let cfg = &ctx.accounts.config;

        // Record state
        let escrow = &mut ctx.accounts.escrow;
        require!(ix_nonce > escrow.last_ix_nonce, EscrowError::BadNonce);
        escrow.last_ix_nonce = ix_nonce;

        escrow.project_id = project_id;
        escrow.buyer = ctx.accounts.buyer.key();
        escrow.seller = ctx.accounts.seller.key();
        escrow.mint = ctx.accounts.mint.key();

        escrow.config = ctx.accounts.config.key();
        escrow.fee_bps = cfg.fee_bps;
        escrow.insurance_bps = cfg.insurance_bps;
        escrow.retention_bps = cfg.retention_bps;

        escrow.amount = amount;
        escrow.vault_bump = ctx.bumps.vault_authority;
        escrow.bump = ctx.bumps.escrow;

        // Oracles / quorum
        escrow.quorum_m = quorum_m;
        escrow.oracles_len = oracles.len() as u8;
        escrow.oracles = [Pubkey::default(); MAX_ORACLES];
        for (i, pk) in oracles.iter().enumerate() {
            escrow.oracles[i] = *pk;
        }

        // Price snapshot
        escrow.price_snapshot_1e6 = price_snapshot_1e6;

        // State flags & timestamps
        escrow.state = EscrowState::Open as u8;
        escrow.created_ts = Clock::get()?.unix_timestamp;
        escrow.verified_ts = 0;
        escrow.released_ts = 0;
        escrow.warranty_end_ts = escrow.created_ts + cfg.warranty_days * 24 * 60 * 60;
        escrow.verify_by_ts = 0;
        escrow.deliver_by_ts = 0;
        escrow.in_progress = false;
        escrow.in_transfer = false;
        escrow.retention_released = false;

        // Milestones init
        escrow.milestones_len = 0;
        escrow.milestones = [Milestone::EMPTY; MAX_MILESTONES];

        // Evidence counters
        escrow.attestations_count = 0;
        escrow.cancel_requested_by = Pubkey::default();
        escrow.dispute_open = false;

        // Optional receipt NFT toggle
        escrow.nft_enabled = nft_enabled;
        escrow.receipt_nft_mint = Pubkey::default();

        // Pull funds from buyer → vault
        let cpi_accounts = Transfer {
            from: ctx.accounts.buyer_ata.to_account_info(),
            to: ctx.accounts.vault_ata.to_account_info(),
            authority: ctx.accounts.buyer.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, amount)?;

        // Project index (1:1 convenience mapping for lookups)
        let index = &mut ctx.accounts.project_index;
        index.project_id = project_id;
        index.escrow = escrow.key();
        index.bump = ctx.bumps.project_index;

        emit!(EscrowCreated {
            project_id,
            buyer: escrow.buyer,
            seller: escrow.seller,
            mint: escrow.mint,
            amount,
            quorum_m,
            price_snapshot_1e6
        });

        Ok(())
    }

    /* -------------------------- Deadlines & Liveness ----------------------- */

    pub fn set_deadlines(ctx: Context<BuyerOrSeller>, verify_by_ts: i64, deliver_by_ts: i64) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!(e.state == EscrowState::Open as u8 || e.state == EscrowState::PartiallyReleased as u8, EscrowError::BadState);
        e.verify_by_ts = verify_by_ts;
        e.deliver_by_ts = deliver_by_ts;
        emit!(DeadlinesSet { project_id: e.project_id, verify_by_ts, deliver_by_ts });
        Ok(())
    }

    pub fn mark_in_progress(ctx: Context<SellerOnly>) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        e.in_progress = true;
        emit!(ProgressMarked { project_id: e.project_id, ts: Clock::get()?.unix_timestamp });
        Ok(())
    }

    /// If not verified by `verify_by_ts`, allow anyone to refund buyer.
    pub fn expire_and_refund(ctx: Context<RefundBuyer>) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        let now = Clock::get()?.unix_timestamp;
        require!(e.verify_by_ts > 0 && now > e.verify_by_ts, EscrowError::NotExpired);
        require!(e.state == EscrowState::Open as u8, EscrowError::BadState);

        // Transfer back to buyer
        let refund_amount = ctx.accounts.vault_ata.amount;
        require!(refund_amount >= e.amount, EscrowError::VaultBalanceLow);

        transfer_from_vault(
            e,
            &ctx.accounts.token_program,
            &ctx.accounts.vault_authority,
            &ctx.accounts.vault_ata,
            &ctx.accounts.buyer_ata,
            refund_amount,
        )?;

        e.state = EscrowState::Refunded as u8;
        e.released_ts = now;

        emit!(ExpiredAndRefunded { project_id: e.project_id, amount: refund_amount });
        Ok(())
    }

    /* ---------------------------- Verification ----------------------------- */

    /// M-of-N oracle quorum verification. Pass any number of signer accounts
    /// in remaining_accounts; we’ll count signers that are in `escrow.oracles`.
    pub fn verify_delivery(ctx: Context<VerifyWithQuorum>, project_id: u64) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!(e.project_id == project_id, EscrowError::ProjectMismatch);
        require!(e.state == EscrowState::Open as u8 || e.state == EscrowState::PartiallyReleased as u8, EscrowError::BadState);

        let votes = count_quorum_votes(e, &ctx.remaining_accounts)?;
        require!((votes as u8) >= e.quorum_m, EscrowError::QuorumNotMet);

        if e.state == EscrowState::Open as u8 {
            e.state = EscrowState::Verified as u8;
        }
        e.verified_ts = Clock::get()?.unix_timestamp;

        emit!(DeliveryVerified {
            project_id,
            quorum_votes: votes as u8,
            when: e.verified_ts
        });

        Ok(())
    }

    /* ----------------------------- Milestones ------------------------------ */

    pub fn add_milestone(ctx: Context<BuyerOrSeller>, amount: u64, evidence_hash: [u8; 32]) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!(e.state == EscrowState::Open as u8 || e.state == EscrowState::Verified as u8, EscrowError::BadState);
        require!((e.milestones_len as usize) < MAX_MILESTONES, EscrowError::TooManyMilestones);

        // Ensure milestone sum <= total amount (retain room for retention if desired)
        let current_sum: u64 = e.milestones().iter().map(|m| m.amount).sum();
        require!(current_sum.saturating_add(amount) <= e.amount, EscrowError::MilestoneOverTotal);

        let id = e.milestones_len;
        e.milestones[id as usize] = Milestone {
            id,
            amount,
            verified: false,
            released: false,
            verify_ts: 0,
            evidence_hash,
            reserved: [0u8; 7],
        };
        e.milestones_len += 1;

        emit!(MilestoneAdded { project_id: e.project_id, id, amount, evidence_hash });
        Ok(())
    }

    pub fn verify_milestone(ctx: Context<VerifyWithQuorum>, milestone_id: u8) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!((milestone_id as usize) < e.milestones_len as usize, EscrowError::BadMilestoneId);

        let votes = count_quorum_votes(e, &ctx.remaining_accounts)?;
        require!((votes as u8) >= e.quorum_m, EscrowError::QuorumNotMet);

        // Cache from `e` before mut borrow
        let project_id = e.project_id;
        let was_open = e.state == EscrowState::Open as u8;

        // Scope the mutable borrow for milestone
        let when: i64 = {
            let m = &mut e.milestones[milestone_id as usize];
            require!(!m.verified, EscrowError::AlreadyVerified);
            m.verified = true;
            m.verify_ts = Clock::get()?.unix_timestamp;
            m.verify_ts
        };

        if (was_open) {
            e.state = EscrowState::Verified as u8;
        }

        emit!(MilestoneVerified { project_id, id: milestone_id, when });
        Ok(())
    }

    /// Releases funds for a verified milestone. Applies fees, insurance, and late penalty if past deliver_by_ts.
    pub fn release_for_milestone(ctx: Context<ReleaseCommon>, milestone_id: u8) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!((milestone_id as usize) < e.milestones_len as usize, EscrowError::BadMilestoneId);

        // Pull milestone data in a separate scope to avoid borrow conflicts
        let payout: u64 = {
            let m = &e.milestones[milestone_id as usize];
            require!(m.verified && !m.released, EscrowError::MilestoneNotReleasable);
            m.amount
        };

        // Guard
        enter_transfer(e)?;

        // Check vault balance
        require!(ctx.accounts.vault_ata.amount >= payout, EscrowError::VaultBalanceLow);

        let now = Clock::get()?.unix_timestamp;

        // Fees
        let (fee_cut, insurance_cut) = calc_fee_splits(payout, e.fee_bps, e.insurance_bps);
        let mut seller_amount = payout.saturating_sub(fee_cut + insurance_cut);

        // Late penalty: reduce seller payout; send to buyer
        if e.deliver_by_ts > 0 && now > e.deliver_by_ts {
            let penalty = mul_bps(seller_amount, e.late_penalty_bps);
            seller_amount = seller_amount.saturating_sub(penalty);

            // penalty → buyer
            if penalty > 0 {
                transfer_from_vault(
                    e,
                    &ctx.accounts.token_program,
                    &ctx.accounts.vault_authority,
                    &ctx.accounts.vault_ata,
                    &ctx.accounts.buyer_ata,
                    penalty,
                )?;
            }
        }

        // Route fees
        if fee_cut > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.treasury_ata,
                fee_cut,
            )?;
        }
        if insurance_cut > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.insurance_ata,
                insurance_cut,
            )?;
        }

        // Pay seller
        if seller_amount > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.seller_ata,
                seller_amount,
            )?;
        }

        // Mark milestone as released
        {
            let m = &mut e.milestones[milestone_id as usize];
            m.released = true;
        }

        e.state = EscrowState::PartiallyReleased as u8;
        e.released_ts = now;

        exit_transfer(e);

        emit!(MilestoneReleased {
            project_id: e.project_id,
            id: milestone_id,
            gross: payout,
            fee_cut,
            insurance_cut,
            seller_received: seller_amount,
        });
        Ok(())
    }

    /* ----------------------------- Full Release ---------------------------- */

    /// Releases remaining balance to seller after overall verification (and optionally milestones).
    pub fn release_payment(ctx: Context<ReleaseCommon>) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!(e.state == EscrowState::Verified as u8 || e.state == EscrowState::PartiallyReleased as u8, EscrowError::BadState);

        // remaining = vault - retention (if retention not released yet)
        let mut remaining = ctx.accounts.vault_ata.amount;
        let retention_due = calc_retention(e.amount, e.retention_bps);
        if !e.retention_released {
            remaining = remaining.saturating_sub(retention_due.min(remaining));
        }

        require!(remaining > 0, EscrowError::NothingToRelease);

        // Guard
        enter_transfer(e)?;

        let (fee_cut, insurance_cut) = calc_fee_splits(remaining, e.fee_bps, e.insurance_bps);
        let mut seller_amount = remaining.saturating_sub(fee_cut + insurance_cut);

        // Late penalty
        let now = Clock::get()?.unix_timestamp;
        if e.deliver_by_ts > 0 && now > e.deliver_by_ts {
            let penalty = mul_bps(seller_amount, e.late_penalty_bps);
            seller_amount = seller_amount.saturating_sub(penalty);
            if penalty > 0 {
                transfer_from_vault(
                    e,
                    &ctx.accounts.token_program,
                    &ctx.accounts.vault_authority,
                    &ctx.accounts.vault_ata,
                    &ctx.accounts.buyer_ata,
                    penalty,
                )?;
            }
        }

        // Route fees
        if fee_cut > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.treasury_ata,
                fee_cut,
            )?;
        }
        if insurance_cut > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.insurance_ata,
                insurance_cut,
            )?;
        }

        // Pay seller
        if seller_amount > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.seller_ata,
                seller_amount,
            )?;
        }

        e.state = EscrowState::Released as u8;
        e.released_ts = now;

        exit_transfer(e);

        emit!(PaymentReleased {
            project_id: e.project_id,
            seller: e.seller,
            amount: remaining,
            fee_cut,
            insurance_cut,
            seller_received: seller_amount,
            when: e.released_ts
        });

        Ok(())
    }

    /// Releases retention after the warranty window passes.
    pub fn release_retention(ctx: Context<ReleaseCommon>) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!(!e.retention_released, EscrowError::RetentionAlreadyReleased);
        let now = Clock::get()?.unix_timestamp;
        require!(now >= e.warranty_end_ts, EscrowError::WarrantyNotEnded);

        let retention = calc_retention(e.amount, e.retention_bps);
        require!(ctx.accounts.vault_ata.amount >= retention, EscrowError::VaultBalanceLow);

        // Guard
        enter_transfer(e)?;

        // Retention pays out to seller with no extra late penalty (warranty passed)
        let (fee_cut, insurance_cut) = calc_fee_splits(retention, e.fee_bps, e.insurance_bps);
        let seller_amount = retention.saturating_sub(fee_cut + insurance_cut);

        if fee_cut > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.treasury_ata,
                fee_cut,
            )?;
        }
        if insurance_cut > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.insurance_ata,
                insurance_cut,
            )?;
        }

        if seller_amount > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.seller_ata,
                seller_amount,
            )?;
        }

        e.retention_released = true;

        exit_transfer(e);

        emit!(RetentionReleased {
            project_id: e.project_id,
            gross: retention,
            fee_cut,
            insurance_cut,
            seller_received: seller_amount
        });
        Ok(())
    }

    /* ------------------------- Cancel / Dispute Flow ------------------------ */

    pub fn request_cancel(ctx: Context<BuyerOrSeller>) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!(e.cancel_requested_by == Pubkey::default(), EscrowError::CancelAlreadyRequested);
        let caller = ctx.accounts.actor.key();
        require!(caller == e.buyer || caller == e.seller, EscrowError::Unauthorized);

        e.cancel_requested_by = caller;
        emit!(CancelRequested { project_id: e.project_id, by: caller });
        Ok(())
    }

    /// Counterparty approves; refunds remaining vault balance to buyer.
    pub fn approve_cancel(ctx: Context<ApproveCancel>) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        let caller = ctx.accounts.actor.key();
        require!(e.cancel_requested_by != Pubkey::default(), EscrowError::CancelNotRequested);
        require!(caller != e.cancel_requested_by, EscrowError::Unauthorized);

        let remaining = ctx.accounts.vault_ata.amount;
        require!(remaining > 0, EscrowError::NothingToRelease);

        transfer_from_vault(
            e,
            &ctx.accounts.token_program,
            &ctx.accounts.vault_authority,
            &ctx.accounts.vault_ata,
            &ctx.accounts.buyer_ata,
            remaining,
        )?;

        e.state = EscrowState::Refunded as u8;
        emit!(CancelApprovedAndRefunded { project_id: e.project_id, amount: remaining });
        Ok(())
    }

    pub fn open_dispute(ctx: Context<BuyerOrSeller>, reason_code: u16, evidence_hash: [u8; 32]) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!(!e.dispute_open, EscrowError::DisputeAlreadyOpen);
        e.dispute_open = true;
        e.state = EscrowState::Dispute as u8;
        emit!(DisputeOpened { project_id: e.project_id, reason_code, evidence_hash });
        Ok(())
    }

    /// Arbiter resolves dispute with outcome: Refund, Release, or Split (seller_pct bps).
    pub fn resolve_dispute(
        ctx: Context<ArbiterResolve>,
        outcome: DisputeOutcome,
        seller_pct_bps: u16,
    ) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!(e.dispute_open, EscrowError::NoOpenDispute);

        // Guard
        enter_transfer(e)?;

        let total = ctx.accounts.vault_ata.amount;
        require!(total > 0, EscrowError::NothingToRelease);

        let (buyer_amt, seller_amt) = match outcome {
            DisputeOutcome::Refund => (total, 0),
            DisputeOutcome::Release => (0, total),
            DisputeOutcome::Split => {
                let seller_amt = mul_bps(total, seller_pct_bps);
                (total.saturating_sub(seller_amt), seller_amt)
            }
        };

        // Apply fees on the seller portion only (platform earns on payout)
        let (fee_cut, insurance_cut) = if seller_amt > 0 {
            calc_fee_splits(seller_amt, e.fee_bps, e.insurance_bps)
        } else {
            (0, 0)
        };
        let seller_net = seller_amt.saturating_sub(fee_cut + insurance_cut);

        if buyer_amt > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.buyer_ata,
                buyer_amt,
            )?;
        }
        if seller_net > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.seller_ata,
                seller_net,
            )?;
        }
        if fee_cut > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.treasury_ata,
                fee_cut,
            )?;
        }
        if insurance_cut > 0 {
            transfer_from_vault(
                e,
                &ctx.accounts.token_program,
                &ctx.accounts.vault_authority,
                &ctx.accounts.vault_ata,
                &ctx.accounts.insurance_ata,
                insurance_cut,
            )?;
        }

        e.dispute_open = false;
        e.state = if seller_amt > 0 { EscrowState::Released as u8 } else { EscrowState::Refunded as u8 };
        e.released_ts = Clock::get()?.unix_timestamp;

        exit_transfer(e);

        emit!(DisputeResolved {
            project_id: e.project_id,
            outcome,
            buyer_received: buyer_amt,
            seller_received: seller_net,
            fee_cut,
            insurance_cut
        });
        Ok(())
    }

    /* -------------------------- Evidence & Attestations --------------------- */

    /// Optional: attach an evidence hash (plus short URI bytes) to escrow.
    pub fn attach_evidence(ctx: Context<BuyerOrSeller>, hash: [u8; 32], uri: Vec<u8>) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        let mut short = [0u8; 96];
        let n = short.len().min(uri.len());
        short[..n].copy_from_slice(&uri[..n]);
        e.last_evidence_hash = hash;
        e.last_evidence_uri96 = short;
        emit!(EvidenceAttached { project_id: e.project_id, hash, uri_prefix: short });
        Ok(())
    }

    /// Create an attestation PDA entry (e.g., inspector note).
    pub fn add_attestation(ctx: Context<AddAttestation>, hash: [u8; 32], uri: Vec<u8>) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        let a = &mut ctx.accounts.attestation;
        let mut short = [0u8; 96];
        let n = short.len().min(uri.len());
        short[..n].copy_from_slice(&uri[..n]);

        a.escrow = e.key();
        a.attester = ctx.accounts.attester.key();
        a.hash = hash;
        a.uri96 = short;
        a.ts = Clock::get()?.unix_timestamp;
        a.bump = ctx.bumps.attestation;

        e.attestations_count = e.attestations_count.saturating_add(1);

        emit!(Attested {
            project_id: e.project_id,
            attester: a.attester,
            hash,
            uri_prefix: short
        });
        Ok(())
    }

    /* ----------------------------- NFT Receipt ------------------------------ */

    /// Initialize a 0-decimal mint for receipt NFT; program is mint+freeze authority.
    pub fn init_receipt_nft(ctx: Context<InitReceiptNft>) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!(e.nft_enabled, EscrowError::NftDisabled);

        // Record mint on escrow
        e.receipt_nft_mint = ctx.accounts.nft_mint.key();

        // Mint 1 to buyer and freeze it (soulbound-ish)
        let mint_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            MintTo {
                mint: ctx.accounts.nft_mint.to_account_info(),
                to: ctx.accounts.buyer_nft_ata.to_account_info(),
                authority: ctx.accounts.nft_mint_authority.to_account_info(),
            },
        );
        token::mint_to(mint_ctx, 1)?;

        // Freeze
        let freeze_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            FreezeAccount {
                account: ctx.accounts.buyer_nft_ata.to_account_info(),
                mint: ctx.accounts.nft_mint.to_account_info(),
                authority: ctx.accounts.nft_freeze_authority.to_account_info(),
            },
        );
        token::freeze_account(freeze_ctx)?;

        emit!(ReceiptNftMinted { project_id: e.project_id, mint: e.receipt_nft_mint, to: ctx.accounts.buyer_nft_ata.key() });
        Ok(())
    }

    /// Burn or unfreeze on final release (choose policy). Here: burn on release.
    pub fn finalize_receipt_nft(ctx: Context<FinalizeReceiptNft>, burn: bool) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        require!(e.state == EscrowState::Released as u8, EscrowError::BadState);
        require!(e.nft_enabled, EscrowError::NftDisabled);
        require!(e.receipt_nft_mint == ctx.accounts.nft_mint.key(), EscrowError::BadNftMint);

        if burn {
            // Thaw then burn
            let thaw = CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                ThawAccount {
                    account: ctx.accounts.buyer_nft_ata.to_account_info(),
                    mint: ctx.accounts.nft_mint.to_account_info(),
                    authority: ctx.accounts.nft_freeze_authority.to_account_info(),
                },
            );
            token::thaw_account(thaw)?;

            let burn_ctx = CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Burn {
                    mint: ctx.accounts.nft_mint.to_account_info(),
                    from: ctx.accounts.buyer_nft_ata.to_account_info(),
                    authority: ctx.accounts.nft_mint_authority.to_account_info(),
                },
            );
            token::burn(burn_ctx, 1)?;
        } else {
            // Thaw and leave transferable if desired
            let thaw = CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                ThawAccount {
                    account: ctx.accounts.buyer_nft_ata.to_account_info(),
                    mint: ctx.accounts.nft_mint.to_account_info(),
                    authority: ctx.accounts.nft_freeze_authority.to_account_info(),
                },
            );
            token::thaw_account(thaw)?;
        }

        emit!(ReceiptNftFinalized { project_id: e.project_id, mint: e.receipt_nft_mint, burned: burn });
        Ok(())
    }

    /* -------------------------- Authority Management ------------------------ */

    pub fn update_oracles(ctx: Context<BuyerOrSeller>, new_oracles: Vec<Pubkey>, new_quorum_m: u8) -> Result<()> {
        require!(new_oracles.len() <= MAX_ORACLES, EscrowError::TooManyOracles);
        require!(new_quorum_m >= QUORUM_MIN, EscrowError::BadQuorum);
        let e = &mut ctx.accounts.escrow;
        e.oracles = [Pubkey::default(); MAX_ORACLES];
        for (i, pk) in new_oracles.iter().enumerate() {
            e.oracles[i] = *pk;
        }
        e.oracles_len = new_oracles.len() as u8;
        e.quorum_m = new_quorum_m;
        emit!(OraclesUpdated { project_id: e.project_id, quorum_m: new_quorum_m, count: e.oracles_len });
        Ok(())
    }

    pub fn update_seller_dest(ctx: Context<SellerOnly>, new_seller: Pubkey) -> Result<()> {
        let e = &mut ctx.accounts.escrow;
        e.seller = new_seller;
        emit!(SellerUpdated { project_id: e.project_id, new_seller });
        Ok(())
    }

    /* -------------------------- Cron-friendly Timeout ---------------------- */

    /// Iterate over timeouts (stubbed for PoC; batching left for future).
    pub fn process_timeouts(_ctx: Context<ProcessTimeouts>, _limit: u8) -> Result<()> {
        emit!(TimeoutsProcessed { processed: 0 });
        Ok(())
    }
}

/* ============================== State & Types ============================== */

#[account]
pub struct Config {
    pub authority: Pubkey,
    pub pending_authority: Pubkey,
    pub treasury: Pubkey,
    pub insurance_treasury: Pubkey,
    pub fee_bps: u16,
    pub insurance_bps: u16,
    pub retention_bps: u16,
    pub warranty_days: i64,
    pub quorum_m: u8,
    pub arbiter: Pubkey,
    pub bump: u8,
    pub reserved: [u8; 64],
}
impl Config {
    pub const SPACE: usize = 8 + 32 + 32 + 32 + 32 + 2 + 2 + 2 + 8 + 1 + 32 + 1 + 64;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum EscrowState {
    Open = 1,
    Verified = 2,
    PartiallyReleased = 3,
    Released = 4,
    Refunded = 5,
    Dispute = 6,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum DisputeOutcome {
    Refund,
    Release,
    Split,
}

#[account]
pub struct Escrow {
    // Keys
    pub project_id: u64,
    pub buyer: Pubkey,
    pub seller: Pubkey,
    pub mint: Pubkey,
    pub config: Pubkey,

    // Economics
    pub amount: u64,
    pub fee_bps: u16,
    pub insurance_bps: u16,
    pub retention_bps: u16,
    pub late_penalty_bps: u16, // default 0 unless set
    pub price_snapshot_1e6: u64, // optional USD notional snapshot

    // Oracles & quorum
    pub quorum_m: u8,
    pub oracles_len: u8,
    pub oracles: [Pubkey; MAX_ORACLES],

    // Lifecycle
    pub state: u8,
    pub created_ts: i64,
    pub verified_ts: i64,
    pub released_ts: i64,
    pub verify_by_ts: i64,
    pub deliver_by_ts: i64,
    pub warranty_end_ts: i64,

    // Milestones (fixed array)
    pub milestones_len: u8,
    pub milestones: [Milestone; MAX_MILESTONES],

    // Evidence and attestations
    pub last_evidence_hash: [u8; 32],
    pub last_evidence_uri96: [u8; 96],
    pub attestations_count: u32,

    // Cancel / dispute
    pub cancel_requested_by: Pubkey,
    pub dispute_open: bool,

    // NFT receipt option
    pub nft_enabled: bool,
    pub receipt_nft_mint: Pubkey,

    // Guards & misc
    pub in_transfer: bool,
    pub in_progress: bool,
    pub retention_released: bool,
    pub last_ix_nonce: u64,

    // Bumps
    pub bump: u8,
    pub vault_bump: u8,

    pub reserved: [u8; 256],
}
impl Escrow {
    pub const SPACE: usize =
        8 + // disc
        8 + 32 + 32 + 32 + 32 + // ids
        8 + 2 + 2 + 2 + 2 + 8 + // economics
        1 + 1 + (32 * MAX_ORACLES) + // quorum/oracles
        1 + 8 + 8 + 8 + 8 + 8 + 8 + // lifecycle
        1 + (Milestone::SPACE * MAX_MILESTONES) + // milestones
        32 + 96 + 4 + // evidence
        32 + 1 + // cancel/dispute
        1 + 32 + // nft
        1 + 1 + 1 + 8 + // guards/misc
        1 + 1 + // bumps
        256; // reserved

    pub fn milestones(&self) -> &[Milestone] {
        &self.milestones[..(self.milestones_len as usize)]
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct Milestone {
    pub id: u8,
    pub amount: u64,
    pub verified: bool,
    pub released: bool,
    pub verify_ts: i64,
    pub evidence_hash: [u8; 32],
    pub reserved: [u8; 7],
}
impl Milestone {
    pub const EMPTY: Milestone = Milestone { id: 0, amount: 0, verified: false, released: false, verify_ts: 0, evidence_hash: [0u8;32], reserved: [0u8;7] };
    pub const SPACE: usize = 1 + 8 + 1 + 1 + 8 + 32 + 7;
}

#[account]
pub struct ProjectIndex {
    pub project_id: u64,
    pub escrow: Pubkey,
    pub bump: u8,
}
impl ProjectIndex {
    pub const SPACE: usize = 8 + 8 + 32 + 1;
}

#[account]
pub struct Attestation {
    pub escrow: Pubkey,
    pub attester: Pubkey,
    pub hash: [u8; 32],
    pub uri96: [u8; 96],
    pub ts: i64,
    pub bump: u8,
}
impl Attestation {
    pub const SPACE: usize = 8 + 32 + 32 + 32 + 96 + 8 + 1;
}

/* =============================== Accounts ================================= */

#[derive(Accounts)]
pub struct InitConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    /// CHECK: treasury owner pubkey
    pub treasury: UncheckedAccount<'info>,
    /// CHECK: insurance treasury owner pubkey
    pub insurance_treasury: UncheckedAccount<'info>,
    /// CHECK: arbiter role pubkey
    pub arbiter: UncheckedAccount<'info>,

    #[account(
        init,
        payer = authority,
        space = Config::SPACE,
        seeds = [b"config"],
        bump
    )]
    pub config: Account<'info, Config>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ConfigAuthority<'info> {
    #[account(mut, seeds = [b"config"], bump = config.bump, has_one = authority)]
    pub config: Account<'info, Config>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct AcceptAuthority<'info> {
    #[account(mut, seeds = [b"config"], bump = config.bump)]
    pub config: Account<'info, Config>,
    /// The account equal to `config.pending_authority`
    pub new_authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(project_id: u64)]
pub struct CreateEscrow<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,

    /// CHECK: seller key (will receive payouts)
    pub seller: UncheckedAccount<'info>,

    #[account(mut)]
    pub mint: Account<'info, Mint>,

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = buyer
    )]
    pub buyer_ata: Account<'info, TokenAccount>,

    /// PDA escrow state
    #[account(
        init,
        payer = buyer,
        space = Escrow::SPACE,
        seeds = [
            b"escrow".as_ref(),
            project_id.to_be_bytes().as_ref(),
            buyer.key().as_ref(),
            seller.key().as_ref(),
            mint.key().as_ref()
        ],
        bump
    )]
    pub escrow: Account<'info, Escrow>,

    /// PDA index mapping project_id → escrow
    #[account(
        init,
        payer = buyer,
        space = ProjectIndex::SPACE,
        seeds = [b"project_index".as_ref(), project_id.to_be_bytes().as_ref()],
        bump
    )]
    pub project_index: Account<'info, ProjectIndex>,

    /// PDA authority for vault
    /// CHECK: PDA only used for signing
    #[account(
        seeds = [b"vault".as_ref(), escrow.key().as_ref()],
        bump
    )]
    pub vault_authority: UncheckedAccount<'info>,

    /// Vault ATA
    #[account(
        init_if_needed,
        payer = buyer,
        associated_token::mint = mint,
        associated_token::authority = vault_authority
    )]
    pub vault_ata: Account<'info, TokenAccount>,

    #[account(
        seeds = [b"config"],
        bump = config.bump
    )]
    pub config: Account<'info, Config>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

/* ======== Other context stubs you’ll need (minimal, compilable) ======== */

#[derive(Accounts)]
pub struct BuyerOrSeller<'info> {
    #[account(mut)]
    pub actor: Signer<'info>,
    #[account(mut)]
    pub escrow: Account<'info, Escrow>,
}

#[derive(Accounts)]
pub struct SellerOnly<'info> {
    #[account(mut)]
    pub seller: Signer<'info>,
    #[account(mut, has_one = seller)]
    pub escrow: Account<'info, Escrow>,
}

#[derive(Accounts)]
pub struct RefundBuyer<'info> {
    #[account(mut)]
    pub escrow: Account<'info, Escrow>,
    /// CHECK: PDA vault authority
    pub vault_authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub vault_ata: Account<'info, TokenAccount>,
    #[account(mut)]
    pub buyer_ata: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct VerifyWithQuorum<'info> {
    #[account(mut)]
    pub escrow: Account<'info, Escrow>,
}

#[derive(Accounts)]
pub struct ReleaseCommon<'info> {
    #[account(mut)]
    pub escrow: Account<'info, Escrow>,
    /// CHECK
    pub vault_authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub vault_ata: Account<'info, TokenAccount>,
    #[account(mut)]
    pub seller_ata: Account<'info, TokenAccount>,
    #[account(mut)]
    pub buyer_ata: Account<'info, TokenAccount>,
    #[account(mut)]
    pub treasury_ata: Account<'info, TokenAccount>,
    #[account(mut)]
    pub insurance_ata: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ApproveCancel<'info> {
    #[account(mut)]
    pub actor: Signer<'info>,
    #[account(mut)]
    pub escrow: Account<'info, Escrow>,
    /// CHECK: PDA vault authority
    pub vault_authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub vault_ata: Account<'info, TokenAccount>,
    #[account(mut)]
    pub buyer_ata: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ArbiterResolve<'info> {
    #[account(mut, seeds = [b"config"], bump = config.bump, has_one = arbiter)]
    pub config: Account<'info, Config>,
    pub arbiter: Signer<'info>,
    #[account(mut)]
    pub escrow: Account<'info, Escrow>,
    /// CHECK
    pub vault_authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub vault_ata: Account<'info, TokenAccount>,
    #[account(mut)]
    pub buyer_ata: Account<'info, TokenAccount>,
    #[account(mut)]
    pub seller_ata: Account<'info, TokenAccount>,
    #[account(mut)]
    pub treasury_ata: Account<'info, TokenAccount>,
    #[account(mut)]
    pub insurance_ata: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct AddAttestation<'info> {
    #[account(mut)]
    pub escrow: Account<'info, Escrow>,
    #[account(mut)]
    pub attester: Signer<'info>, // mutable: pays for init
    #[account(
        init,
        payer = attester,
        space = Attestation::SPACE,
        seeds = [b"attestation".as_ref(), escrow.key().as_ref(), attester.key().as_ref()],
        bump
    )]
    pub attestation: Account<'info, Attestation>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitReceiptNft<'info> {
    #[account(mut)]
    pub escrow: Account<'info, Escrow>,
    #[account(mut)]
    pub nft_mint: Account<'info, Mint>,
    #[account(mut)]
    pub buyer_nft_ata: Account<'info, TokenAccount>,
    /// CHECK: Program acts as mint authority
    pub nft_mint_authority: UncheckedAccount<'info>,
    /// CHECK: Program acts as freeze authority
    pub nft_freeze_authority: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct FinalizeReceiptNft<'info> {
    #[account(mut)]
    pub escrow: Account<'info, Escrow>,
    #[account(mut)]
    pub nft_mint: Account<'info, Mint>,
    #[account(mut)]
    pub buyer_nft_ata: Account<'info, TokenAccount>,
    /// CHECK
    pub nft_mint_authority: UncheckedAccount<'info>,
    /// CHECK
    pub nft_freeze_authority: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ProcessTimeouts<'info> {
    pub caller: Signer<'info>,
}

/* =============================== Events =================================== */

#[event] pub struct ConfigUpdated { pub fee_bps: u16, pub insurance_bps: u16, pub retention_bps: u16, pub warranty_days: i64, pub quorum_m: u8 }
#[event] pub struct ConfigAuthorityProposed { pub proposed: Pubkey }
#[event] pub struct ConfigAuthorityTransferred { pub new_authority: Pubkey }

#[event] pub struct EscrowCreated { pub project_id: u64, pub buyer: Pubkey, pub seller: Pubkey, pub mint: Pubkey, pub amount: u64, pub quorum_m: u8, pub price_snapshot_1e6: u64 }
#[event] pub struct DeadlinesSet { pub project_id: u64, pub verify_by_ts: i64, pub deliver_by_ts: i64 }
#[event] pub struct ProgressMarked { pub project_id: u64, pub ts: i64 }
#[event] pub struct ExpiredAndRefunded { pub project_id: u64, pub amount: u64 }
#[event] pub struct DeliveryVerified { pub project_id: u64, pub quorum_votes: u8, pub when: i64 }

#[event] pub struct MilestoneAdded { pub project_id: u64, pub id: u8, pub amount: u64, pub evidence_hash: [u8;32] }
#[event] pub struct MilestoneVerified { pub project_id: u64, pub id: u8, pub when: i64 }
#[event] pub struct MilestoneReleased { pub project_id: u64, pub id: u8, pub gross: u64, pub fee_cut: u64, pub insurance_cut: u64, pub seller_received: u64 }

#[event] pub struct PaymentReleased { pub project_id: u64, pub seller: Pubkey, pub amount: u64, pub fee_cut: u64, pub insurance_cut: u64, pub seller_received: u64, pub when: i64 }
#[event] pub struct RetentionReleased { pub project_id: u64, pub gross: u64, pub fee_cut: u64, pub insurance_cut: u64, pub seller_received: u64 }

#[event] pub struct CancelRequested { pub project_id: u64, pub by: Pubkey }
#[event] pub struct CancelApprovedAndRefunded { pub project_id: u64, pub amount: u64 }

#[event] pub struct DisputeOpened { pub project_id: u64, pub reason_code: u16, pub evidence_hash: [u8;32] }
#[event] pub struct DisputeResolved { pub project_id: u64, pub outcome: DisputeOutcome, pub buyer_received: u64, pub seller_received: u64, pub fee_cut: u64, pub insurance_cut: u64 }

#[event] pub struct EvidenceAttached { pub project_id: u64, pub hash: [u8;32], pub uri_prefix: [u8;96] }
#[event] pub struct Attested { pub project_id: u64, pub attester: Pubkey, pub hash: [u8;32], pub uri_prefix: [u8;96] }

#[event] pub struct ReceiptNftMinted { pub project_id: u64, pub mint: Pubkey, pub to: Pubkey }
#[event] pub struct ReceiptNftFinalized { pub project_id: u64, pub mint: Pubkey, pub burned: bool }

#[event] pub struct TimeoutsProcessed { pub processed: u8 }
#[event] pub struct OraclesUpdated { pub project_id: u64, pub quorum_m: u8, pub count: u8 }
#[event] pub struct SellerUpdated { pub project_id: u64, pub new_seller: Pubkey }

/* ================================ Errors ================================== */

#[error_code]
pub enum EscrowError {
    #[msg("Amount must be greater than zero.")] ZeroAmount,
    #[msg("Quorum must be at least 1.")] BadQuorum,
    #[msg("Too many oracles.")] TooManyOracles,
    #[msg("Nonce must increase.")] BadNonce,
    #[msg("Escrow is in a wrong state for this action.")] BadState,
    #[msg("Escrow not expired.")] NotExpired,
    #[msg("Vault balance too low.")] VaultBalanceLow,
    #[msg("Provided project_id does not match escrow.")] ProjectMismatch,
    #[msg("Oracle quorum not met.")] QuorumNotMet,
    #[msg("Already verified.")] AlreadyVerified,
    #[msg("Milestone not releasable.")] MilestoneNotReleasable,
    #[msg("Bad milestone id.")] BadMilestoneId,
    #[msg("Nothing to release.")] NothingToRelease,
    #[msg("Retention already released.")] RetentionAlreadyReleased,
    #[msg("Warranty not ended.")] WarrantyNotEnded,
    #[msg("Cancel already requested.")] CancelAlreadyRequested,
    #[msg("Unauthorized.")] Unauthorized,
    #[msg("Cancel not requested.")] CancelNotRequested,
    #[msg("No open dispute.")] NoOpenDispute,
    #[msg("Dispute already open.")] DisputeAlreadyOpen,
    #[msg("Receipt NFT not enabled.")] NftDisabled,
    #[msg("Wrong NFT mint provided.")] BadNftMint,
    #[msg("Too many milestones.")] TooManyMilestones,
    #[msg("Milestones exceed total escrow amount.")] MilestoneOverTotal,
    #[msg("Bad authority accept.")] BadAuthorityAccept,
    #[msg("Reentrancy detected.")] Reentrancy,
}

/* ============================== Helpers/Utils ============================== */

fn mul_bps(amount: u64, bps: u16) -> u64 {
    amount.saturating_mul(bps as u64) / 10_000
}

fn calc_fee_splits(amount: u64, fee_bps: u16, insurance_bps: u16) -> (u64, u64) {
    (mul_bps(amount, fee_bps), mul_bps(amount, insurance_bps))
}

fn calc_retention(total: u64, retention_bps: u16) -> u64 {
    mul_bps(total, retention_bps)
}

fn enter_transfer(e: &mut Account<Escrow>) -> Result<()> {
    require!(!e.in_transfer, EscrowError::Reentrancy);
    e.in_transfer = true;
    Ok(())
}
fn exit_transfer(e: &mut Account<Escrow>) {
    e.in_transfer = false;
}

/// Transfer tokens out of the vault using the PDA signer.
fn transfer_from_vault<'info>(
    e: &Account<'info, Escrow>,
    token_program: &Program<'info, Token>,
    vault_authority: &UncheckedAccount<'info>,
    from_vault_ata: &Account<'info, TokenAccount>,
    to_ata: &Account<'info, TokenAccount>,
    amount: u64,
) -> Result<()> {
    // Avoid temporary key drop: bind to a local
    let escrow_key: Pubkey = e.key();
    let bump = e.vault_bump;
    let seeds_slice: [&[u8]; 3] = [b"vault", escrow_key.as_ref(), &[bump]];
    let signer_seeds: [&[&[u8]]; 1] = [&seeds_slice];

    let cpi_accounts = Transfer {
        from: from_vault_ata.to_account_info(),
        to: to_ata.to_account_info(),
        authority: vault_authority.to_account_info(),
    };
    let cpi_ctx = CpiContext::new(token_program.to_account_info(), cpi_accounts)
        .with_signer(&signer_seeds);
    token::transfer(cpi_ctx, amount)
}

/// Count how many of the remaining accounts are signers AND are in the oracle set.
fn count_quorum_votes(e: &Account<Escrow>, remaining: &[AccountInfo]) -> Result<usize> {
    let mut votes = 0usize;
    for ai in remaining.iter() {
        if !ai.is_signer { continue; }
        for i in 0..(e.oracles_len as usize) {
            if e.oracles[i] != Pubkey::default() && e.oracles[i] == ai.key() {
                votes += 1;
                break;
            }
        }
    }
    Ok(votes)
}
