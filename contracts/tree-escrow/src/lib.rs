#![no_std]

//! Tree Escrow Contract
//!
//! Two parallel funding flows backed by shared oracle infrastructure:
//!
//! ## Single-donor flow (keyed by farmer address)
//!   • `deposit` / `batch_deposit` — donor funds an escrow for a farmer
//!   • `verify_progress` — admin-verified progress update; streams 10% of
//!     the escrow to the planter on each of 5 calls (50% total).
//!     The first call also mints TREE rewards and records planting proof.
//!   • `verify_survival` releases the remaining escrow (≥6 months past
//!     planting, survival rate ≥ threshold).
//!   • `refund` returns funds to the donor before the first progress update.
//!
//! ## Co-funded flow (keyed by tree_id) — Closes #402
//!   • `register_tree` — admin opens a co-fundable tree escrow
//!   • `contribute` — any funder adds to the pool
//!   • `release_proportional` — pays each contributor proportional to their
//!     share of the total pool, gated by an oracle-confirmed survival rate
//!     ≥ threshold. The integer-division remainder goes to the largest
//!     contributor.
//!
//! ## Oracle survival verification — Closes #394
//!   • `submit_survival_report` — registered oracle attests on-chain to a
//!     tree's survival rate. Stored as an `OracleReport` keyed by tree_id.
//!   • The configurable `SurvivalThreshold` (set at init) gates survival
//!     release for both flows.
//!
//! ## Tree ID QR hash — Closes #496
//!   • `register_qr_hash` — admin stores the SHA-256 of a physical QR label
//!     payload against a tree_id for later verification.
//!   • `get_qr_hash` — retrieve the stored hash for off-chain label checking.

#[cfg(test)]
extern crate std;

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, symbol_short, token, Address, BytesN, Env, Vec,
};
use harvesta_errors::HarvestaError;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Tranche 1: 30% at planting
const TRANCHE_1_BPS: i128 = 3_000;
/// Tranche 2: 40% at 6-month survival
const TRANCHE_2_BPS: i128 = 4_000;
/// Tranche 3: 30% at 1-year milestone
const TRANCHE_3_BPS: i128 = 3_000;
/// 10% per progress update in basis points
const STREAM_BPS: i128 = 1_000;
const BPS_DENOM: i128 = 10_000;

/// Number of verified progress updates before survival release
const PROGRESS_STREAM_COUNT: u32 = 5;

/// 6 months in seconds (approx 26 weeks)
const SIX_MONTHS_SECS: u64 = 60 * 60 * 24 * 7 * 26;

/// Maximum trees per batch deposit (Stellar operation limit safety margin)
const MAX_BATCH_SIZE: u32 = 50;

// ── Types ─────────────────────────────────────────────────────────────────────
/// 1 year in seconds (approx 52 weeks)
const ONE_YEAR_SECS: u64 = 60 * 60 * 24 * 7 * 52;
/// Window in which a sponsor may challenge a verification outcome (#469)
const DISPUTE_WINDOW_SECS: u64 = 60 * 60 * 24 * 7;

/// 14 days in seconds — unaccepted jobs expire after this window (Closes #517)
const JOB_EXPIRY_SECS: u64 = 60 * 60 * 24 * 14;

/// 1 year in seconds
const ONE_YEAR_SECS: u64 = 60 * 60 * 24 * 365;

/// 90 days: planting must be confirmed before admin may transition Pending → Failed.
const PLANTING_TIMEOUT_SECS: u64 = 60 * 60 * 24 * 90;

/// Maximum slots per batch deposit (Stellar operation limit safety margin)
const MAX_BATCH_SIZE: u32 = 50;

/// Maximum slots per corporate bulk batch — closes #487
const CORP_BATCH_SIZE: u32 = 500;

/// Default minimum planting density (trees per hectare) for large jobs
const DEFAULT_MIN_DENSITY: i128 = 1_000;

/// Default job size threshold (in hectares) above which density rules apply
const DEFAULT_JOB_SIZE_THRESHOLD: i128 = 10;

// ── Types ─────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum EscrowStatus {
    Funded,
    Planted,
    Survived,
    Completed,
    Refunded,
    /// Sponsor refunded because no planter accepted within 14 days (Closes #517)
    JobExpired,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowRecord {
    pub donor: Address,
    pub gift_recipient: Option<Address>,
    pub farmer: Address,
    pub token: Address,
    pub total_amount: i128,
    pub tree_count: i128,
    pub area_hectares: i128,
    pub verified_tree_count: i128,
    pub tree_tokens_minted: i128,
    pub released: i128,
    pub progress_updates: u32,
    pub status: EscrowStatus,
    pub planted_at: u64,
    pub planting_proof: BytesN<32>,
    pub survival_proof: BytesN<32>,
    pub survival_rate_percent: u32,
    pub year_proof: BytesN<32>,
    /// Ledger timestamp after which the job can be expired if still unaccepted (Closes #517)
    pub expiry_deadline: u64,
}

/// A single slot in a batch deposit: one farmer address and the amount for that tree.
#[contracttype]
#[derive(Clone, Debug)]
pub struct BatchSlot {
    pub farmer: Address,
    pub amount: i128,
    pub gift_recipient: Option<Address>,
    pub referrer: Option<Address>,
}

/// Oracle-submitted survival report for a single tree.
#[contracttype]
#[derive(Clone, Debug)]
pub struct OracleReport {
    pub tree_id: u64,
    pub survival_rate_percent: u32,
    pub reported_at: u64,
    pub oracle: Address,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum TreeFundingStatus {
    Open,
    Released,
    Refunded,
}

/// Physical lifecycle state of a co-funded tree.
/// Distinct from `TreeFundingStatus` which tracks payment state.
///
/// Valid transitions:
///   Pending  → Planted   admin confirms physical planting
///   Pending  → Failed    admin marks timeout; only after PLANTING_TIMEOUT_SECS
///   Planted  → Verified  admin confirms survival milestone
///
/// Verified and Failed are terminal — no further transitions allowed.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum TreeStatus {
    Pending,
    Planted,
    Verified,
    Failed,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Contribution {
    pub funder: Address,
    pub amount: i128,
}

/// Outcome of a sponsor-initiated verification dispute (#469).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum DisputeOutcome {
    /// DAO upheld the verification — pending fund release may proceed.
    VerificationUpheld,
    /// DAO overturned the verification — funds remain locked for refund.
    VerificationOverturned,
}

/// Sponsor dispute record keyed by tree_id.
#[contracttype]
#[derive(Clone, Debug)]
pub struct DisputeRecord {
    pub tree_id: u64,
    pub sponsor: Address,
    /// Content-address hash of off-chain evidence (IPFS CID digest).
    pub evidence_cid: BytesN<32>,
    pub opened_at: u64,
    pub resolved: bool,
    pub outcome: DisputeOutcome,
    /// DAO votes that the verification should stand.
    pub votes_uphold: u32,
    /// DAO votes that the verification should be overturned.
    pub votes_overturn: u32,
}

/// Co-funded tree escrow record: multiple contributors share a single pool
/// with proportional payouts on release.
#[contracttype]
#[derive(Clone, Debug)]
pub struct TreeFunding {
    pub tree_id: u64,
    pub farmer: Address,
    pub token: Address,
    pub contributions: Vec<Contribution>,
    pub total_funded: i128,
    pub released: i128,
    pub status: TreeFundingStatus,
    pub tree_status: TreeStatus,
    pub registered_at: u64,
    pub planted_at: u64,
    pub verified_at: u64,
}

/// Sponsor rating for a planter (1-5 stars)
#[contracttype]
#[derive(Clone, Debug)]
pub struct PlanterRating {
    pub sponsor: Address,
    pub farmer: Address,
    pub rating: u32, // 1-5 stars
    pub rated_at: u64,
}

