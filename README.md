# construction_escrow

# 🏗️ Construction Escrow Protocol

## 🌐 Overview

The **Construction Escrow Program** is a Solana smart contract built with the Anchor framework.  
It manages escrow accounts between **buyers** and **sellers** of construction materials, ensuring funds are only released after verified delivery.  

It supports **milestones**, **multi-oracle verification**, **time-locks**, **retention (holdbacks)**, **NFT receipts**, and **dispute resolution**.  
A comprehensive TypeScript test suite is included to demonstrate account setup, PDA derivation, and step-by-step validation.

> **Note:** This project was developed as a **proof of concept** for a client researching how **construction escrow systems** could be implemented on blockchain, specifically leveraging the **Solana ecosystem**. The goal was to explore transparency, automation, and trustless settlement for large-scale material contracts.

---

## ⚙️ Constants

- `MAX_ORACLES = 8` → maximum number of oracles/verifiers per project.  
- `MAX_MILESTONES = 10` → maximum number of payment milestones.  
- `QUORUM_MIN = 1` → minimum quorum for oracle verification.  

---

## 📦 Accounts

### 🔹 Config
Holds global configuration for the protocol.  
Fields:
- `authority` → current admin authority of the market.  
- `pending_authority` → new authority waiting to be accepted.  
- `treasury` → PDA that collects protocol fees.  
- `insurance_treasury` → PDA that collects insurance fees.  
- `fee_bps` → base protocol fee (basis points).  
- `insurance_bps` → insurance fund fee (basis points).  
- `retention_bps` → % held back as retention until warranty ends.  
- `warranty_days` → warranty period in days.  
- `quorum_m` → required quorum (M-of-N) for oracle verification.  
- `arbiter` → designated dispute resolver.  

---

### 🔹 Escrow
Represents a construction project escrow account.  
Fields:
- `project_id` → unique identifier.  
- `buyer`, `seller` → counterparties.  
- `mint` → SPL token used for payment.  
- `config` → reference to Config PDA.  
- `amount` → total escrowed amount.  
- `fee_bps`, `insurance_bps`, `retention_bps` → copied from Config at creation.  
- `late_penalty_bps` → optional penalty for late delivery.  
- `price_snapshot_1e6` → price snapshot (USD notional, 6 decimals).  
- `quorum_m` → quorum required for verification.  
- `oracles` → array of oracle pubkeys.  
- `state` → escrow state machine:
  - `Open`
  - `Verified`
  - `PartiallyReleased`
  - `Released`
  - `Refunded`
  - `Dispute`  
- `created_ts`, `verified_ts`, `released_ts` → lifecycle timestamps.  
- `verify_by_ts`, `deliver_by_ts` → deadlines.  
- `warranty_end_ts` → timestamp when retention can be released.  
- `milestones` → fixed array of milestone structs.  
- `last_evidence_hash` → SHA-256 evidence (docs, photos).  
- `attestations_count` → number of attestations attached.  
- `cancel_requested_by` → if cancel was requested, stores who requested.  
- `dispute_open` → flag for dispute state.  
- `nft_enabled` → whether to issue an NFT receipt.  
- `receipt_nft_mint` → mint address for NFT receipt.  
- `in_transfer` → reentrancy guard.  
- `retention_released` → true once retention is paid out.  

---

### 🔹 Milestone
Represents a stage payment.  
Fields:
- `id` → milestone index.  
- `amount` → payment amount.  
- `verified` → true once verified by oracles.  
- `released` → true once funds are released.  
- `verify_ts` → timestamp when verified.  
- `evidence_hash` → SHA-256 hash of delivery evidence.  

---

### 🔹 ProjectIndex
Maps `project_id → escrow PDA` for quick lookups.

---

### 🔹 Attestation
Represents an external attestation (e.g., inspector note).  
Fields:
- `escrow` → escrow it belongs to.  
- `attester` → signer.  
- `hash` → SHA-256 evidence hash.  
- `uri96` → optional URI prefix (truncated to 96 bytes).  
- `ts` → timestamp.  

---

## 🛠️ Instructions (Functions)

### 🔧 Config & Authority
- `init_config` → initialize Config PDA.  
- `update_fee_splits` → update fee % and insurance %.  
- `transfer_market_authority_propose` → propose new authority.  
- `transfer_market_authority_accept` → accept authority transfer.  

---

### 💰 Escrow Lifecycle
- `create_escrow(project_id, buyer, seller, amount, oracles, quorum_m, price_snapshot, nft_enabled)`  
  Creates a new escrow, transfers buyer’s tokens to a PDA vault.  

- `set_deadlines(verify_by_ts, deliver_by_ts)`  
  Set verification and delivery deadlines.  

- `mark_in_progress()`  
  Seller marks project as started.  

- `expire_and_refund()`  
  Refund buyer if verification not done by deadline.  

---


### ✅ Verification & Milestones
- `verify_delivery(project_id)`  
  Verifies delivery using M-of-N oracle signatures.  

- `add_milestone(amount, evidence_hash)`  
  Add a milestone with supporting evidence.  

- `verify_milestone(milestone_id)`  
  Verify a milestone with oracle quorum.  

- `release_for_milestone(milestone_id)`  
  Release funds for a verified milestone (fees + penalties applied).  

- `release_payment()`  
  Release all remaining funds (minus retention).  

- `release_retention()`  
  Release retention after warranty ends.  

---

### 🚫 Cancel & Dispute
- `request_cancel()` → buyer/seller requests cancel.  
- `approve_cancel()` → counterparty approves cancel → buyer refunded.  
- `open_dispute(reason_code, evidence_hash)` → open a dispute.  
- `resolve_dispute(outcome, seller_pct_bps)` → arbiter resolves dispute (refund, release, split).  

---

### 📜 Evidence & Compliance
- `attach_evidence(hash, uri)` → attach evidence to escrow.  
- `add_attestation(hash, uri)` → add inspector or third-party attestation.  

---

### 🪪 NFT Receipts
- `init_receipt_nft()` → mint a **soulbound NFT** as buyer’s receipt.  
- `finalize_receipt_nft(burn: bool)` → burn or unfreeze NFT at final release.  

---

### 🔒 Authority & Oracles
- `update_oracles(new_oracles, new_quorum_m)` → update oracle set.  
- `update_seller_dest(new_seller)` → update seller payout destination.  

---

### ⏱️ Timeout Processing
- `process_timeouts(limit)` → cron-friendly function to auto-refund expired escrows.  

---

## 🧪 Tests (TypeScript)

The test suite demonstrates:

1. **Mint & ATA setup**  
   - Creates SPL mint and funds buyer.  
   - Ensures all ATAs exist.  

2. **Config initialization**  
   - Creates `Config` PDA with fees, retention, warranty.  

3. **Escrow creation**  
   - Derives PDAs (`escrow`, `vault_authority`, `project_index`).  
   - Transfers buyer tokens into vault.  

4. **Milestone flow**  
   - Adds milestone, verifies with oracle quorum.  
   - Releases funds, applying fees + insurance cut.  

5. **Final release & retention**  
   - Releases payment to seller.  
   - Holds retention until warranty ends, then releases.  

6. **Failure case**  
   - Tries to release before verification → fails with expected error logs.  

✅ Each step uses **assertions** and **console logs** to confirm state transitions and balances.  
✅ Program Derived Addresses (PDAs) are derived exactly as in Rust using `seeds`.  
✅ Errors are captured with `try/catch` and full transaction logs printed for debugging.  

---
