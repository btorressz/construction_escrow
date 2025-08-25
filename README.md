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