/// Aggregated reputation score for a planter
#[contracttype]
#[derive(Clone, Debug)]
pub struct PlanterReputation {
    pub farmer: Address,
    pub total_ratings: u32,
    pub sum_ratings: u128,
    pub average_rating: u32,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum PayoutType {
    Tranche2,
    Tranche3,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Payout {
    pub planter: Address,
    pub amount: i128,
    pub payout_type: PayoutType,
    pub timestamp: u64,
}

/// Aggregated on-chain receipt for a corporate bulk sponsorship — closes #487.
#[contracttype]
#[derive(Clone, Debug)]
pub struct CorpBatchRecord {
    pub batch_id: u64,
    pub sponsor: Address,
    pub token: Address,
    pub total_trees: i128,
    pub total_amount: i128,
    pub farmer_count: u32,
    pub created_at: u64,
}

/// Registered arbiter record — a trusted third party that can override
/// verification results and resolve locked disputes (#649).
#[contracttype]
#[derive(Clone, Debug)]
pub struct ArbiterRecord {
    pub arbiter: Address,
    pub registered_at: u64,
}

// ── Storage keys ──────────────────────────────────────────────────────────────

#[contracttype]
enum DataKey {
    AdminTree,
    Oracle,
    SurvivalThreshold,
    /// Minimum planting density (trees per hectare) for large jobs
    MinDensity,
    /// Job size threshold (hectares) above which density rules apply
    JobSizeThreshold,
    /// Per-farmer single-donor escrow record
    MinDensity,
    JobSizeThreshold,
    Paused,
    Escrow(Address),
    OracleReport(u64),
    TreeFunding(u64),
    /// Track used proof hashes for replay attack prevention (#481)
    UsedProof(BytesN<32>),
    /// Sponsor dispute on a verification outcome (#469)
    Dispute(u64),
    DaoMembers,
    /// Registered arbiter address (#649)
    Arbiter,
    SponsorRating(Address, Address),
    PlanterReputation(Address),
    PayoutHistory(Address),
    CorpBatchSeq,
    CorpBatch(u64),
}

/// A single slot in a batch deposit: one farmer address and the amount for that tree.
#[contracttype]
#[derive(Clone, Debug)]
pub struct BatchSlot {
    pub farmer: Address,
    pub amount: i128,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct TreeEscrow;

#[contractimpl]
impl TreeEscrow {
    /// One-time initialisation.
    ///
    /// * `admin` — controls planting verification, refunds, and tree registration
    /// * `tree_token` — TREE reward token; the contract must be its admin
    /// * `oracle` — the only address allowed to submit survival reports
    /// * `survival_threshold_percent` — minimum survival rate (0..=100) for Tranche 2 release
    /// * `min_density` — minimum trees per hectare for jobs above size threshold
    /// * `job_size_threshold` — minimum job size (hectares) for density rules to apply
    pub fn initialize(
        env: Env,
        admin: Address,
        tree_token: Address,
        oracle: Address,
        survival_threshold_percent: u32,
        min_density: i128,
        job_size_threshold: i128,
    ) {
        if env.storage().instance().has(&DataKey::AdminTree) {
            panic_with_error!(&env, HarvestaError::AlreadyInitialized);
        }
        if survival_threshold_percent > 100 {
            panic_with_error!(&env, HarvestaError::SurvivalThresholdOutOfRange);
        }
        if min_density <= 0 {
            panic!("min density must be positive");
        }
        if job_size_threshold <= 0 {
            panic!("job size threshold must be positive");
        }
        if min_density <= 0 {
            panic!("min density must be positive");
        }
        if job_size_threshold <= 0 {
            panic!("job size threshold must be positive");
        }
        if token::StellarAssetClient::new(&env, &tree_token).admin()
            != env.current_contract_address()
        {
            panic_with_error!(&env, HarvestaError::ContractMustBeTreeTokenAdmin);
        }

        let tree_decimals = token::Client::new(&env, &tree_token).decimals();

        env.storage()
            .instance()
            .set(&DataKey::AdminTree, &(admin, tree_token, tree_decimals));
        env.storage().instance().set(&DataKey::Oracle, &oracle);
        env.storage()
            .instance()
            .set(&DataKey::SurvivalThreshold, &survival_threshold_percent);
        env.storage()
            .instance()
            .set(&DataKey::MinDensity, &min_density);
        env.storage()
            .instance()
            .set(&DataKey::JobSizeThreshold, &job_size_threshold);
    }

    // ── Single-donor flow ─────────────────────────────────────────────────────

    /// Donor deposits `amount` of `token` into escrow for `farmer`.
    pub fn deposit(
        env: Env,
        donor: Address,
        farmer: Address,
        token: Address,
        amount: i128,
        tree_count: i128,
        area_hectares: i128,
    ) {
        Self::deposit_internal(env, donor, None, farmer, token, amount, tree_count, area_hectares);
    }

    /// Sponsor trees as a gift - NFT receipt and carbon credits go to a different recipient address.
    ///
    /// `recipient_wallet` - the address that will receive the TREE tokens (NFT receipt and carbon credits)
    /// `farmer` - the farmer to plant the trees
    /// `token` - the token to use for payment (XLM or USDC)
    /// `amount` - the total amount to deposit
    /// `tree_count` - the maximum number of trees covered by this donation
    /// `area_hectares` - planting area in hectares
    pub fn sponsor_as_gift(
        env: Env,
        donor: Address,
        recipient_wallet: Address,
        farmer: Address,
        token: Address,
        amount: i128,
        tree_count: i128,
        area_hectares: i128,
    ) {
        Self::deposit_internal(env, donor, Some(recipient_wallet), farmer, token, amount, tree_count, area_hectares);
    }

    fn deposit_internal(
        env: Env,
        donor: Address,
        gift_recipient: Option<Address>,
        farmer: Address,
        token: Address,
        amount: i128,
        tree_count: i128,
        area_hectares: i128,
    ) {
        donor.require_auth();

        if Self::is_paused(&env) {
            panic!("contract is paused - deposits are not allowed");
        }

        if amount <= 0 {
            panic_with_error!(&env, HarvestaError::AmountMustBePositive);
        }
        if tree_count <= 0 {
            panic_with_error!(&env, HarvestaError::TreeCountMustBePositive);
        }
        if area_hectares <= 0 {
            panic!("area hectares must be positive");
        }

        // Enforce minimum planting density for large jobs
        let job_size_threshold = Self::job_size_threshold(&env);
        if area_hectares >= job_size_threshold {
            let min_density = Self::min_density(&env);
            let actual_density = tree_count / area_hectares;
            if actual_density < min_density {
                panic!("planting density below minimum for job size");
            }
        }
        if area_hectares <= 0 {
            panic!("area hectares must be positive");
        }

        // Enforce minimum planting density for large jobs
        let job_size_threshold = Self::job_size_threshold(&env);
        if area_hectares >= job_size_threshold {
            let min_density = Self::min_density(&env);
            let actual_density = tree_count / area_hectares;
            if actual_density < min_density {
                panic!("planting density below minimum for job size");
            }
        }

        let key = DataKey::Escrow(farmer.clone());
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, HarvestaError::EscrowAlreadyExists);
        }

        contract_utils::assert_whitelisted(&env, &token);
        token::Client::new(&env, &token).transfer(&donor, &env.current_contract_address(), &amount);

        let empty_hash = BytesN::from_array(&env, &[0; 32]);
        env.storage().persistent().set(
            &key,
            &EscrowRecord {
                donor: donor.clone(),
                gift_recipient,
                farmer: farmer.clone(),
                token,
                total_amount: amount,
                tree_count,
                area_hectares,
                verified_tree_count: 0,
                tree_tokens_minted: 0,
                released: 0,
                progress_updates: 0,
                status: EscrowStatus::Funded,
                planted_at: 0,
                planting_proof: empty_hash.clone(),
                survival_proof: empty_hash.clone(),
                survival_rate_percent: 0,
                year_proof: empty_hash,
                expiry_deadline: env.ledger().timestamp() + JOB_EXPIRY_SECS,
            },
        );

        env.events()
            .publish((symbol_short!("deposit"), farmer), amount);
    }

    /// Batch deposit: donor funds N tree slots in a single contract invocation.
    ///
    /// Gas efficiency: one token transfer for the total, then N storage writes.
    /// Each slot maps to one farmer escrow record in the next planting cycle.
    ///
    /// Constraints:
    ///   - All slots must use the same token.
    ///   - No farmer in the batch may already have an active escrow.
    ///   - Batch size is capped at MAX_BATCH_SIZE (50) to stay within ledger limits.
    pub fn batch_deposit(
        env: Env,
        donor: Address,
        token: Address,
        slots: Vec<BatchSlot>,
    ) {
        donor.require_auth();

        let n = slots.len();
        if n == 0 {
            panic!("batch must contain at least one slot");
        }
        if n > MAX_BATCH_SIZE {
            panic!("batch exceeds maximum size of 50");
        }

        // Validate all slots and compute total in a single pass
    /// Each slot represents 1 tree on a small area (0.01 hectares for density calculation).
    pub fn batch_deposit(env: Env, donor: Address, token: Address, slots: Vec<BatchSlot>) {
        donor.require_auth();

        if Self::is_paused(&env) {
            panic!("contract is paused - deposits are not allowed");
        }

        let n = slots.len();
        if n == 0 {
            panic_with_error!(&env, HarvestaError::BatchEmpty);
        }
        if n > MAX_BATCH_SIZE {
            panic_with_error!(&env, HarvestaError::BatchTooLarge);
        }

        let mut total: i128 = 0;
        for i in 0..n {
            let slot = slots.get(i).unwrap();
            if slot.amount <= 0 {
                panic!("each slot amount must be positive");
            }
            let key = Self::record_key(&env, &slot.farmer);
            if env.storage().persistent().has(&key) {
                panic!("active escrow already exists for a farmer in this batch");
            }
            total += slot.amount;
        }

        // Single token transfer for the entire batch — gas-efficient
        token::Client::new(&env, &token)
            .transfer(&donor, &env.current_contract_address(), &total);

        // Write one escrow record per slot
        for i in 0..n {
            let slot = slots.get(i).unwrap();
            let key = Self::record_key(&env, &slot.farmer);
            env.storage().persistent().set(&key, &EscrowRecord {
                donor:          donor.clone(),
                farmer:         slot.farmer.clone(),
                token:          token.clone(),
                total_amount:   slot.amount,
                released:       0,
                status:         EscrowStatus::Funded,
                planted_at:     None,
                planting_proof: None,
                survival_proof: None,
            });
            env.events().publish((symbol_short!("deposit"), slot.farmer), slot.amount);
                panic_with_error!(&env, HarvestaError::SlotAmountMustBePositive);
            }
            let key = DataKey::Escrow(slot.farmer.clone());
            if env.storage().persistent().has(&key) {
                panic_with_error!(&env, HarvestaError::EscrowAlreadyExists);
            }
            total = total.checked_add(slot.amount).expect("batch total overflow");
        }

        contract_utils::assert_whitelisted(&env, &token);
        token::Client::new(&env, &token)
            .transfer(&donor, &env.current_contract_address(), &total);

        let zero_hash = BytesN::from_array(&env, &[0; 32]);
        for i in 0..n {
            let slot = slots.get(i).unwrap();
            let key = DataKey::Escrow(slot.farmer.clone());
            // Batch deposits use a fixed small area (0.01 hectares) per tree
            // This ensures batch deposits are exempt from density rules (below threshold)
            let batch_area_hectares = 1_i128 / 100; // 0.01 hectares per tree
            
            env.storage().persistent().set(
                &key,
                &EscrowRecord {
                    donor: donor.clone(),
                    gift_recipient: slot.gift_recipient.clone(),
                    farmer: slot.farmer.clone(),
                    token: token.clone(),
                    total_amount: slot.amount,
                    tree_count: 1,
                    area_hectares: batch_area_hectares,
                    area_hectares: 1_i128 / 100,
                    verified_tree_count: 0,
                    tree_tokens_minted: 0,
                    released: 0,
                    progress_updates: 0,
                    status: EscrowStatus::Funded,
                    planted_at: 0,
                    planting_proof: zero_hash.clone(),
                    survival_proof: zero_hash.clone(),
                    survival_rate_percent: 0,
                    year_proof: empty_hash,
                    year_proof: zero_hash.clone(),
                    expiry_deadline: env.ledger().timestamp() + JOB_EXPIRY_SECS,
                },
            );
            env.events()
                .publish((symbol_short!("deposit"), slot.farmer), slot.amount);
        }

        env.events().publish((symbol_short!("batch"), donor), total);
    }

    /// Verifier calls this after GPS + photo proof of planting is validated.
    /// Releases 75% of escrowed funds instantly to the farmer.
    /// Mints one TREE token to the donor for each verified tree.
    /// 
    /// OPTIMIZED: Reduced storage operations from 4 to 2 (1 read + 1 write)
    /// Admin-verified planting: releases Tranche 1 (30%) and mints TREE rewards.
    pub fn verify_planting(
    /// Admin-verified progress update: streams 10% of the escrow to the planter.
    ///
    /// May be called up to 5 times per escrow (each releasing exactly 10% of
    /// `total_amount`). The first call transitions the escrow from `Funded` to
    /// `Planted`, mints TREE rewards, and records the planting proof.
    pub fn verify_progress(
        env: Env,
        farmer: Address,
        proof_hash: BytesN<32>,
        verified_tree_count: i128,
    ) {
        let (admin, tree_token, tree_decimals) = Self::admin_tree(&env);
        admin.require_auth();

        let key = DataKey::Escrow(farmer.clone());
        let mut rec: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::EscrowNotFound));

        if rec.status == EscrowStatus::Completed || rec.status == EscrowStatus::Refunded {
            panic!("escrow not active");
        }
        if rec.progress_updates >= PROGRESS_STREAM_COUNT {
            panic!("all progress updates completed");
        }
        if verified_tree_count > rec.tree_count {
            panic_with_error!(&env, HarvestaError::VerifiedCountExceedsDonation);
        }

        // Replay attack prevention (#481): reject duplicate proof hashes
        let proof_key = DataKey::UsedProof(proof_hash.clone());
        if env.storage().persistent().has(&proof_key) {
            panic!("proof hash already used: replay attack prevented");
        }

        let tranche1 = (rec.total_amount * TRANCHE_1_BPS) / BPS_DENOM;
        let tranche1 = rec
            .total_amount
            .checked_mul(TRANCHE_1_BPS)
            .expect("tranche1 calculation overflow")
            .checked_div(BPS_DENOM)
            .expect("tranche1 division error");
        let tree_unit = Self::compute_token_unit(tree_decimals);
        let tree_tokens = verified_tree_count
            .checked_mul(tree_unit)
            .expect("tree token mint amount overflow");

        // First call: transition Funded → Planted and mint TREE rewards.
        if rec.status == EscrowStatus::Funded {
            let recipient = rec.gift_recipient.clone().unwrap_or_else(|| rec.donor.clone());
            token::StellarAssetClient::new(&env, &tree_token).mint(&recipient, &tree_tokens);
            rec.status = EscrowStatus::Planted;
            rec.planted_at = env.ledger().timestamp();
            rec.planting_proof = proof_hash;
            rec.verified_tree_count = verified_tree_count;
            rec.tree_tokens_minted = tree_tokens;
            env.events().publish((symbol_short!("treemint"), recipient), tree_tokens);
        }

        // Stream 10% of total_amount to the planter.
        let stream_amount = (rec.total_amount * STREAM_BPS) / BPS_DENOM;
        rec.released = rec.released.checked_add(stream_amount).expect("released amount overflow");
        rec.progress_updates += 1;

        // CEI: mark proof used and persist before external token transfer.
        env.storage().persistent().set(&proof_key, &true);
        env.storage().persistent().set(&key, &rec);

        token::Client::new(&env, &rec.token).transfer(
            &env.current_contract_address(),
            &rec.farmer,
            &stream_amount,
        );

        env.events()
            .publish((symbol_short!("progress"), farmer), (rec.progress_updates, stream_amount));
    }

    /// Admin-verified survival check: releases Tranche 2 (40%) once 6 months
    /// have elapsed and the reported survival rate ≥ the configured threshold.
    /// Admin-verified survival check: releases the remaining escrow once 6 months
    /// have elapsed, all progress updates are completed, and the reported
    /// survival rate ≥ the configured threshold.
    pub fn verify_survival(
        env: Env,
        farmer: Address,
        proof_hash: BytesN<32>,
        survival_rate_percent: u32,
    ) {
        let (admin, _tree_token, _decimals) = Self::admin_tree(&env);
        admin.require_auth();

        if survival_rate_percent > 100 {
            panic_with_error!(&env, HarvestaError::SurvivalRateOutOfRange);
        }

        let key = DataKey::Escrow(farmer.clone());
        let mut rec: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::EscrowNotFound));

        if rec.status != EscrowStatus::Planted {
            panic_with_error!(&env, HarvestaError::PlantingNotVerified);
        }
        if rec.progress_updates < PROGRESS_STREAM_COUNT {
            panic!("all progress updates must be completed first");
        }

        let now = env.ledger().timestamp();
        if now < rec.planted_at + SIX_MONTHS_SECS {
            panic_with_error!(&env, HarvestaError::SurvivalPeriodNotElapsed);
        }

        let threshold = Self::survival_threshold(&env);
        if survival_rate_percent < threshold {
            panic_with_error!(&env, HarvestaError::SurvivalRateBelowMinimum);
        }

        // Replay attack prevention (#481): reject duplicate proof hashes
        let proof_key = DataKey::UsedProof(proof_hash.clone());
        if env.storage().persistent().has(&proof_key) {
            panic!("proof hash already used: replay attack prevented");
        }

        let tranche2 = rec.total_amount - rec.released;
        let tranche2 = (rec.total_amount * TRANCHE_2_BPS) / BPS_DENOM;
        if tranche2 <= 0 {
            panic_with_error!(&env, HarvestaError::NothingToRelease);
        }

        token::Client::new(&env, &rec.token).transfer(
            &env.current_contract_address(),
            &rec.farmer,
            &tranche2,
        );

        // Record the Tranche2 payout for the farmer
        Self::record_payout(&env, rec.farmer.clone(), tranche2, PayoutType::Tranche2);

        rec.released += tranche2;
        rec.status = EscrowStatus::Completed;
        rec.survival_proof = proof_hash.clone();
        rec.status = EscrowStatus::Survived;
        rec.survival_proof = proof_hash;
        rec.survival_rate_percent = survival_rate_percent;
        env.storage().persistent().set(&proof_key, &true);

        env.storage().persistent().set(&key, &rec);

        env.events()
            .publish((symbol_short!("survived"), farmer), tranche2);
    }

    /// Admin-verified 1-year milestone: releases Tranche 3 (30%) once 1 year
    /// has elapsed since planting.
    pub fn verify_year_milestone(
        env: Env,
        farmer: Address,
        proof_hash: BytesN<32>,
    ) {
        let (admin, _tree_token, _decimals) = Self::admin_tree(&env);
        admin.require_auth();

        let key = DataKey::Escrow(farmer.clone());
        let mut rec: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .expect("no escrow for farmer");

        if rec.status != EscrowStatus::Survived {
            panic!("survival not yet verified");
        }

        let now = env.ledger().timestamp();
        if now < rec.planted_at + ONE_YEAR_SECS {
            panic!("1-year milestone period not yet elapsed");
        }

        let tranche3 = rec.total_amount - rec.released;
        if tranche3 <= 0 {
            panic!("nothing left to release");
        }

        token::Client::new(&env, &rec.token).transfer(
            &env.current_contract_address(),
            &rec.farmer,
            &tranche3,
        );

        rec.released += tranche3;
        rec.status = EscrowStatus::Completed;
        rec.year_proof = proof_hash;

        env.storage().persistent().set(&key, &rec);

        env.events()
            .publish((symbol_short!("year_milestone"), farmer), tranche3);
    }

    pub fn refund(env: Env, farmer: Address) {
        let (admin, _tree_token, _decimals) = Self::admin_tree(&env);
        admin.require_auth();

        let key = DataKey::Escrow(farmer.clone());
        let mut rec: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .expect("no escrow for farmer");

        if rec.status != EscrowStatus::Survived {
            panic!("survival not yet verified");
        }

        let now = env.ledger().timestamp();
        if now < rec.planted_at + ONE_YEAR_SECS {
            panic!("1-year milestone period not yet elapsed");
        }

        let tranche3 = rec.total_amount - rec.released;
        if tranche3 <= 0 {
            panic!("nothing left to release");
        }

        token::Client::new(&env, &rec.token).transfer(
            &env.current_contract_address(),
            &rec.farmer,
            &tranche3,
        );

        rec.released += tranche3;
        rec.status = EscrowStatus::Completed;
        rec.year_proof = proof_hash;

        env.storage().persistent().set(&key, &rec);

        env.events()
            .publish((symbol_short!("yearmile"), farmer), tranche3);
    }

    pub fn refund(env: Env, farmer: Address) {
        let (admin, _tree_token, _decimals) = Self::admin_tree(&env);
        admin.require_auth();

        let key = DataKey::Escrow(farmer.clone());
        let mut rec: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::EscrowNotFound));

        if rec.status != EscrowStatus::Funded {
            panic_with_error!(&env, HarvestaError::RefundAfterPlanting);
        }

        token::Client::new(&env, &rec.token).transfer(
            &env.current_contract_address(),
            &rec.donor,
            &rec.total_amount,
        );

        rec.status = EscrowStatus::Refunded;
        env.storage().persistent().set(&key, &rec);

        env.events()
            .publish((symbol_short!("refund"), farmer), rec.total_amount);
    }

    /// Expires a job that has never been accepted by a planter.
    ///
    /// Any caller may trigger expiry once `expiry_deadline` has passed and the
    /// escrow is still in `Funded` status (i.e. no planter has started work).
    /// The full deposit is returned to the sponsor and a `JobExpired` event is
    /// emitted. Closes #517.
    pub fn expire_job(env: Env, farmer: Address) {
        let key = DataKey::Escrow(farmer.clone());
        let mut rec: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .expect("no escrow for farmer");

        if rec.status != EscrowStatus::Funded {
            panic!("job cannot be expired: planting already started or job already closed");
        }

        let now = env.ledger().timestamp();
        if now < rec.expiry_deadline {
            panic!("expiry deadline has not yet passed");
        }

        token::Client::new(&env, &rec.token).transfer(
            &env.current_contract_address(),
            &rec.donor,
            &rec.total_amount,
        );

        rec.status = EscrowStatus::JobExpired;
        env.storage().persistent().set(&key, &rec);

        env.events()
            .publish((symbol_short!("jobexpir"), farmer), rec.total_amount);
    }

    pub fn get_record(env: Env, farmer: Address) -> Option<EscrowRecord> {
        env.storage().persistent().get(&DataKey::Escrow(farmer))
    }

    // ── Planter rating system (#483) ─────────────────────────────────────────

    /// Sponsor rates a planter after job completion. Rating must be 1-5 stars.
    /// Only callable by the original donor after escrow is completed.
    pub fn rate_planter(
        env: Env,
        sponsor: Address,
        farmer: Address,
        rating: u32,
    ) {
        sponsor.require_auth();

        if rating < 1 || rating > 5 {
            panic!("rating must be between 1 and 5");
        }

        // Check if escrow exists and is completed
        let escrow_key = DataKey::Escrow(farmer.clone());
        let rec: EscrowRecord = env
            .storage()
            .persistent()
            .get(&escrow_key)
            .expect("no escrow for farmer");

        if rec.donor != sponsor {
            panic!("only the original donor can rate the planter");
        }

        if rec.status != EscrowStatus::Completed {
            panic!("can only rate after escrow is completed");
        }

        // Prevent duplicate ratings from same sponsor
        let rating_key = DataKey::SponsorRating(sponsor.clone(), farmer.clone());
        if env.storage().persistent().has(&rating_key) {
            panic!("sponsor has already rated this planter");
        }

        // Store the individual rating
        let rating_record = PlanterRating {
            sponsor: sponsor.clone(),
            farmer: farmer.clone(),
            rating,
            rated_at: env.ledger().timestamp(),
        };
        env.storage().persistent().set(&rating_key, &rating_record);

        // Update aggregated reputation
        let rep_key = DataKey::PlanterReputation(farmer.clone());
        let mut rep: PlanterReputation = env
            .storage()
            .persistent()
            .get(&rep_key)
            .unwrap_or(PlanterReputation {
                farmer: farmer.clone(),
                total_ratings: 0,
                sum_ratings: 0,
                average_rating: 0,
            });

        rep.total_ratings += 1;
        rep.sum_ratings += rating as u128;
        rep.average_rating = ((rep.sum_ratings * 20) / rep.total_ratings as u128) as u32; // Scale to 0-100 (5 stars * 20 = 100)

        env.storage().persistent().set(&rep_key, &rep);

        env.events()
            .publish((symbol_short!("rated"), farmer), (sponsor, rating));
    }

    pub fn get_planter_reputation(env: Env, farmer: Address) -> Option<PlanterReputation> {
        env.storage().persistent().get(&DataKey::PlanterReputation(farmer))
    }

    // ── Oracle survival reports (#394) ────────────────────────────────────────

    /// Oracle-submitted on-chain attestation of a tree's survival rate.
    /// Overwrites any prior report for the same tree (latest wins).
    pub fn submit_survival_report(
        env: Env,
        oracle: Address,
        tree_id: u64,
        survival_rate_percent: u32,
    ) {
        let registered_oracle: Address = env
            .storage()
            .instance()
            .get(&DataKey::Oracle)
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::NotInitialized));

        if oracle != registered_oracle {
            panic_with_error!(&env, HarvestaError::UnauthorizedOracle);
        }
        oracle.require_auth();

        if survival_rate_percent > 100 {
            panic_with_error!(&env, HarvestaError::SurvivalRateOutOfRange);
        }

        let report = OracleReport {
            tree_id,
            survival_rate_percent,
            reported_at: env.ledger().timestamp(),
            oracle: oracle.clone(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::OracleReport(tree_id), &report);

        env.events().publish(
            (symbol_short!("oraclerp"), oracle),
            (tree_id, survival_rate_percent),
        );
    }

    pub fn get_oracle_report(env: Env, tree_id: u64) -> Option<OracleReport> {
        env.storage()
            .persistent()
            .get(&DataKey::OracleReport(tree_id))
    }

    pub fn get_survival_threshold(env: Env) -> u32 {
        Self::survival_threshold(&env)
    }

    // ── Co-funded flow (#402) ─────────────────────────────────────────────────

    /// Admin opens a tree as co-fundable. Sets the farmer payout address and
    /// the funding token. After registration, anyone may `contribute`.
    pub fn register_tree(env: Env, tree_id: u64, farmer: Address, token: Address) {
        let (admin, _tree_token, _decimals) = Self::admin_tree(&env);
        admin.require_auth();
        contract_utils::assert_whitelisted(&env, &token);

        let key = DataKey::TreeFunding(tree_id);
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, HarvestaError::TreeAlreadyRegistered);
        }

        let funding = TreeFunding {
            tree_id,
            farmer,
            token,
            contributions: Vec::new(&env),
            total_funded: 0,
            released: 0,
            status: TreeFundingStatus::Open,
            tree_status: TreeStatus::Pending,
            registered_at: env.ledger().timestamp(),
            planted_at: 0,
            verified_at: 0,
        };
        env.storage().persistent().set(&key, &funding);

        env.events()
            .publish((symbol_short!("treereg"), tree_id), funding.farmer);
    }

    /// A funder contributes `amount` to the pool for `tree_id`. If the funder
    /// already has a contribution, their share is added to (not overwritten).
    pub fn contribute(env: Env, funder: Address, tree_id: u64, amount: i128) {
        funder.require_auth();

        if Self::is_paused(&env) {
            panic!("contract is paused - contributions are not allowed");
        }

        if amount <= 0 {
            panic_with_error!(&env, HarvestaError::AmountMustBePositive);
        }

        let key = DataKey::TreeFunding(tree_id);
        let mut funding: TreeFunding = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::TreeNotRegistered));

        if funding.status != TreeFundingStatus::Open {
            panic_with_error!(&env, HarvestaError::TreeNotOpenForContributions);
        }

        token::Client::new(&env, &funding.token).transfer(
            &funder,
            &env.current_contract_address(),
            &amount,
        );

        // Merge with existing contribution from this funder, if any.
        let n = funding.contributions.len();
        let mut found = false;
        for i in 0..n {
            let mut c = funding.contributions.get(i).unwrap();
            if c.funder == funder {
                c.amount = c.amount.checked_add(amount).expect("contribution amount overflow");
                funding.contributions.set(i, c);
                found = true;
                break;
            }
        }
        if !found {
            funding.contributions.push_back(Contribution {
                funder: funder.clone(),
                amount,
            });
        }

        funding.total_funded = funding.total_funded.checked_add(amount).expect("total funded overflow");
        env.storage().persistent().set(&key, &funding);

        env.events()
            .publish((symbol_short!("cofunded"), tree_id), (funder, amount));
    }

    /// Pays out `payout_amount` from the pool, splitting it proportionally
    /// across each contributor by their share of `total_funded`.
    pub fn release_proportional(env: Env, tree_id: u64, payout_amount: i128) {
        let (admin, _tree_token, _decimals) = Self::admin_tree(&env);
        admin.require_auth();

        let report: OracleReport = env
            .storage()
            .persistent()
            .get(&DataKey::OracleReport(tree_id))
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::NoOracleReport));

        let threshold = Self::survival_threshold(&env);
        if report.survival_rate_percent < threshold {
            panic_with_error!(&env, HarvestaError::SurvivalRateBelowMinimum);
        }

        let key = DataKey::TreeFunding(tree_id);
        let mut funding: TreeFunding = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::TreeNotRegistered));

        if funding.status != TreeFundingStatus::Open {
            panic_with_error!(&env, HarvestaError::TreeNotOpenForRelease);
        }
        if Self::dispute_is_open(&env, tree_id) {
            panic!("fund release paused: dispute is open");
        }
        if funding.total_funded <= 0 {
            panic_with_error!(&env, HarvestaError::NoFundsToRelease);
        }
        let remaining = funding.total_funded.checked_sub(funding.released).expect("remaining calculation underflow");
        if payout_amount <= 0 || payout_amount > remaining {
            panic_with_error!(&env, HarvestaError::InvalidPayoutAmount);
        }

        let token_client = token::Client::new(&env, &funding.token);
        let n = funding.contributions.len();

        // Identify the largest contributor (earliest-recorded wins ties).
        let mut largest_idx: u32 = 0;
        let mut largest_amount: i128 = 0;
        for i in 0..n {
            let c = funding.contributions.get(i).unwrap();
            if c.amount > largest_amount {
                largest_amount = c.amount;
                largest_idx = i;
            }
        }

        let mut paid_so_far: i128 = 0;
        for i in 0..n {
            if i == largest_idx {
                continue;
            }
            let c = funding.contributions.get(i).unwrap();
            let payout = (c.amount.checked_mul(payout_amount).expect("payout calculation overflow")).checked_div(funding.total_funded).expect("payout division error");
            if payout > 0 {
                token_client.transfer(
                    &env.current_contract_address(),
                    &c.funder,
                    &payout,
                );
            }
            paid_so_far = paid_so_far.checked_add(payout).expect("paid_so_far overflow");
            env.events()
                .publish((symbol_short!("propayout"), tree_id), (c.funder, payout));
        }

        let largest = funding.contributions.get(largest_idx).unwrap();
        let largest_payout = payout_amount.checked_sub(paid_so_far).expect("largest payout calculation underflow");
        if largest_payout > 0 {
            token_client.transfer(
                &env.current_contract_address(),
                &largest.funder,
                &largest_payout,
            );
        }
        env.events().publish(
            (symbol_short!("propayout"), tree_id),
            (largest.funder, largest_payout),
        );

        funding.released = funding.released.checked_add(payout_amount).expect("released amount overflow");
        if funding.released >= funding.total_funded {
            funding.status = TreeFundingStatus::Released;
        }
        env.storage().persistent().set(&key, &funding);
    }

    pub fn get_tree_funding(env: Env, tree_id: u64) -> Option<TreeFunding> {
        env.storage()
            .persistent()
            .get(&DataKey::TreeFunding(tree_id))
    }

    // ── Tree lifecycle state machine (#462) ───────────────────────────────────

    /// Admin transitions a co-funded tree through its physical lifecycle states.
    ///
    ///   Pending  → Planted   admin confirms physical planting
    ///   Pending  → Failed    only after PLANTING_TIMEOUT_SECS (90 days)
    ///   Planted  → Verified  admin confirms survival milestone
    ///
    /// Verified and Failed are terminal.
    pub fn update_status(env: Env, tree_id: u64, new_status: TreeStatus) {
        let (admin, _, _) = Self::admin_tree(&env);
        admin.require_auth();

        let key = DataKey::TreeFunding(tree_id);
        let mut funding: TreeFunding = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::TreeNotRegistered));

        let now = env.ledger().timestamp();

        match (&funding.tree_status, &new_status) {
            (TreeStatus::Pending, TreeStatus::Planted) => {
                funding.planted_at = now;
            }
            (TreeStatus::Pending, TreeStatus::Failed) => {
                if now < funding.registered_at + PLANTING_TIMEOUT_SECS {
                    panic_with_error!(&env, HarvestaError::PlantingTimeoutNotReached);
                }
            }
            (TreeStatus::Planted, TreeStatus::Verified) => {
                funding.verified_at = now;
            }
            _ => {
                panic_with_error!(&env, HarvestaError::InvalidTreeStatusTransition);
            }
        }

        funding.tree_status = new_status.clone();
        env.storage().persistent().set(&key, &funding);

        env.events().publish(
            (symbol_short!("statchg"), tree_id),
            (new_status, now),
        );
    }

    /// Returns the current physical lifecycle status of a co-funded tree.
    pub fn get_tree_status(env: Env, tree_id: u64) -> TreeStatus {
        let key = DataKey::TreeFunding(tree_id);
        let funding: TreeFunding = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::TreeNotRegistered));
        funding.tree_status
    }

    // ── Dispute resolution (#469) ─────────────────────────────────────────────

    /// Admin configures the DAO member set that may vote on verification disputes.
    pub fn set_dao_members(env: Env, members: soroban_sdk::Vec<Address>) {
        let (admin, _tree_token, _decimals) = Self::admin_tree(&env);
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::DaoMembers, &members);
    }

    pub fn get_dao_members(env: Env) -> soroban_sdk::Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::SurvivalThreshold)
            .expect("contract not initialized")
    }


    fn min_density(env: &Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::MinDensity)
            .expect("contract not initialized")
    }

    fn job_size_threshold(env: &Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::JobSizeThreshold)
            .expect("contract not initialized")
    }

    fn compute_token_unit(decimals: u32) -> i128 {
        let mut unit = 1i128;
        let mut i = 0u32;
        while i < decimals {
            unit = unit.checked_mul(10).expect("token unit overflow");
            i += 1;
        }
        unit
            .get(&DataKey::DaoMembers)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env))
    }

    /// Sponsor opens a dispute within 7 days of an oracle verification report.
    /// Pauses any pending proportional fund release for the tree.
    pub fn open_dispute(
        env: Env,
        sponsor: Address,
        tree_id: u64,
        evidence_cid: BytesN<32>,
    ) {
        sponsor.require_auth();

        let report: OracleReport = env
            .storage()
            .persistent()
            .get(&DataKey::OracleReport(tree_id))
            .expect("no verification report for tree");

        let now = env.ledger().timestamp();
        if now > report.reported_at + DISPUTE_WINDOW_SECS {
            panic!("dispute window expired");
        }

        let dispute_key = DataKey::Dispute(tree_id);
        if let Some(existing) = env.storage().persistent().get::<_, DisputeRecord>(&dispute_key) {
            if !existing.resolved {
                panic!("dispute already open for tree");
            }
        }

        let funding: TreeFunding = env
            .storage()
            .persistent()
            .get(&DataKey::TreeFunding(tree_id))
            .expect("tree not registered");

        if !Self::is_tree_contributor(&funding, &sponsor) {
            panic!("only a tree contributor may open a dispute");
        }

        let dispute = DisputeRecord {
            tree_id,
            sponsor: sponsor.clone(),
            evidence_cid,
            opened_at: now,
            resolved: false,
            outcome: DisputeOutcome::VerificationUpheld,
            votes_uphold: 0,
            votes_overturn: 0,
        };

        env.storage().persistent().set(&dispute_key, &dispute);

        env.events().publish(
            (symbol_short!("DispOpen"), tree_id),
            sponsor,
        );
    }

    /// DAO member casts an arbitration vote on an open dispute.
    pub fn cast_dao_vote(env: Env, voter: Address, tree_id: u64, uphold_verification: bool) {
        voter.require_auth();

        if !Self::is_dao_member(&env, &voter) {
            panic!("caller is not a DAO member");
        }

        let dispute_key = DataKey::Dispute(tree_id);
        let mut dispute: DisputeRecord = env
            .storage()
            .persistent()
            .get(&dispute_key)
            .expect("no dispute for tree");

        if dispute.resolved {
            panic!("dispute already resolved");
        }

        if uphold_verification {
            dispute.votes_uphold += 1;
        } else {
            dispute.votes_overturn += 1;
        }

        env.storage().persistent().set(&dispute_key, &dispute);

        env.events().publish(
            (symbol_short!("DaoVote"), tree_id),
            (voter, uphold_verification),
        );
    }

    /// Tally DAO votes and resolve the dispute. Emits `DisputeResolved`.
    pub fn resolve_dispute(env: Env, resolver: Address, tree_id: u64) {
        let (admin, _tree_token, _decimals) = Self::admin_tree(&env);
        if resolver != admin && !Self::is_dao_member(&env, &resolver) {
            panic!("unauthorized resolver");
        }
        resolver.require_auth();

        let dispute_key = DataKey::Dispute(tree_id);
        let mut dispute: DisputeRecord = env
            .storage()
            .persistent()
            .get(&dispute_key)
            .expect("no dispute for tree");

        if dispute.resolved {
            panic!("dispute already resolved");
        }

        let total_votes = dispute.votes_uphold + dispute.votes_overturn;
        if total_votes == 0 {
            panic!("no DAO votes cast");
        }

        let outcome = if dispute.votes_overturn > dispute.votes_uphold {
            DisputeOutcome::VerificationOverturned
        } else {
            DisputeOutcome::VerificationUpheld
        };

        dispute.resolved = true;
        dispute.outcome = outcome.clone();
        env.storage().persistent().set(&dispute_key, &dispute);

        env.events().publish(
            (symbol_short!("DispResol"), tree_id),
            outcome,
        );
    }

    pub fn get_dispute(env: Env, tree_id: u64) -> Option<DisputeRecord> {
        env.storage().persistent().get(&DataKey::Dispute(tree_id))
    }

    pub fn has_open_dispute(env: Env, tree_id: u64) -> bool {
        Self::dispute_is_open(&env, tree_id)
    }

    // ── Arbiter dispute resolution (#649) ─────────────────────────────────────

    /// Admin registers a trusted third-party arbiter.
    /// Only one arbiter is active at a time; calling again replaces the previous one.
    pub fn register_arbiter(env: Env, arbiter: Address) {
        let (admin, _tree_token, _decimals) = Self::admin_tree(&env);
        admin.require_auth();

        let record = ArbiterRecord {
            arbiter: arbiter.clone(),
            registered_at: env.ledger().timestamp(),
        };
        env.storage().instance().set(&DataKey::Arbiter, &record);

        env.events()
            .publish((symbol_short!("arbReg"), arbiter), ());
    }

    /// Returns the currently registered arbiter record, if any.
    pub fn get_arbiter(env: Env) -> Option<ArbiterRecord> {
        env.storage().instance().get(&DataKey::Arbiter)
    }

    /// Arbiter overrides the verification result for a single-donor escrow,
    /// moving a locked/Planted escrow back to Funded so a refund can proceed,
    /// or forcing it to Completed so the remaining balance is released.
    ///
    /// * `tree_released` — `true` to release remaining funds to the farmer
    ///   (treats as completed); `false` to revert to Funded so the donor can
    ///   reclaim via `refund`.
    pub fn arbiter_override(env: Env, arbiter: Address, farmer: Address, tree_released: bool) {
        arbiter.require_auth();
        Self::assert_is_arbiter(&env, &arbiter);

        let key = DataKey::Escrow(farmer.clone());
        let mut rec: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::EscrowNotFound));

        if rec.status == EscrowStatus::Completed || rec.status == EscrowStatus::Refunded {
            panic!("escrow already finalised");
        }

        if tree_released {
            // Release remaining balance to farmer
            let remaining = rec.total_amount.checked_sub(rec.released).expect("underflow");
            if remaining > 0 {
                token::Client::new(&env, &rec.token).transfer(
                    &env.current_contract_address(),
                    &rec.farmer,
                    &remaining,
                );
                rec.released = rec.total_amount;
            }
            rec.status = EscrowStatus::Completed;
        } else {
            // Revert to Funded so donor may call refund
            rec.status = EscrowStatus::Funded;
        }

        env.storage().persistent().set(&key, &rec);

        env.events()
            .publish((symbol_short!("arbOvrd"), farmer), tree_released);
    }

    /// Arbiter resolves a co-funded tree dispute, bypassing the DAO vote
    /// requirement. Sets the dispute outcome directly and unblocks or
    /// permanently locks fund release for `tree_id`.
    ///
    /// * `uphold` — `true` to uphold the verification (release can proceed);
    ///   `false` to overturn it (funds remain locked for contributor refund).
    pub fn arbiter_resolve(env: Env, arbiter: Address, tree_id: u64, uphold: bool) {
        arbiter.require_auth();
        Self::assert_is_arbiter(&env, &arbiter);

        let dispute_key = DataKey::Dispute(tree_id);
        let mut dispute: DisputeRecord = env
            .storage()
            .persistent()
            .get(&dispute_key)
            .expect("no dispute for tree");

        if dispute.resolved {
            panic!("dispute already resolved");
        }

        let outcome = if uphold {
            DisputeOutcome::VerificationUpheld
        } else {
            DisputeOutcome::VerificationOverturned
        };

        dispute.resolved = true;
        dispute.outcome = outcome.clone();
        env.storage().persistent().set(&dispute_key, &dispute);

        env.events()
            .publish((symbol_short!("arbRes"), tree_id), outcome);
    }

    // ── Whitelist management ──────────────────────────────────────────────────

    /// Add `addr` to the contract whitelist. Restricted to admin.
    pub fn add_to_whitelist(env: Env, addr: Address) {
        let (admin, _tree_token, _decimals) = Self::admin_tree(&env);
        admin.require_auth();
        contract_utils::add_to_whitelist(&env, &addr);
    }

    /// Remove `addr` from the contract whitelist. Restricted to admin.
    pub fn remove_from_whitelist(env: Env, addr: Address) {
        let (admin, _tree_token, _decimals) = Self::admin_tree(&env);
        admin.require_auth();
        contract_utils::remove_from_whitelist(&env, &addr);
    }

    /// Returns `true` if `addr` is whitelisted.
    pub fn is_whitelisted(env: Env, addr: Address) -> bool {
        contract_utils::is_whitelisted(&env, &addr)
    }

    /// Panics if `addr` is not whitelisted.
    pub fn assert_whitelisted(env: Env, addr: Address) {
        contract_utils::assert_whitelisted(&env, &addr);
    }

    // ── internal helpers ──────────────────────────────────────────────────────

    fn admin_tree(env: &Env) -> (Address, Address, u32) {
        env.storage()
            .instance()
            .get(&DataKey::AdminTree)
            .unwrap_or_else(|| panic_with_error!(env, HarvestaError::NotInitialized))
    }

    fn survival_threshold(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::SurvivalThreshold)
            .unwrap_or_else(|| panic_with_error!(env, HarvestaError::NotInitialized))
    }

    fn dispute_is_open(env: &Env, tree_id: u64) -> bool {
        env.storage()
            .persistent()
            .get::<_, DisputeRecord>(&DataKey::Dispute(tree_id))
            .map(|d| !d.resolved)
            .unwrap_or(false)
    }

    fn is_dao_member(env: &Env, address: &Address) -> bool {
        let members: soroban_sdk::Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::DaoMembers)
            .unwrap_or_else(|| soroban_sdk::Vec::new(env));

        for i in 0..members.len() {
            if members.get(i).unwrap() == *address {
                return true;
            }
        }
        false
    }

    fn is_tree_contributor(funding: &TreeFunding, address: &Address) -> bool {
        for i in 0..funding.contributions.len() {
            if funding.contributions.get(i).unwrap().funder == *address {
                return true;
            }
        }
        false
    }

    fn assert_is_arbiter(env: &Env, address: &Address) {
        let record: ArbiterRecord = env
            .storage()
            .instance()
            .get(&DataKey::Arbiter)
            .unwrap_or_else(|| panic_with_error!(env, HarvestaError::NotArbiter));
        if record.arbiter != *address {
            panic_with_error!(env, HarvestaError::NotArbiter);
        }
    }

    fn is_paused(env: &Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    fn compute_token_unit(decimals: u32) -> i128 {
        let mut unit = 1i128;
        let mut i = 0u32;
        while i < decimals {
            unit = unit.checked_mul(10).expect("token unit overflow");
            i += 1;
        }
        unit
    }

    fn record_payout(env: &Env, planter: Address, amount: i128, payout_type: PayoutType) {
        let key = DataKey::PayoutHistory(planter.clone());
        let mut payouts: Vec<Payout> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(env));

        let payout = Payout {
            planter,
            amount,
            payout_type,
            timestamp: env.ledger().timestamp(),
        };

        payouts.push_back(payout);
        env.storage().persistent().set(&key, &payouts);
    }

    fn min_density(env: &Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::MinDensity)
            .expect("not initialized")
    }

    fn job_size_threshold(env: &Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::JobSizeThreshold)
            .expect("not initialized")
    }

    // ── Sponsor cancellation — closes #482 ───────────────────────────────────

    /// Sponsor cancels their tree order before any planter has accepted.
    /// Returns the full escrowed amount to the sponsor.
    /// Only valid while the escrow is in `Funded` status.
    pub fn sponsor_cancel(env: Env, sponsor: Address, farmer: Address) {
        sponsor.require_auth();

        let key = DataKey::Escrow(farmer.clone());
        let mut rec: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::EscrowNotFound));

        if rec.donor != sponsor {
            panic_with_error!(&env, HarvestaError::Unauthorized);
        }
        if rec.status != EscrowStatus::Funded {
            panic_with_error!(&env, HarvestaError::RefundAfterPlanting);
        }

        let refund_amount = rec.total_amount;
        let token = rec.token.clone();

        // CEI: mark as refunded and persist before external transfer.
        rec.status = EscrowStatus::Refunded;
        env.storage().persistent().set(&key, &rec);

        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &sponsor,
            &refund_amount,
        );

        env.events()
            .publish((symbol_short!("spcncl"), farmer), refund_amount);
    }

    // ── Corporate bulk sponsorship — closes #487 ─────────────────────────────

    /// Org wallet batch-sponsors up to 500 trees.
    ///
    /// Creates individual per-farmer escrow records (the normal payout flow)
    /// and a single `CorpBatchRecord` that acts as an aggregated on-chain
    /// receipt / carbon certificate for the sponsoring organisation.
    ///
    /// Returns the corporate batch ID.
    pub fn corporate_batch_deposit(
        env: Env,
        sponsor: Address,
        token: Address,
        slots: Vec<BatchSlot>,
    ) -> u64 {
        sponsor.require_auth();

        if Self::is_paused(&env) {
            panic!("contract is paused - deposits are not allowed");
        }

        let n = slots.len();
        if n == 0 {
            panic_with_error!(&env, HarvestaError::BatchEmpty);
        }
        if n > CORP_BATCH_SIZE {
            panic_with_error!(&env, HarvestaError::BatchTooLarge);
        }

        let mut total_amount: i128 = 0;
        let mut total_trees: i128 = 0;
        for i in 0..n {
            let slot = slots.get(i).unwrap();
            if slot.amount <= 0 {
                panic_with_error!(&env, HarvestaError::SlotAmountMustBePositive);
            }
            let key = DataKey::Escrow(slot.farmer.clone());
            if env.storage().persistent().has(&key) {
                panic_with_error!(&env, HarvestaError::EscrowAlreadyExists);
            }
            total_amount = total_amount.checked_add(slot.amount).expect("batch total overflow");
            total_trees = total_trees.checked_add(1).expect("tree count overflow");
        }

        contract_utils::assert_whitelisted(&env, &token);
        token::Client::new(&env, &token)
            .transfer(&sponsor, &env.current_contract_address(), &total_amount);

        let empty_hash = BytesN::from_array(&env, &[0; 32]);
        for i in 0..n {
            let slot = slots.get(i).unwrap();
            let key = DataKey::Escrow(slot.farmer.clone());
            env.storage().persistent().set(
                &key,
                &EscrowRecord {
                    donor: sponsor.clone(),
                    gift_recipient: slot.gift_recipient.clone(),
                    farmer: slot.farmer.clone(),
                    token: token.clone(),
                    total_amount: slot.amount,
                    tree_count: 1,
                    area_hectares: 1_i128 / 100,
                    verified_tree_count: 0,
                    tree_tokens_minted: 0,
                    released: 0,
                    progress_updates: 0,
                    status: EscrowStatus::Funded,
                    planted_at: 0,
                    planting_proof: empty_hash.clone(),
                    survival_proof: empty_hash.clone(),
                    survival_rate_percent: 0,
                    year_proof: empty_hash.clone(),
                    expiry_deadline: env.ledger().timestamp() + JOB_EXPIRY_SECS,
                },
            );
            env.events()
                .publish((symbol_short!("corpslot"), slot.farmer), slot.amount);
        }

        // Assign batch ID and persist the aggregated receipt.
        let batch_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::CorpBatchSeq)
            .unwrap_or(0u64)
            .checked_add(1)
            .expect("corp batch seq overflow");
        env.storage().instance().set(&DataKey::CorpBatchSeq, &batch_id);

        let receipt = CorpBatchRecord {
            batch_id,
            sponsor: sponsor.clone(),
            token,
            total_trees,
            total_amount,
            farmer_count: n,
            created_at: env.ledger().timestamp(),
        };
        env.storage().persistent().set(&DataKey::CorpBatch(batch_id), &receipt);

        env.events()
            .publish((symbol_short!("corpbatch"), sponsor), (batch_id, total_trees, total_amount));

        batch_id
    }

    /// Retrieve the corporate batch receipt by ID.
    pub fn get_corp_batch(env: Env, batch_id: u64) -> Option<CorpBatchRecord> {
        env.storage().persistent().get(&DataKey::CorpBatch(batch_id))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token, vec, Address, BytesN, Env,
    };

    const DEFAULT_THRESHOLD: u32 = 70;
    const DEFAULT_MIN_DENSITY: i128 = 1_000;
    const DEFAULT_JOB_SIZE_THRESHOLD: i128 = 10;

    #[allow(dead_code)]
    struct Ctx {
        env: Env,
        admin: Address,
        oracle: Address,
        donor: Address,
        farmer: Address,
        token: Address,
        tree_token: Address,
        client: TreeEscrowClient<'static>,
    }

    fn setup() -> Ctx {
        setup_with_threshold(DEFAULT_THRESHOLD)
    }

    fn setup_with_threshold(threshold: u32) -> Ctx {
        setup_with_density(threshold, DEFAULT_MIN_DENSITY, DEFAULT_JOB_SIZE_THRESHOLD)
    }

    fn setup_with_density(threshold: u32, min_density: i128, job_size_threshold: i128) -> Ctx {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, TreeEscrow);
        let client = TreeEscrowClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let oracle = Address::generate(&env);
        let donor = Address::generate(&env);
        let farmer = Address::generate(&env);

        let token_id = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        token::StellarAssetClient::new(&env, &token_id).mint(&donor, &10_000);

        let tree_token_id = env
            .register_stellar_asset_contract_v2(contract_id.clone())
            .address();

        client.initialize(&admin, &tree_token_id, &oracle, &threshold, &min_density, &job_size_threshold);
        client.add_to_whitelist(&tree_token_id);
        client.add_to_whitelist(&token_id);
        Ctx {
            env,
            admin,
            oracle,
            donor,
            farmer,
            token: token_id,
            tree_token: tree_token_id,
            client,
        }
    }

    fn proof(env: &Env, seed: u8) -> BytesN<32> {
        BytesN::from_array(env, &[seed; 32])
    }

    fn balance(env: &Env, token: &Address, who: &Address) -> i128 {
        token::Client::new(env, token).balance(who)
    }

    fn fund(env: &Env, token: &Address, who: &Address, amount: i128) {
        token::StellarAssetClient::new(env, token).mint(who, &amount);
    }

    fn setup_with_threshold(threshold: u32) -> Ctx {
        setup_with_density(threshold, DEFAULT_MIN_DENSITY, DEFAULT_JOB_SIZE_THRESHOLD)
    }

    fn setup_with_density(threshold: u32, min_density: i128, job_size_threshold: i128) -> Ctx {
    /// Complete all 5 progress updates for a farmer's escrow.
    fn complete_progress(ctx: &Ctx, farmer: &Address, verified_count: i128, seed_offset: u8) {
        for i in 0..5u8 {
            ctx.client.verify_progress(farmer, &proof(&ctx.env, seed_offset + i), &verified_count);
        }
    }

    // ── initialise ────────────────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "Error(Contract, #8)")]
    fn test_initialize_requires_contract_as_tree_token_admin() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, TreeEscrow);
        let client = TreeEscrowClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let oracle = Address::generate(&env);
        let tree_token_id = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();

        client.initialize(&admin, &tree_token_id, &oracle, &70, &DEFAULT_MIN_DENSITY, &DEFAULT_JOB_SIZE_THRESHOLD);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #21)")]
    fn test_initialize_rejects_threshold_above_100() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreeEscrow);
        let client = TreeEscrowClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let oracle = Address::generate(&env);
        let tree_token_id = env
            .register_stellar_asset_contract_v2(contract_id.clone())
            .address();
        client.initialize(&admin, &tree_token_id, &oracle, &101, &DEFAULT_MIN_DENSITY, &DEFAULT_JOB_SIZE_THRESHOLD);
    }

        client.initialize(&admin, &tree_token_id, &oracle, &threshold, &min_density, &job_size_threshold);
        client.initialize(&admin, &tree_token_id, &oracle, &threshold);
        client.add_to_whitelist(&tree_token_id);
        client.add_to_whitelist(&token_id);
        Ctx {
            env,
            admin,
            oracle,
            donor,
            farmer,
            token: token_id,
            tree_token: tree_token_id,
            client,
    // ── Single-donor lifecycle ────────────────────────────────────────────────

    #[test]
    fn test_full_lifecycle() {
        let ctx = setup();
        let stream_10pct = 1_000; // 10% of 10_000

        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Funded
        );

        // 5 progress updates, each streaming 10%
        for i in 0..5 {
            ctx.client
                .verify_progress(&ctx.farmer, &proof(&ctx.env, i as u8 + 1), &42);
            let rec = ctx.client.get_record(&ctx.farmer).unwrap();
            assert_eq!(rec.progress_updates, i + 1);
            assert_eq!(rec.released, stream_10pct * (i as i128 + 1));
        }

        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.status, EscrowStatus::Planted);
        assert_eq!(rec.released, 5_000); // 50% (5 × 10%)
        assert_eq!(rec.progress_updates, 5);
        assert_eq!(rec.tree_count, 42);
        assert_eq!(rec.verified_tree_count, 42);

        let tree_unit = 10i128.pow(token::Client::new(&ctx.env, &ctx.tree_token).decimals());
        assert_eq!(rec.tree_tokens_minted, 42 * tree_unit);
        assert_eq!(
            balance(&ctx.env, &ctx.tree_token, &ctx.donor),
            42 * tree_unit
        );

        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);

        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &70);
        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.status, EscrowStatus::Survived);
        assert_eq!(rec.released, 9_000); // 50% + 40% = 90%
        assert_eq!(rec.survival_rate_percent, 70);

        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);

        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 7));
        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.status, EscrowStatus::Completed);
        assert_eq!(rec.released, 10_000); // 100%
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #24)")]
    fn test_survival_too_early_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env.ledger().with_mut(|l| l.timestamp += 86_400);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &80);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #23)")]
    fn test_survival_below_threshold_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &69);
    }

    #[test]
    fn test_threshold_is_configurable_at_init() {
        let ctx = setup_with_threshold(50);
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &55);
        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.status, EscrowStatus::Survived);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 7));
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Completed
        );
    }

    #[test]
    #[should_panic(expected = "all progress updates completed")]
    fn test_extra_progress_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        // 6th call should be rejected
        ctx.client
            .verify_progress(&ctx.farmer, &proof(&ctx.env, 6), &42);
    }

    #[test]
    fn test_refund_before_planting() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client.refund(&ctx.farmer);
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Refunded
        );
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #20)")]
    fn test_refund_after_progress_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client
            .verify_progress(&ctx.farmer, &proof(&ctx.env, 1), &42);
        ctx.client.refund(&ctx.farmer);
    }

    // ── expire_job (#517) ─────────────────────────────────────────────────────

    #[test]
    fn test_expire_job_happy_path() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &5, &1);

        // Advance time past the 14-day deadline.
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += JOB_EXPIRY_SECS + 1);

        let pre_balance = balance(&ctx.env, &ctx.token, &ctx.donor);
        ctx.client.expire_job(&ctx.farmer);

        // Sponsor refunded in full.
        assert_eq!(
            balance(&ctx.env, &ctx.token, &ctx.donor) - pre_balance,
            10_000
        );
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::JobExpired
        );
    }

    #[test]
    #[should_panic(expected = "expiry deadline has not yet passed")]
    fn test_expire_job_too_early_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &5, &1);

        // Only 1 day has passed — well before the 14-day deadline.
        ctx.env.ledger().with_mut(|l| l.timestamp += 86_400);
        ctx.client.expire_job(&ctx.farmer);
    }

    #[test]
    #[should_panic(expected = "job cannot be expired: planting already started or job already closed")]
    fn test_expire_job_after_planting_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &5, &1);
        ctx.client
            .verify_progress(&ctx.farmer, &proof(&ctx.env, 1), &5);

        // Even if time has elapsed, a planted job cannot be expired.
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += JOB_EXPIRY_SECS + 1);
        ctx.client.expire_job(&ctx.farmer);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #10)")]
    fn test_deposit_rejects_zero_tree_count() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &0, &5);
    }

    // ── 1-year milestone tests (#494) ───────────────────────────────────────────

    #[test]
    #[should_panic(expected = "survival not yet verified")]
    fn test_year_milestone_before_survival_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 6));
    }

    #[test]
    #[should_panic(expected = "1-year milestone period not yet elapsed")]
    fn test_year_milestone_too_early_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &70);
        ctx.env.ledger().with_mut(|l| l.timestamp += 86_400);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 7));
    }

    #[test]
    fn test_year_milestone_at_one_year_accepted() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &70);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 7));
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Completed
        );
    }

    #[test]
    #[should_panic(expected = "survival not yet verified")]
    fn test_year_milestone_double_call_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &70);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 7));
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 8));
    }

    // ── Planter rating tests (#483) ───────────────────────────────────────────

    #[test]
    fn test_rate_planter_after_completion() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &70);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 7));

        ctx.client.rate_planter(&ctx.donor, &ctx.farmer, &5);

        let rep = ctx.client.get_planter_reputation(&ctx.farmer).unwrap();
        assert_eq!(rep.total_ratings, 1);
        assert_eq!(rep.sum_ratings, 5);
        assert_eq!(rep.average_rating, 100); // 5 * 20 = 100
    }

    #[test]
    #[should_panic(expected = "rating must be between 1 and 5")]
    fn test_rating_out_of_range_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &70);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 7));

        ctx.client.rate_planter(&ctx.donor, &ctx.farmer, &6);
    }

    #[test]
    #[should_panic(expected = "can only rate after escrow is completed")]
    fn test_rating_before_completion_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client.verify_progress(&ctx.farmer, &proof(&ctx.env, 1), &42);

        ctx.client.rate_planter(&ctx.donor, &ctx.farmer, &5);
    }

    #[test]
    #[should_panic(expected = "only the original donor can rate the planter")]
    fn test_non_donor_cannot_rate() {
        let ctx = setup();
        let impostor = Address::generate(&ctx.env);
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &70);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 7));

        client.initialize(&admin, &tree_token_id, &oracle, &70, &DEFAULT_MIN_DENSITY, &DEFAULT_JOB_SIZE_THRESHOLD);
    }

    #[test]
    #[should_panic(expected = "survival threshold must be 0..=100")]
    fn test_initialize_rejects_threshold_above_100() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, TreeEscrow);
        let client = TreeEscrowClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let oracle = Address::generate(&env);
        let tree_token_id = env
            .register_stellar_asset_contract_v2(contract_id.clone())
            .address();
        client.initialize(&admin, &tree_token_id, &oracle, &101, &DEFAULT_MIN_DENSITY, &DEFAULT_JOB_SIZE_THRESHOLD);
    }
        ctx.client.rate_planter(&impostor, &ctx.farmer, &5);
    }

    #[test]
    #[should_panic(expected = "sponsor has already rated this planter")]
    fn test_duplicate_rating_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &70);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 7));

        ctx.client.rate_planter(&ctx.donor, &ctx.farmer, &5);
        ctx.client.rate_planter(&ctx.donor, &ctx.farmer, &4);
    }

    #[test]
    fn test_multiple_sponsors_can_rate_same_planter() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &70);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 7));

        // First sponsor rates
        ctx.client.rate_planter(&ctx.donor, &ctx.farmer, &5);

        // Create a second escrow with different donor and a fresh farmer
        let donor2 = Address::generate(&ctx.env);
        let farmer2 = Address::generate(&ctx.env);
        token::StellarAssetClient::new(&ctx.env, &ctx.token).mint(&donor2, &10_000);
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Funded
        );
            .deposit(&donor2, &farmer2, &ctx.token, &10_000, &30, &3);
        complete_progress(&ctx, &farmer2, 30, 11);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&farmer2, &proof(&ctx.env, 20), &70);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&farmer2, &proof(&ctx.env, 21));

        // Second sponsor rates their own farmer
        ctx.client.rate_planter(&donor2, &farmer2, &4);

        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.status, EscrowStatus::Planted);
        assert_eq!(rec.released, 3_000); // 30% of 10,000
        assert_eq!(rec.released, 5_000);
        assert_eq!(rec.progress_updates, 5);
        assert_eq!(rec.tree_count, 42);
        assert_eq!(rec.verified_tree_count, 42);
        let rep1 = ctx.client.get_planter_reputation(&ctx.farmer).unwrap();
        assert_eq!(rep1.total_ratings, 1);
        assert_eq!(rep1.sum_ratings, 5);
        assert_eq!(rep1.average_rating, 100); // 5 * 20 = 100

        let rep2 = ctx.client.get_planter_reputation(&farmer2).unwrap();
        assert_eq!(rep2.total_ratings, 1);
        assert_eq!(rep2.sum_ratings, 4);
        assert_eq!(rep2.average_rating, 80); // 4 * 20 = 80
    }

    #[test]
    fn test_reputation_calculation_with_various_ratings() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        complete_progress(&ctx, &ctx.farmer, 42, 1);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &70);
        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.status, EscrowStatus::Survived);
        assert_eq!(rec.released, 7_000); // 30% + 40% = 70%
        assert_eq!(rec.survival_rate_percent, 70);

        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);

        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 3));
        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.status, EscrowStatus::Completed);
        assert_eq!(rec.released, 10_000); // 100%
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 7));

        // Rate the main farmer from the original donor
        ctx.client.rate_planter(&ctx.donor, &ctx.farmer, &3_u32);

        // Add ratings from other sponsors (each using a fresh farmer)
        for i in 1u32..=4 {
            let donor = Address::generate(&ctx.env);
            let farmer = Address::generate(&ctx.env);
            token::StellarAssetClient::new(&ctx.env, &ctx.token).mint(&donor, &10_000);
            ctx.client.deposit(&donor, &farmer, &ctx.token, &10_000, &10, &1);
            complete_progress(&ctx, &farmer, 10, 50 + (i as u8 - 1) * 5);
            ctx.env.ledger().with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
            ctx.client.verify_survival(&farmer, &proof(&ctx.env, (i * 10) as u8), &70);
            ctx.env.ledger().with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
            ctx.client.verify_year_milestone(&farmer, &proof(&ctx.env, (i * 10 + 1) as u8));
            ctx.client.rate_planter(&donor, &farmer, &i);
        }

        // The original farmer has 1 rating of 3 stars
        let rep = ctx.client.get_planter_reputation(&ctx.farmer).unwrap();
        assert_eq!(rep.total_ratings, 1);
        assert_eq!(rep.sum_ratings, 3);
        assert_eq!(rep.average_rating, 60); // (3 * 20) / 1 = 60
    }

    #[test]
    fn test_verified_tree_count_controls_tree_mint_amount() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client
            .verify_planting(&ctx.farmer, &proof(&ctx.env, 1), &42);
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42);
        for i in 0..5 {
            ctx.client
                .verify_progress(&ctx.farmer, &proof(&ctx.env, i as u8 + 1), &42);
        }
        ctx.env.ledger().with_mut(|l| l.timestamp += 86_400);
        ctx.client
            .verify_progress(&ctx.farmer, &proof(&ctx.env, 1), &30);

        let tree_unit = 10i128.pow(token::Client::new(&ctx.env, &ctx.tree_token).decimals());
        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.verified_tree_count, 30);
        assert_eq!(rec.tree_tokens_minted, 30 * tree_unit);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #12)")]
    fn test_verified_tree_count_cannot_exceed_donation() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client
            .verify_planting(&ctx.farmer, &proof(&ctx.env, 1), &42);
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42);
        for i in 0..5 {
            ctx.client
                .verify_progress(&ctx.farmer, &proof(&ctx.env, i as u8 + 1), &42);
        }
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_progress(&ctx.farmer, &proof(&ctx.env, 1), &43);
    }

    // ── Planting density tests (#514) ───────────────────────────────────────────

    #[test]
    fn test_small_job_exempt_from_density_rules() {
        // Job size (5 hectares) is below threshold (10 hectares)
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client
            .verify_planting(&ctx.farmer, &proof(&ctx.env, 1), &42);
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42);
        for i in 0..5 {
            ctx.client
                .verify_progress(&ctx.farmer, &proof(&ctx.env, i as u8 + 1), &42);
        }
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 2), &55);
        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.status, EscrowStatus::Survived);
        
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 3));
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 6), &55);
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &100, &5);
        // Density = 100/5 = 20 trees/hectare, which is below min_density (1000)
        // But since job is small, it should be accepted
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Funded
        );
    }

    #[test]
    #[should_panic(expected = "planting density below minimum for job size")]
    fn test_large_job_below_minimum_density_rejected() {
        // Job size (10 hectares) meets threshold
        // Density must be >= 1000 trees/hectare
        let ctx = setup();
        // 5000 trees / 10 hectares = 500 trees/hectare (below 1000 minimum)
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &5_000, &10);
    }

    #[test]
    fn test_large_job_at_minimum_density_accepted() {
        // Job size (10 hectares) meets threshold
        // Density = 10000 trees / 10 hectares = 1000 trees/hectare (meets minimum)
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &10_000, &10);
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Funded
        );
    }

    #[test]
    fn test_large_job_above_minimum_density_accepted() {
        // Job size (10 hectares) meets threshold
        // Density = 15000 trees / 10 hectares = 1500 trees/hectare (above minimum)
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42);
        for i in 0..5 {
            ctx.client
                .verify_progress(&ctx.farmer, &proof(&ctx.env, i as u8 + 1), &42);
        }
        // 6th call should be rejected
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &15_000, &10);
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Funded
        );
    }

    #[test]
    fn test_custom_density_threshold() {
        // Test with custom density threshold of 500 trees/hectare
        let ctx = setup_with_density(70, 500, 5);
        // Job size (5 hectares) meets custom threshold
        // Density = 2500 trees / 5 hectares = 500 trees/hectare (meets 500 minimum)
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &2_500, &5);
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Funded
        );
    }

    #[test]
    #[should_panic(expected = "area hectares must be positive")]
    fn test_deposit_rejects_zero_area() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &0);
    }

    #[test]
    fn test_batch_deposit_creates_record_per_slot() {
        let ctx = setup();
        let f1 = Address::generate(&ctx.env);
        let f2 = Address::generate(&ctx.env);
        let slots = vec![
            &ctx.env,
            BatchSlot {
                farmer: f1.clone(),
                amount: 1_500,
                gift_recipient: None,
                referrer: None,
            },
            BatchSlot {
                farmer: f2.clone(),
                amount: 2_500,
                gift_recipient: None,
                referrer: None,
            },
        ];
        ctx.client.batch_deposit(&ctx.donor, &ctx.token, &slots);

        let r1 = ctx.client.get_record(&f1).unwrap();
        assert_eq!(r1.total_amount, 1_500);
        assert_eq!(r1.status, EscrowStatus::Funded);
        let r2 = ctx.client.get_record(&f2).unwrap();
        assert_eq!(r2.total_amount, 2_500);
    }

    // ── Dead tree replacement (#489) ──────────────────────────────────────────

    /// Helper: deposit + verify planting, leaving the escrow in `Planted`.
    fn deposit_and_plant(ctx: &Ctx) {
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client.refund(&ctx.farmer);
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42);
        ctx.client
            .verify_planting(&ctx.farmer, &proof(&ctx.env, 1), &42);
    }

    #[test]
    fn test_verify_dead_marks_escrow_dead_without_moving_funds() {
        let ctx = setup();
        deposit_and_plant(&ctx);

        let contract_balance_before = balance(&ctx.env, &ctx.token, &ctx.client.address);
        ctx.client.verify_dead(&ctx.farmer, &proof(&ctx.env, 3));

        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.status, EscrowStatus::Dead);
        assert_eq!(rec.death_proof, proof(&ctx.env, 3));
        assert_eq!(rec.replant_count, 0);
        // The retained Tranche 2 balance (25%) stays in escrow.
        assert_eq!(
            balance(&ctx.env, &ctx.token, &ctx.client.address),
            contract_balance_before
        );
        assert_eq!(rec.released, 7_500);
    }

    #[test]
    #[should_panic(expected = "tree must be planted to be marked dead")]
    fn test_verify_dead_rejected_before_planting() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42);
        ctx.client.verify_dead(&ctx.farmer, &proof(&ctx.env, 3));
    }

    #[test]
    #[should_panic(expected = "tree must be planted to be marked dead")]
    fn test_verify_dead_rejected_after_completion() {
        let ctx = setup();
        deposit_and_plant(&ctx);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client
            .verify_progress(&ctx.farmer, &proof(&ctx.env, 1), &42);
        ctx.client.refund(&ctx.farmer);
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 2), &80);
        // Completed escrows keep no balance and cannot be marked dead.
        ctx.client.verify_dead(&ctx.farmer, &proof(&ctx.env, 3));
    }

    #[test]
    fn test_request_replant_restarts_survival_cycle_for_free() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &0, &5);
    }

    // ── 1-year milestone tests (#494) ───────────────────────────────────────────

    #[test]
    #[should_panic(expected = "survival not yet verified")]
    fn test_year_milestone_before_survival_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client
            .verify_planting(&ctx.farmer, &proof(&ctx.env, 1), &42);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 2));
    }

    #[test]
    #[should_panic(expected = "1-year milestone period not yet elapsed")]
    fn test_year_milestone_too_early_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client
            .verify_planting(&ctx.farmer, &proof(&ctx.env, 1), &42);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 2), &70);
        ctx.env.ledger().with_mut(|l| l.timestamp += 86_400);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 3));
    }

    #[test]
    fn test_year_milestone_at_one_year_accepted() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client
            .verify_planting(&ctx.farmer, &proof(&ctx.env, 1), &42);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 2), &70);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 3));
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Completed
        );
    }

    #[test]
    #[should_panic(expected = "nothing left to release")]
    fn test_year_milestone_double_call_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client
            .verify_planting(&ctx.farmer, &proof(&ctx.env, 1), &42);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 2), &70);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += ONE_YEAR_SECS - SIX_MONTHS_SECS + 1);
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 3));
        ctx.client
            .verify_year_milestone(&ctx.farmer, &proof(&ctx.env, 4));
        deposit_and_plant(&ctx);

        let donor_balance_after_plant = balance(&ctx.env, &ctx.token, &ctx.donor);
        ctx.client.verify_dead(&ctx.farmer, &proof(&ctx.env, 3));

        // Advance time so we can prove the survival clock resets on replant.
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client.request_replant(&ctx.farmer);

        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.status, EscrowStatus::Planted);
        assert_eq!(rec.replant_count, 1);
        assert_eq!(rec.planted_at, ctx.env.ledger().timestamp());
        assert_eq!(rec.survival_rate_percent, 0);
        // The sponsor paid nothing for the replant.
        assert_eq!(
            balance(&ctx.env, &ctx.token, &ctx.donor),
            donor_balance_after_plant
        );
    }

    #[test]
    fn test_replanted_tree_releases_remainder_on_survival() {
        let ctx = setup();
        deposit_and_plant(&ctx);
        ctx.client.verify_dead(&ctx.farmer, &proof(&ctx.env, 3));
        ctx.client.request_replant(&ctx.farmer);

        let farmer_before = balance(&ctx.env, &ctx.token, &ctx.farmer);
        // Six months must elapse from the *replant*, not the original planting.
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client
            .verify_progress(&ctx.farmer, &proof(&ctx.env, 1), &30);
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 2), &80);

        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.status, EscrowStatus::Completed);
        assert_eq!(rec.released, 10_000);
        // Remaining 25% is released to the farmer for the surviving replant.
        assert_eq!(
            balance(&ctx.env, &ctx.token, &ctx.farmer) - farmer_before,
            2_500
        );
    }

    #[test]
    #[should_panic(expected = "6-month survival period not yet elapsed")]
    fn test_replant_resets_survival_clock() {
        let ctx = setup();
        deposit_and_plant(&ctx);
        // Let the original 6 months nearly elapse before death.
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS);
        ctx.client.verify_dead(&ctx.farmer, &proof(&ctx.env, 3));
        ctx.client.request_replant(&ctx.farmer);
        // Survival immediately after replant must fail: the clock restarted.
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client
            .verify_progress(&ctx.farmer, &proof(&ctx.env, 1), &43);
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 2), &80);
    }

    // ── Planting density tests (#514) ───────────────────────────────────────────

    #[test]
    fn test_small_job_exempt_from_density_rules() {
        // Job size (5 hectares) is below threshold (10 hectares)
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &100, &5);
        // Density = 100/5 = 20 trees/hectare, which is below min_density (1000)
        // But since job is small, it should be accepted
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Funded
        );
    }

    #[test]
    #[should_panic(expected = "planting density below minimum for job size")]
    fn test_large_job_below_minimum_density_rejected() {
        // Job size (10 hectares) meets threshold
        // Density must be >= 1000 trees/hectare
        let ctx = setup();
        // 5000 trees / 10 hectares = 500 trees/hectare (below 1000 minimum)
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &5_000, &10);
    }

    #[test]
    fn test_large_job_at_minimum_density_accepted() {
        // Job size (10 hectares) meets threshold
        // Density = 10000 trees / 10 hectares = 1000 trees/hectare (meets minimum)
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &10_000, &10);
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Funded
        );
    }

    #[test]
    fn test_large_job_above_minimum_density_accepted() {
        // Job size (10 hectares) meets threshold
        // Density = 15000 trees / 10 hectares = 1500 trees/hectare (above minimum)
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &15_000, &10);
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Funded
        );
    }

    #[test]
    fn test_custom_density_threshold() {
        // Test with custom density threshold of 500 trees/hectare
        let ctx = setup_with_density(70, 500, 5);
        // Job size (5 hectares) meets custom threshold
        // Density = 2000 trees / 5 hectares = 400 trees/hectare (below 500 minimum)
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &2_000, &5);
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Funded
        );
    }

    #[test]
    #[should_panic(expected = "area hectares must be positive")]
    fn test_deposit_rejects_zero_area() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &0);
    }

    #[test]
    fn test_repeated_death_and_replant() {
        let ctx = setup();
        deposit_and_plant(&ctx);

        ctx.client.verify_dead(&ctx.farmer, &proof(&ctx.env, 3));
        ctx.client.request_replant(&ctx.farmer);
        ctx.client.verify_dead(&ctx.farmer, &proof(&ctx.env, 4));
        ctx.client.request_replant(&ctx.farmer);

        let rec = ctx.client.get_record(&ctx.farmer).unwrap();
        assert_eq!(rec.replant_count, 2);
        assert_eq!(rec.status, EscrowStatus::Planted);
    }

    #[test]
    #[should_panic(expected = "tree is not marked dead")]
    fn test_request_replant_rejected_when_not_dead() {
        let ctx = setup();
        deposit_and_plant(&ctx);
        ctx.client.request_replant(&ctx.farmer);
    }


    // ── Oracle survival reports (#394) ────────────────────────────────────────

    #[test]
    fn test_submit_survival_report_records_report() {
        let ctx = setup();
        ctx.client.submit_survival_report(&ctx.oracle, &7, &82);

        let r = ctx.client.get_oracle_report(&7).unwrap();
        assert_eq!(r.tree_id, 7);
        assert_eq!(r.survival_rate_percent, 82);
        assert_eq!(r.oracle, ctx.oracle);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #26)")]
    fn test_submit_survival_report_rejects_unauthorized_caller() {
        let ctx = setup();
        let impostor = Address::generate(&ctx.env);
        ctx.client.submit_survival_report(&impostor, &7, &82);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #22)")]
    fn test_submit_survival_report_rejects_above_100() {
        let ctx = setup();
        ctx.client.submit_survival_report(&ctx.oracle, &7, &101);
    }

    #[test]
    fn test_oracle_report_overwrites_on_resubmission() {
        let ctx = setup();
        ctx.client.submit_survival_report(&ctx.oracle, &9, &50);
        ctx.client.submit_survival_report(&ctx.oracle, &9, &90);
        assert_eq!(
            ctx.client.get_oracle_report(&9).unwrap().survival_rate_percent,
            90
        );
    }

    #[test]
    fn test_survival_threshold_is_queryable() {
        let ctx = setup_with_threshold(85);
        assert_eq!(ctx.client.get_survival_threshold(), 85);
    }

    // ── Co-funded flow (#402) ─────────────────────────────────────────────────

    fn register_and_contribute(ctx: &Ctx, tree_id: u64, contribs: &[(Address, i128)]) {
        ctx.client.register_tree(&tree_id, &ctx.farmer, &ctx.token);
        for (funder, amount) in contribs {
            fund(&ctx.env, &ctx.token, funder, *amount);
            ctx.client.contribute(funder, &tree_id, amount);
        }
    }

    #[test]
    fn test_cofund_two_funders_full_pool_payout() {
        let ctx = setup();
        let a = Address::generate(&ctx.env);
        let b = Address::generate(&ctx.env);
        register_and_contribute(&ctx, 1, &[(a.clone(), 4_000), (b.clone(), 6_000)]);

        let funding = ctx.client.get_tree_funding(&1).unwrap();
        assert_eq!(funding.total_funded, 10_000);
        assert_eq!(funding.contributions.len(), 2);

        ctx.client.submit_survival_report(&ctx.oracle, &1, &80);
        let pre_a = balance(&ctx.env, &ctx.token, &a);
        let pre_b = balance(&ctx.env, &ctx.token, &b);

        ctx.client.release_proportional(&1, &10_000);

        assert_eq!(balance(&ctx.env, &ctx.token, &a) - pre_a, 4_000);
        assert_eq!(balance(&ctx.env, &ctx.token, &b) - pre_b, 6_000);
        assert_eq!(
            ctx.client.get_tree_funding(&1).unwrap().status,
            TreeFundingStatus::Released
        );
    }

    #[test]
    fn test_cofund_three_funders_remainder_goes_to_largest() {
        let ctx = setup();
        let a = Address::generate(&ctx.env);
        let b = Address::generate(&ctx.env);
        let c = Address::generate(&ctx.env);
        register_and_contribute(
            &ctx,
            2,
            &[(a.clone(), 100), (b.clone(), 100), (c.clone(), 101)],
        );

        let pre_a = balance(&ctx.env, &ctx.token, &a);
        let pre_b = balance(&ctx.env, &ctx.token, &b);
        let pre_c = balance(&ctx.env, &ctx.token, &c);

        ctx.client.submit_survival_report(&ctx.oracle, &2, &80);
        ctx.client.release_proportional(&2, &100);

        assert_eq!(balance(&ctx.env, &ctx.token, &a) - pre_a, 33);
        assert_eq!(balance(&ctx.env, &ctx.token, &b) - pre_b, 33);
        assert_eq!(balance(&ctx.env, &ctx.token, &c) - pre_c, 34);

        let f = ctx.client.get_tree_funding(&2).unwrap();
        assert_eq!(f.status, TreeFundingStatus::Open);
        assert_eq!(f.released, 100);
    }

    #[test]
    fn test_cofund_single_funder_receives_full_pool() {
        let ctx = setup();
        let a = Address::generate(&ctx.env);
        register_and_contribute(&ctx, 3, &[(a.clone(), 7_777)]);

        let pre_a = balance(&ctx.env, &ctx.token, &a);
        ctx.client.submit_survival_report(&ctx.oracle, &3, &80);
        ctx.client.release_proportional(&3, &7_777);

        assert_eq!(balance(&ctx.env, &ctx.token, &a) - pre_a, 7_777);
    }

    #[test]
    fn test_cofund_partial_release_then_full_release() {
        let ctx = setup();
        let a = Address::generate(&ctx.env);
        let b = Address::generate(&ctx.env);
        register_and_contribute(&ctx, 8, &[(a.clone(), 4_000), (b.clone(), 6_000)]);
        ctx.client.submit_survival_report(&ctx.oracle, &8, &80);

        ctx.client.release_proportional(&8, &7_500);
        let f = ctx.client.get_tree_funding(&8).unwrap();
        assert_eq!(f.released, 7_500);
        assert_eq!(f.status, TreeFundingStatus::Open);

        ctx.client.release_proportional(&8, &2_500);
        assert_eq!(
            ctx.client.get_tree_funding(&8).unwrap().status,
            TreeFundingStatus::Released
        );
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #13)")]
    fn test_cofund_release_exceeding_remaining_rejected() {
        let ctx = setup();
        let a = Address::generate(&ctx.env);
        register_and_contribute(&ctx, 9, &[(a, 1_000)]);
        ctx.client.submit_survival_report(&ctx.oracle, &9, &80);
        ctx.client.release_proportional(&9, &1_001);
    }

    #[test]
    fn test_cofund_same_funder_contributes_twice_share_merges() {
        let ctx = setup();
        let a = Address::generate(&ctx.env);
        ctx.client.register_tree(&4, &ctx.farmer, &ctx.token);
        fund(&ctx.env, &ctx.token, &a, 1_000);
        ctx.client.contribute(&a, &4, &1_000);
        fund(&ctx.env, &ctx.token, &a, 500);
        ctx.client.contribute(&a, &4, &500);

        let funding = ctx.client.get_tree_funding(&4).unwrap();
        assert_eq!(funding.contributions.len(), 1);
        assert_eq!(funding.contributions.get(0).unwrap().amount, 1_500);
        assert_eq!(funding.total_funded, 1_500);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #31)")]
    fn test_cofund_contribute_before_register_rejected() {
        let ctx = setup();
        let a = Address::generate(&ctx.env);
        fund(&ctx.env, &ctx.token, &a, 100);
        ctx.client.contribute(&a, &99, &100);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #27)")]
    fn test_cofund_release_without_oracle_report_rejected() {
        let ctx = setup();
        let a = Address::generate(&ctx.env);
        register_and_contribute(&ctx, 5, &[(a, 1_000)]);
        ctx.client.release_proportional(&5, &1_000);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #23)")]
    fn test_cofund_release_below_threshold_rejected() {
        let ctx = setup();
        let a = Address::generate(&ctx.env);
        register_and_contribute(&ctx, 6, &[(a, 1_000)]);
        ctx.client.submit_survival_report(&ctx.oracle, &6, &50);
        ctx.client.release_proportional(&6, &1_000);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #33)")]
    fn test_cofund_release_after_full_payout_rejected() {
        let ctx = setup();
        let a = Address::generate(&ctx.env);
        register_and_contribute(&ctx, 7, &[(a, 1_000)]);
        ctx.client.submit_survival_report(&ctx.oracle, &7, &80);
        ctx.client.release_proportional(&7, &1_000);
        ctx.client.release_proportional(&7, &1);
    }

    // ── Replay attack prevention (#481) ────────────────────────────────────────

    #[test]
    #[should_panic(expected = "proof hash already used")]
    fn test_planting_proof_replay_across_escrows_rejected() {
        let ctx = setup();
        let farmer_a = Address::generate(&ctx.env);
        let farmer_b = Address::generate(&ctx.env);
        let donor2 = Address::generate(&ctx.env);
        fund(&ctx.env, &ctx.token, &donor2, 10_000);

        ctx.client
            .deposit(&ctx.donor, &farmer_a, &ctx.token, &10_000, &1, &1);
        ctx.client
            .verify_progress(&farmer_a, &proof(&ctx.env, 1), &1);

        ctx.client
            .deposit(&donor2, &farmer_b, &ctx.token, &10_000, &1, &1);
        ctx.client
            .verify_progress(&farmer_b, &proof(&ctx.env, 1), &1);
    }

    #[test]
    fn test_planting_proof_different_hashes_across_escrows_allowed() {
        let ctx = setup();
        let farmer_a = Address::generate(&ctx.env);
        let farmer_b = Address::generate(&ctx.env);
        let donor2 = Address::generate(&ctx.env);
        fund(&ctx.env, &ctx.token, &donor2, 10_000);

        ctx.client
            .deposit(&ctx.donor, &farmer_a, &ctx.token, &10_000, &1, &1);
        ctx.client
            .verify_progress(&farmer_a, &proof(&ctx.env, 1), &1);

        ctx.client
            .deposit(&donor2, &farmer_b, &ctx.token, &10_000, &1, &1);
        ctx.client
            .verify_progress(&farmer_b, &proof(&ctx.env, 2), &1);

        assert_eq!(
            ctx.client.get_record(&farmer_b).unwrap().status,
            EscrowStatus::Planted
        );
    }

    #[test]
    #[should_panic(expected = "proof hash already used")]
    fn test_survival_proof_replay_across_escrows_rejected() {
        let ctx = setup();
        let farmer_a = Address::generate(&ctx.env);
        let farmer_b = Address::generate(&ctx.env);
        let donor2 = Address::generate(&ctx.env);
        fund(&ctx.env, &ctx.token, &donor2, 10_000);

        ctx.client
            .deposit(&ctx.donor, &farmer_a, &ctx.token, &10_000, &1, &1);
        complete_progress(&ctx, &farmer_a, 1, 1);

        ctx.client
            .deposit(&donor2, &farmer_b, &ctx.token, &10_000, &1, &1);
        complete_progress(&ctx, &farmer_b, 1, 11);

        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);

        ctx.client
            .verify_survival(&farmer_a, &proof(&ctx.env, 21), &70);
        ctx.client
            .verify_survival(&farmer_b, &proof(&ctx.env, 21), &70);
    }

    #[test]
    #[should_panic(expected = "proof hash already used")]
    fn test_planting_proof_replay_as_survival_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &1, &1);
        complete_progress(&ctx, &ctx.farmer, 1, 1);

        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += SIX_MONTHS_SECS + 1);

        // Replay one of the already-used planting proofs as a survival proof.
        ctx.client
            .verify_survival(&ctx.farmer, &proof(&ctx.env, 1), &70);
    }

    // ── Dispute resolution (#469) ─────────────────────────────────────────────

    #[test]
    fn test_open_dispute_pauses_fund_release() {
        let ctx = setup();
        let sponsor = Address::generate(&ctx.env);
        let dao = Address::generate(&ctx.env);
        ctx.client.set_dao_members(&vec![&ctx.env, dao.clone()]);

        register_and_contribute(&ctx, 20, &[(sponsor.clone(), 5_000)]);
        ctx.client.submit_survival_report(&ctx.oracle, &20, &85);

        ctx.client.open_dispute(&sponsor, &20, &proof(&ctx.env, 9));
        assert!(ctx.client.has_open_dispute(&20));

        ctx.client.cast_dao_vote(&dao, &20, &false);
        ctx.client.resolve_dispute(&dao, &20);

        let dispute = ctx.client.get_dispute(&20).unwrap();
        assert!(dispute.resolved);
        assert_eq!(dispute.outcome, DisputeOutcome::VerificationOverturned);
    }

    #[test]
    #[should_panic(expected = "fund release paused: dispute is open")]
    fn test_release_blocked_while_dispute_open() {
        let ctx = setup();
        let sponsor = Address::generate(&ctx.env);
        register_and_contribute(&ctx, 21, &[(sponsor.clone(), 3_000)]);
        ctx.client.submit_survival_report(&ctx.oracle, &21, &80);
        ctx.client.open_dispute(&sponsor, &21, &proof(&ctx.env, 10));
        ctx.client.release_proportional(&21, &3_000);
    }

    #[test]
    #[should_panic(expected = "dispute window expired")]
    fn test_open_dispute_rejected_after_seven_days() {
        let ctx = setup();
        let sponsor = Address::generate(&ctx.env);
        register_and_contribute(&ctx, 22, &[(sponsor.clone(), 1_000)]);
        ctx.client.submit_survival_report(&ctx.oracle, &22, &80);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += DISPUTE_WINDOW_SECS + 1);
        ctx.client.open_dispute(&sponsor, &22, &proof(&ctx.env, 11));
    }

    #[test]
    fn test_dao_upholds_verification_and_release_proceeds() {
        let ctx = setup();
        let sponsor = Address::generate(&ctx.env);
        let dao = Address::generate(&ctx.env);
        ctx.client.set_dao_members(&vec![&ctx.env, dao.clone()]);

        register_and_contribute(&ctx, 23, &[(sponsor.clone(), 4_000)]);
        ctx.client.submit_survival_report(&ctx.oracle, &23, &90);
        ctx.client.open_dispute(&sponsor, &23, &proof(&ctx.env, 12));

        ctx.client.cast_dao_vote(&dao, &23, &true);
        ctx.client.resolve_dispute(&dao, &23);

        assert!(!ctx.client.has_open_dispute(&23));
        assert_eq!(
            ctx.client.get_dispute(&23).unwrap().outcome,
            DisputeOutcome::VerificationUpheld
        );

        let pre = balance(&ctx.env, &ctx.token, &sponsor);
        ctx.client.release_proportional(&23, &4_000);
        assert_eq!(balance(&ctx.env, &ctx.token, &sponsor) - pre, 4_000);
    }

    // ── Sponsor cancellation tests (#482) ────────────────────────────────────

    #[test]
    fn test_sponsor_cancel_refunds_before_acceptance() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);

        let pre = balance(&ctx.env, &ctx.token, &ctx.donor);
        ctx.client.sponsor_cancel(&ctx.donor, &ctx.farmer);
        let post = balance(&ctx.env, &ctx.token, &ctx.donor);

        assert_eq!(post - pre, 10_000);
        assert_eq!(
            ctx.client.get_record(&ctx.farmer).unwrap().status,
            EscrowStatus::Refunded
        );
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #17)")]
    fn test_sponsor_cancel_unknown_escrow_rejected() {
        let ctx = setup();
        let unknown = Address::generate(&ctx.env);
        ctx.client.sponsor_cancel(&ctx.donor, &unknown);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #3)")]
    fn test_sponsor_cancel_wrong_donor_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &5_000, &10, &1);
        let impostor = Address::generate(&ctx.env);
        ctx.client.sponsor_cancel(&impostor, &ctx.farmer);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #20)")]
    fn test_sponsor_cancel_after_progress_rejected() {
        let ctx = setup();
        ctx.client
            .deposit(&ctx.donor, &ctx.farmer, &ctx.token, &10_000, &42, &5);
        ctx.client.verify_progress(&ctx.farmer, &proof(&ctx.env, 1), &42);
        ctx.client.sponsor_cancel(&ctx.donor, &ctx.farmer);
    }

    // ── Corporate bulk sponsorship tests (#487) ───────────────────────────────

    #[test]
    fn test_corp_batch_deposit_creates_escrows_and_receipt() {
        let ctx = setup();
        let f1 = Address::generate(&ctx.env);
        let f2 = Address::generate(&ctx.env);
        let f3 = Address::generate(&ctx.env);

        let slots = vec![
            &ctx.env,
            BatchSlot { farmer: f1.clone(), amount: 1_000, gift_recipient: None, referrer: None },
            BatchSlot { farmer: f2.clone(), amount: 2_000, gift_recipient: None, referrer: None },
            BatchSlot { farmer: f3.clone(), amount: 3_000, gift_recipient: None, referrer: None },
        ];

        let batch_id = ctx.client.corporate_batch_deposit(&ctx.donor, &ctx.token, &slots);
        assert_eq!(batch_id, 1);

        let receipt = ctx.client.get_corp_batch(&batch_id).unwrap();
        assert_eq!(receipt.total_amount, 6_000);
        assert_eq!(receipt.total_trees, 3);
        assert_eq!(receipt.farmer_count, 3);
        assert_eq!(receipt.sponsor, ctx.donor);

        assert_eq!(ctx.client.get_record(&f1).unwrap().status, EscrowStatus::Funded);
        assert_eq!(ctx.client.get_record(&f2).unwrap().total_amount, 2_000);
        assert_eq!(ctx.client.get_record(&f3).unwrap().total_amount, 3_000);
    }

    #[test]
    fn test_corp_batch_ids_increment() {
        let ctx = setup();
        let make_slots = |env: &soroban_sdk::Env, donor: &Address| {
            let f = Address::generate(env);
            fund(env, &ctx.token, donor, 1_000);
            vec![
                env,
                BatchSlot { farmer: f, amount: 1_000, gift_recipient: None, referrer: None },
            ]
        };
        let donor2 = Address::generate(&ctx.env);
        let slots1 = make_slots(&ctx.env, &ctx.donor);
        let slots2 = make_slots(&ctx.env, &donor2);

        let id1 = ctx.client.corporate_batch_deposit(&ctx.donor, &ctx.token, &slots1);
        let id2 = ctx.client.corporate_batch_deposit(&donor2, &ctx.token, &slots2);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #28)")]
    fn test_corp_batch_empty_rejected() {
        let ctx = setup();
        let empty = vec![&ctx.env];
        ctx.client.corporate_batch_deposit(&ctx.donor, &ctx.token, &empty);
    }

    use proptest::prelude::*;
    use std::collections::HashSet;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]
        #[test]
        fn test_tree_id_uniqueness_invariant(tree_ids in prop::collection::vec(0u64..1000u64, 1..30)) {
            let ctx = setup();
            let mut registered = HashSet::new();

            for tree_id in tree_ids {
                if registered.contains(&tree_id) {
                    // Try to register the tree again. It must fail.
                    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        ctx.client.register_tree(&tree_id, &ctx.farmer, &ctx.token);
                    }));
                    assert!(res.is_err(), "Expected panic when registering duplicate tree ID {}", tree_id);
                } else {
                    // Register the tree for the first time. It must succeed.
                    ctx.client.register_tree(&tree_id, &ctx.farmer, &ctx.token);
                    registered.insert(tree_id);

                    // Verify that the tree can be retrieved and its info is correct
                    let funding = ctx.client.get_tree_funding(&tree_id).unwrap();
                    assert_eq!(funding.tree_id, tree_id);
                    assert_eq!(funding.farmer, ctx.farmer);
                    assert_eq!(funding.token, ctx.token);
                }
            }

            // Post-condition: check that all registered tree IDs still exist and retrieve correctly
            for tree_id in &registered {
                let funding = ctx.client.get_tree_funding(tree_id).unwrap();
                assert_eq!(funding.tree_id, *tree_id);
            }
        }
    }

    // ── Tree lifecycle state machine (#462) ───────────────────────────────────

    #[test]
    fn test_tree_status_initial_is_pending() {
        let ctx = setup();
        ctx.client.register_tree(&100, &ctx.farmer, &ctx.token);
        assert_eq!(ctx.client.get_tree_status(&100), TreeStatus::Pending);
    }

    #[test]
    fn test_update_status_pending_to_planted() {
        let ctx = setup();
        ctx.client.register_tree(&100, &ctx.farmer, &ctx.token);
        let ts_before = ctx.env.ledger().timestamp();
        ctx.client.update_status(&100, &TreeStatus::Planted);
        assert_eq!(ctx.client.get_tree_status(&100), TreeStatus::Planted);
        let f = ctx.client.get_tree_funding(&100).unwrap();
        assert!(f.planted_at >= ts_before);
    }

    #[test]
    fn test_update_status_planted_to_verified() {
        let ctx = setup();
        ctx.client.register_tree(&101, &ctx.farmer, &ctx.token);
        ctx.client.update_status(&101, &TreeStatus::Planted);
        let ts_before = ctx.env.ledger().timestamp();
        ctx.client.update_status(&101, &TreeStatus::Verified);
        assert_eq!(ctx.client.get_tree_status(&101), TreeStatus::Verified);
        let f = ctx.client.get_tree_funding(&101).unwrap();
        assert!(f.verified_at >= ts_before);
    }

    #[test]
    fn test_update_status_pending_to_failed_after_timeout() {
        let ctx = setup();
        ctx.client.register_tree(&102, &ctx.farmer, &ctx.token);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += PLANTING_TIMEOUT_SECS + 1);
        ctx.client.update_status(&102, &TreeStatus::Failed);
        assert_eq!(ctx.client.get_tree_status(&102), TreeStatus::Failed);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #91)")]
    fn test_update_status_pending_to_failed_before_timeout_rejected() {
        let ctx = setup();
        ctx.client.register_tree(&103, &ctx.farmer, &ctx.token);
        ctx.client.update_status(&103, &TreeStatus::Failed);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #90)")]
    fn test_update_status_pending_to_verified_rejected() {
        let ctx = setup();
        ctx.client.register_tree(&104, &ctx.farmer, &ctx.token);
        ctx.client.update_status(&104, &TreeStatus::Verified);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #90)")]
    fn test_update_status_planted_to_failed_rejected() {
        let ctx = setup();
        ctx.client.register_tree(&105, &ctx.farmer, &ctx.token);
        ctx.client.update_status(&105, &TreeStatus::Planted);
        ctx.client.update_status(&105, &TreeStatus::Failed);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #90)")]
    fn test_update_status_verified_is_terminal() {
        let ctx = setup();
        ctx.client.register_tree(&106, &ctx.farmer, &ctx.token);
        ctx.client.update_status(&106, &TreeStatus::Planted);
        ctx.client.update_status(&106, &TreeStatus::Verified);
        ctx.client.update_status(&106, &TreeStatus::Planted);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #90)")]
    fn test_update_status_failed_is_terminal() {
        let ctx = setup();
        ctx.client.register_tree(&107, &ctx.farmer, &ctx.token);
        ctx.env
            .ledger()
            .with_mut(|l| l.timestamp += PLANTING_TIMEOUT_SECS + 1);
        ctx.client.update_status(&107, &TreeStatus::Failed);
        ctx.client.update_status(&107, &TreeStatus::Planted);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #31)")]
    fn test_update_status_nonexistent_tree() {
        let ctx = setup();
        ctx.client.update_status(&999, &TreeStatus::Planted);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #31)")]
    fn test_get_tree_status_nonexistent_tree() {
        let ctx = setup();
        ctx.client.get_tree_status(&999);
    }

    #[test]
    fn test_update_status_timestamps_correct() {
        let ctx = setup();
        ctx.client.register_tree(&108, &ctx.farmer, &ctx.token);

        let t0 = ctx.env.ledger().timestamp();

        ctx.env.ledger().with_mut(|l| l.timestamp += 1_000);
        let t1 = ctx.env.ledger().timestamp();
        ctx.client.update_status(&108, &TreeStatus::Planted);

        ctx.env.ledger().with_mut(|l| l.timestamp += 1_000);
        let t2 = ctx.env.ledger().timestamp();
        ctx.client.update_status(&108, &TreeStatus::Verified);

        let f = ctx.client.get_tree_funding(&108).unwrap();
        assert_eq!(f.registered_at, t0);
        assert_eq!(f.planted_at, t1);
        assert_eq!(f.verified_at, t2);
    }
}

    fn min_density(env: &Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::MinDensity)
            .expect(" not initialized\)
 }

 fn job_size_threshold(env: &Env) -> i128 {
 env.storage()
 .instance()
 .get(&DataKey::JobSizeThreshold)
 .expect(\not initialized\)
 }

