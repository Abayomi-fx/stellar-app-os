#![no_std]

//! Treasury Contract — Closes #648
//!
//! Extends the 2-of-3 multisig treasury (#492) with a fee-sharing mechanism
//! that automatically distributes incoming transaction fees between three
//! recipient pools: planters, verifiers, and the replanting reserve.
//!
//! # Fee sharing design
//!
//! ## Weight configuration
//!
//! Weights are stored as **basis points** (bps) where 10 000 bps = 100%.
//! The three weights must sum to exactly 10 000 at all times.
//!
//! Default split (set at `initialize`):
//!   - Planters pool   : 5 000 bps  (50 %)
//!   - Verifiers pool  : 3 000 bps  (30 %)
//!   - Replanting fund : 2 000 bps  (20 %)
//!
//! ## Governance: adjusting weights
//!
//! Weight changes follow the same 2-of-3 multisig flow used for withdrawals:
//!
//!   1. A signer calls `propose_weights(signer, planters_bps, verifiers_bps, reserve_bps)`.
//!   2. A *different* signer calls `approve_weights(signer, weight_proposal_id)`.
//!   3. On the second approval the new weights are written atomically.
//!
//! This ensures no single keyholder can unilaterally redirect fee flows.
//!
//! ## Atomic fee distribution
//!
//! `distribute_fees(from, amount)` pulls `amount` tokens from `from` into
//! the contract and immediately splits them across the three recipient
//! addresses in a single transaction.  Rounding dust (from integer division)
//! goes to the replanting reserve.  All three transfers happen atomically —
//! there is no intermediate state where only some recipients have been paid.
//!
//! ## Backward compatibility
//!
//! All existing functions (`deposit`, `propose`, `approve`, `cancel`,
//! `get_proposal`, `balance`) are preserved without modification.

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, token, Address, Env,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Total basis points representing 100 %.
const BPS_TOTAL: i128 = 10_000;

/// Default split: planters 50 %, verifiers 30 %, replanting reserve 20 %.
const DEFAULT_PLANTERS_BPS: i128 = 5_000;
const DEFAULT_VERIFIERS_BPS: i128 = 3_000;
const DEFAULT_RESERVE_BPS: i128 = 2_000;

// ── Types ─────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalStatus {
    Open,
    Executed,
    Cancelled,
}

/// A pending withdrawal proposal (unchanged from #492).
#[contracttype]
#[derive(Clone, Debug)]
pub struct WithdrawProposal {
    pub proposer: Address,
    /// Second signer who approved; `None` until approved.
    pub approver: Option<Address>,
    pub to: Address,
    pub amount: i128,
    pub status: ProposalStatus,
}

/// A governance proposal to update fee-split weights.
///
/// All three weights are expressed in basis points; they must sum to 10 000.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct WeightProposal {
    pub proposer: Address,
    /// Second signer who approved; `None` until approved.
    pub approver: Option<Address>,
    /// Proposed basis points for the planters pool.
    pub planters_bps: i128,
    /// Proposed basis points for the verifiers pool.
    pub verifiers_bps: i128,
    /// Proposed basis points for the replanting reserve.
    pub reserve_bps: i128,
    pub status: ProposalStatus,
}

/// Active fee-split weights.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FeeWeights {
    /// Destination address for the planters share.
    pub planters_addr: Address,
    /// Destination address for the verifiers share.
    pub verifiers_addr: Address,
    /// Destination address for the replanting reserve.
    pub reserve_addr: Address,
    /// Planters share in basis points (0–10 000).
    pub planters_bps: i128,
    /// Verifiers share in basis points (0–10 000).
    pub verifiers_bps: i128,
    /// Replanting reserve share in basis points (0–10 000).
    /// Receives integer-division dust so total always equals the input.
    pub reserve_bps: i128,
}

#[contracttype]
enum DataKey {
    /// `(signer_a, signer_b, signer_c)`
    Signers,
    /// Payment token address.
    Token,
    /// Auto-incrementing withdrawal proposal counter.
    NextId,
    /// Withdrawal proposal by id.
    Proposal(u32),
    /// Active fee-split weights and recipient addresses.
    FeeWeights,
    /// Auto-incrementing weight-proposal counter.
    NextWeightId,
    /// Weight-change governance proposal by id.
    WeightProposal(u32),
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct Treasury;

#[contractimpl]
impl Treasury {
    // ── Lifecycle ─────────────────────────────────────────────────────────────

    /// One-time initialisation.
    ///
    /// * `signer_a/b/c`      — three distinct multisig keyholders.
    /// * `token`             — SAC token held and disbursed by this contract.
    /// * `planters_addr`     — initial planters pool recipient.
    /// * `verifiers_addr`    — initial verifiers pool recipient.
    /// * `reserve_addr`      — initial replanting reserve recipient.
    ///
    /// Fee weights are set to the defaults (50 / 30 / 20 %).
    pub fn initialize(
        env: Env,
        signer_a: Address,
        signer_b: Address,
        signer_c: Address,
        token: Address,
        planters_addr: Address,
        verifiers_addr: Address,
        reserve_addr: Address,
    ) {
        if env.storage().instance().has(&DataKey::Signers) {
            panic!("already initialized");
        }
        if signer_a == signer_b || signer_a == signer_c || signer_b == signer_c {
            panic!("signers must be distinct");
        }

        env.storage()
            .instance()
            .set(&DataKey::Signers, &(signer_a, signer_b, signer_c));
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::NextId, &0u32);
        env.storage().instance().set(&DataKey::NextWeightId, &0u32);

        let weights = FeeWeights {
            planters_addr,
            verifiers_addr,
            reserve_addr,
            planters_bps: DEFAULT_PLANTERS_BPS,
            verifiers_bps: DEFAULT_VERIFIERS_BPS,
            reserve_bps: DEFAULT_RESERVE_BPS,
        };
        env.storage().instance().set(&DataKey::FeeWeights, &weights);

        env.events()
            .publish((symbol_short!("init"),), env.ledger().timestamp());
    }

    // ── Fee distribution ──────────────────────────────────────────────────────

    /// Pull `amount` tokens from `from` and split them atomically across the
    /// three fee recipients according to the active `FeeWeights`.
    ///
    /// Split formula:
    ///   planters_share  = amount × planters_bps  / 10_000
    ///   verifiers_share = amount × verifiers_bps / 10_000
    ///   reserve_share   = amount − planters_share − verifiers_share
    ///                     (absorbs integer-division dust)
    ///
    /// All three transfers execute in the same transaction; if any one fails
    /// the entire call reverts.
    ///
    /// # Panics
    /// - `"amount must be positive"` — `amount` ≤ 0
    /// - `"not initialized"`         — contract not yet initialised
    pub fn distribute_fees(env: Env, from: Address, amount: i128) {
        from.require_auth();

        if amount <= 0 {
            panic!("amount must be positive");
        }

        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .expect("not initialized");

        let weights: FeeWeights = env
            .storage()
            .instance()
            .get(&DataKey::FeeWeights)
            .expect("not initialized");

        let token_client = token::Client::new(&env, &token);
        let contract_addr = env.current_contract_address();

        // Pull the full fee into the treasury first
        token_client.transfer(&from, &contract_addr, &amount);

        // Compute shares — reserve absorbs any rounding dust
        let planters_share = amount * weights.planters_bps / BPS_TOTAL;
        let verifiers_share = amount * weights.verifiers_bps / BPS_TOTAL;
        let reserve_share = amount - planters_share - verifiers_share;

        // Atomically disburse all three shares
        token_client.transfer(&contract_addr, &weights.planters_addr, &planters_share);
        token_client.transfer(&contract_addr, &weights.verifiers_addr, &verifiers_share);
        token_client.transfer(&contract_addr, &weights.reserve_addr, &reserve_share);

        env.events().publish(
            (symbol_short!("fee_dist"),),
            (amount, planters_share, verifiers_share, reserve_share),
        );
    }

    /// Return the current fee-split weights and recipient addresses.
    pub fn get_fee_weights(env: Env) -> FeeWeights {
        env.storage()
            .instance()
            .get(&DataKey::FeeWeights)
            .expect("not initialized")
    }

    // ── Governance: weight updates ────────────────────────────────────────────

    /// A signer opens a governance proposal to update fee-split weights.
    ///
    /// Returns the new `weight_proposal_id`.
    ///
    /// # Panics
    /// - `"not a signer"`            — caller is not one of the three signers
    /// - `"weights must sum to 10000"` — bps values don't add up
    /// - `"weights must be non-negative"` — any bps value < 0
    pub fn propose_weights(
        env: Env,
        signer: Address,
        planters_bps: i128,
        verifiers_bps: i128,
        reserve_bps: i128,
    ) -> u32 {
        signer.require_auth();
        Self::assert_signer(&env, &signer);
        Self::assert_valid_weights(planters_bps, verifiers_bps, reserve_bps);

        let id: u32 = env
            .storage()
            .instance()
            .get(&DataKey::NextWeightId)
            .expect("not initialized");

        let proposal = WeightProposal {
            proposer: signer,
            approver: None,
            planters_bps,
            verifiers_bps,
            reserve_bps,
            status: ProposalStatus::Open,
        };

        env.storage()
            .instance()
            .set(&DataKey::WeightProposal(id), &proposal);
        env.storage()
            .instance()
            .set(&DataKey::NextWeightId, &(id + 1));

        env.events()
            .publish((symbol_short!("wt_prop"),), (id,));

        id
    }

    /// A *different* signer approves an open weight proposal.
    /// On the second approval the new weights are written atomically.
    ///
    /// # Panics
    /// - `"not a signer"`              — caller is not a recognised signer
    /// - `"weight proposal not found"` — unknown `weight_proposal_id`
    /// - `"proposal is not open"`      — already executed or cancelled
    /// - `"proposer cannot also approve"` — same address as proposer
    /// - `"already approved"`          — a second approver already exists
    pub fn approve_weights(env: Env, signer: Address, weight_proposal_id: u32) {
        signer.require_auth();
        Self::assert_signer(&env, &signer);

        let mut proposal: WeightProposal = env
            .storage()
            .instance()
            .get(&DataKey::WeightProposal(weight_proposal_id))
            .expect("weight proposal not found");

        if proposal.status != ProposalStatus::Open {
            panic!("proposal is not open");
        }
        if proposal.proposer == signer {
            panic!("proposer cannot also approve");
        }
        if proposal.approver.is_some() {
            panic!("already approved");
        }

        // Update weights atomically — read existing to preserve addresses
        let mut weights: FeeWeights = env
            .storage()
            .instance()
            .get(&DataKey::FeeWeights)
            .expect("not initialized");

        weights.planters_bps = proposal.planters_bps;
        weights.verifiers_bps = proposal.verifiers_bps;
        weights.reserve_bps = proposal.reserve_bps;

        env.storage().instance().set(&DataKey::FeeWeights, &weights);

        proposal.approver = Some(signer);
        proposal.status = ProposalStatus::Executed;
        env.storage()
            .instance()
            .set(&DataKey::WeightProposal(weight_proposal_id), &proposal);

        env.events().publish(
            (symbol_short!("wt_exec"),),
            (
                weight_proposal_id,
                proposal.planters_bps,
                proposal.verifiers_bps,
                proposal.reserve_bps,
            ),
        );
    }

    /// Any signer can cancel an open weight proposal.
    pub fn cancel_weights(env: Env, signer: Address, weight_proposal_id: u32) {
        signer.require_auth();
        Self::assert_signer(&env, &signer);

        let mut proposal: WeightProposal = env
            .storage()
            .instance()
            .get(&DataKey::WeightProposal(weight_proposal_id))
            .expect("weight proposal not found");

        if proposal.status != ProposalStatus::Open {
            panic!("proposal is not open");
        }

        proposal.status = ProposalStatus::Cancelled;
        env.storage()
            .instance()
            .set(&DataKey::WeightProposal(weight_proposal_id), &proposal);

        env.events()
            .publish((symbol_short!("wt_cncl"),), (weight_proposal_id,));
    }

    /// Return a weight proposal by id.
    pub fn get_weight_proposal(env: Env, weight_proposal_id: u32) -> WeightProposal {
        env.storage()
            .instance()
            .get(&DataKey::WeightProposal(weight_proposal_id))
            .expect("weight proposal not found")
    }

    // ── Deposit (unchanged) ───────────────────────────────────────────────────

    /// Transfer `amount` of the treasury token from `from` into this contract.
    pub fn deposit(env: Env, from: Address, amount: i128) {
        from.require_auth();
        if amount <= 0 {
            panic!("amount must be positive");
        }
        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .expect("not initialized");
        token::Client::new(&env, &token).transfer(
            &from,
            &env.current_contract_address(),
            &amount,
        );
    }

    // ── Multisig withdrawal flow (unchanged) ──────────────────────────────────

    /// A signer opens a withdrawal proposal. Returns the new `proposal_id`.
    pub fn propose(env: Env, signer: Address, to: Address, amount: i128) -> u32 {
        signer.require_auth();
        Self::assert_signer(&env, &signer);
        if amount <= 0 {
            panic!("amount must be positive");
        }

        let id: u32 = env
            .storage()
            .instance()
            .get(&DataKey::NextId)
            .expect("not initialized");

        let proposal = WithdrawProposal {
            proposer: signer,
            approver: None,
            to,
            amount,
            status: ProposalStatus::Open,
        };
        env.storage()
            .instance()
            .set(&DataKey::Proposal(id), &proposal);
        env.storage()
            .instance()
            .set(&DataKey::NextId, &(id + 1));

        env.events().publish((symbol_short!("proposed"),), (id,));

        id
    }

    /// A *different* signer approves an open proposal.
    /// Reaching 2 approvals immediately executes the transfer.
    pub fn approve(env: Env, signer: Address, proposal_id: u32) {
        signer.require_auth();
        Self::assert_signer(&env, &signer);

        let mut proposal: WithdrawProposal = env
            .storage()
            .instance()
            .get(&DataKey::Proposal(proposal_id))
            .expect("proposal not found");

        if proposal.status != ProposalStatus::Open {
            panic!("proposal is not open");
        }
        if proposal.proposer == signer {
            panic!("proposer cannot also approve");
        }
        if proposal.approver.is_some() {
            panic!("already approved");
        }

        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .expect("not initialized");

        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &proposal.to,
            &proposal.amount,
        );

        proposal.approver = Some(signer);
        proposal.status = ProposalStatus::Executed;
        env.storage()
            .instance()
            .set(&DataKey::Proposal(proposal_id), &proposal);

        env.events()
            .publish((symbol_short!("executed"),), (proposal_id,));
    }

    /// Any signer can cancel an open withdrawal proposal.
    pub fn cancel(env: Env, signer: Address, proposal_id: u32) {
        signer.require_auth();
        Self::assert_signer(&env, &signer);

        let mut proposal: WithdrawProposal = env
            .storage()
            .instance()
            .get(&DataKey::Proposal(proposal_id))
            .expect("proposal not found");

        if proposal.status != ProposalStatus::Open {
            panic!("proposal is not open");
        }

        proposal.status = ProposalStatus::Cancelled;
        env.storage()
            .instance()
            .set(&DataKey::Proposal(proposal_id), &proposal);

        env.events()
            .publish((symbol_short!("cancelled"),), (proposal_id,));
    }

    // ── Queries (unchanged) ───────────────────────────────────────────────────

    /// Return a withdrawal proposal by id.
    pub fn get_proposal(env: Env, proposal_id: u32) -> WithdrawProposal {
        env.storage()
            .instance()
            .get(&DataKey::Proposal(proposal_id))
            .expect("proposal not found")
    }

    /// Return the current treasury token balance of this contract.
    pub fn balance(env: Env) -> i128 {
        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .expect("not initialized");
        token::Client::new(&env, &token).balance(&env.current_contract_address())
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn assert_signer(env: &Env, addr: &Address) {
        let (a, b, c): (Address, Address, Address) = env
            .storage()
            .instance()
            .get(&DataKey::Signers)
            .expect("not initialized");
        if *addr != a && *addr != b && *addr != c {
            panic!("not a signer");
        }
    }

    fn assert_valid_weights(planters_bps: i128, verifiers_bps: i128, reserve_bps: i128) {
        if planters_bps < 0 || verifiers_bps < 0 || reserve_bps < 0 {
            panic!("weights must be non-negative");
        }
        if planters_bps + verifiers_bps + reserve_bps != BPS_TOTAL {
            panic!("weights must sum to 10000");
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use soroban_sdk::{testutils::Address as _, Address, Env};

    use crate::{FeeWeights, ProposalStatus, Treasury, TreasuryClient};

    // ── helpers ───────────────────────────────────────────────────────────────

    fn deploy_token(env: &Env, admin: &Address) -> Address {
        env.register_stellar_asset_contract_v2(admin.clone()).address()
    }

    fn mint(env: &Env, token: &Address, to: &Address, amount: i128) {
        soroban_sdk::token::StellarAssetClient::new(env, token).mint(to, &amount);
    }

    fn token_balance(env: &Env, token: &Address, addr: &Address) -> i128 {
        soroban_sdk::token::Client::new(env, token).balance(addr)
    }

    struct Ctx {
        env: Env,
        contract: Address,
        sa: Address,
        sb: Address,
        sc: Address,
        token: Address,
        planters: Address,
        verifiers: Address,
        reserve: Address,
    }

    fn setup() -> Ctx {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let token = deploy_token(&env, &admin);
        let sa = Address::generate(&env);
        let sb = Address::generate(&env);
        let sc = Address::generate(&env);
        let planters = Address::generate(&env);
        let verifiers = Address::generate(&env);
        let reserve = Address::generate(&env);
        let contract = env.register(Treasury, ());
        TreasuryClient::new(&env, &contract).initialize(
            &sa, &sb, &sc, &token, &planters, &verifiers, &reserve,
        );
        Ctx { env, contract, sa, sb, sc, token, planters, verifiers, reserve }
    }

    // ── distribute_fees: happy path ───────────────────────────────────────────

    #[test]
    fn test_distribute_fees_default_weights() {
        // Default: planters=50%, verifiers=30%, reserve=20%
        // amount=1000 → planters=500, verifiers=300, reserve=200
        let Ctx { env, contract, token, planters, verifiers, reserve, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let funder = Address::generate(&env);
        mint(&env, &token, &funder, 1_000);

        client.distribute_fees(&funder, &1_000i128);

        assert_eq!(token_balance(&env, &token, &planters), 500);
        assert_eq!(token_balance(&env, &token, &verifiers), 300);
        assert_eq!(token_balance(&env, &token, &reserve), 200);
        // Treasury holds nothing after distribution
        assert_eq!(client.balance(), 0);
    }

    #[test]
    fn test_distribute_fees_treasury_balance_zero_after() {
        let Ctx { env, contract, token, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let funder = Address::generate(&env);
        mint(&env, &token, &funder, 3_000);
        client.distribute_fees(&funder, &3_000i128);
        assert_eq!(client.balance(), 0);
    }

    #[test]
    fn test_distribute_fees_dust_goes_to_reserve() {
        // amount=1001: planters=500 (50%), verifiers=300 (30%), reserve=201 (dust)
        let Ctx { env, contract, token, planters, verifiers, reserve, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let funder = Address::generate(&env);
        mint(&env, &token, &funder, 1_001);

        client.distribute_fees(&funder, &1_001i128);

        assert_eq!(token_balance(&env, &token, &planters), 500);
        assert_eq!(token_balance(&env, &token, &verifiers), 300);
        // 1001 - 500 - 300 = 201  (reserve absorbs the dust)
        assert_eq!(token_balance(&env, &token, &reserve), 201);
    }

    #[test]
    fn test_distribute_fees_small_amount_all_dust_to_reserve() {
        // amount=3: planters=1 (3×5000/10000=1), verifiers=0 (3×3000/10000=0), reserve=2
        let Ctx { env, contract, token, planters, verifiers, reserve, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let funder = Address::generate(&env);
        mint(&env, &token, &funder, 3);

        client.distribute_fees(&funder, &3i128);

        assert_eq!(token_balance(&env, &token, &planters), 1);
        assert_eq!(token_balance(&env, &token, &verifiers), 0);
        assert_eq!(token_balance(&env, &token, &reserve), 2);
    }

    #[test]
    fn test_distribute_fees_emits_event() {
        // Just verify the call succeeds and balance lands correctly
        let Ctx { env, contract, token, planters, verifiers, reserve, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let funder = Address::generate(&env);
        mint(&env, &token, &funder, 10_000);
        client.distribute_fees(&funder, &10_000i128);
        assert_eq!(token_balance(&env, &token, &planters), 5_000);
        assert_eq!(token_balance(&env, &token, &verifiers), 3_000);
        assert_eq!(token_balance(&env, &token, &reserve), 2_000);
    }

    // ── distribute_fees: error paths ──────────────────────────────────────────

    #[test]
    #[should_panic(expected = "amount must be positive")]
    fn test_distribute_zero_rejected() {
        let Ctx { env, contract, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let funder = Address::generate(&env);
        client.distribute_fees(&funder, &0i128);
    }

    #[test]
    #[should_panic(expected = "amount must be positive")]
    fn test_distribute_negative_rejected() {
        let Ctx { env, contract, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let funder = Address::generate(&env);
        client.distribute_fees(&funder, &-1i128);
    }

    // ── get_fee_weights ───────────────────────────────────────────────────────

    #[test]
    fn test_get_fee_weights_defaults() {
        let Ctx { env, contract, planters, verifiers, reserve, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let w = client.get_fee_weights();
        assert_eq!(w.planters_bps, 5_000i128);
        assert_eq!(w.verifiers_bps, 3_000i128);
        assert_eq!(w.reserve_bps, 2_000i128);
        assert_eq!(w.planters_addr, planters);
        assert_eq!(w.verifiers_addr, verifiers);
        assert_eq!(w.reserve_addr, reserve);
    }

    // ── governance: propose/approve weights ───────────────────────────────────

    #[test]
    fn test_propose_and_approve_weights_updates_split() {
        // Change to 40/40/20
        let Ctx { env, contract, sa, sb, token, planters, verifiers, reserve, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);

        let wp_id = client.propose_weights(&sa, &4_000i128, &4_000i128, &2_000i128);
        client.approve_weights(&sb, &wp_id);

        let w = client.get_fee_weights();
        assert_eq!(w.planters_bps, 4_000i128);
        assert_eq!(w.verifiers_bps, 4_000i128);
        assert_eq!(w.reserve_bps, 2_000i128);

        // Now distribute and verify new split
        let funder = Address::generate(&env);
        mint(&env, &token, &funder, 10_000);
        client.distribute_fees(&funder, &10_000i128);
        assert_eq!(token_balance(&env, &token, &planters), 4_000);
        assert_eq!(token_balance(&env, &token, &verifiers), 4_000);
        assert_eq!(token_balance(&env, &token, &reserve), 2_000);
    }

    #[test]
    fn test_third_signer_can_approve_weights() {
        let Ctx { env, contract, sa, sc, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let wp_id = client.propose_weights(&sa, &6_000i128, &3_000i128, &1_000i128);
        client.approve_weights(&sc, &wp_id);
        let w = client.get_fee_weights();
        assert_eq!(w.planters_bps, 6_000i128);
    }

    #[test]
    fn test_approve_weights_marks_proposal_executed() {
        let Ctx { env, contract, sa, sb, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let wp_id = client.propose_weights(&sa, &5_000i128, &3_000i128, &2_000i128);
        client.approve_weights(&sb, &wp_id);
        let p = client.get_weight_proposal(&wp_id);
        assert_eq!(p.status, ProposalStatus::Executed);
    }

    #[test]
    fn test_weights_preserved_across_multiple_proposals() {
        // First proposal: 60/30/10. Second proposal: 70/20/10.
        // Only second should be active.
        let Ctx { env, contract, sa, sb, sc, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);

        let wp1 = client.propose_weights(&sa, &6_000i128, &3_000i128, &1_000i128);
        client.approve_weights(&sb, &wp1);

        let wp2 = client.propose_weights(&sc, &7_000i128, &2_000i128, &1_000i128);
        client.approve_weights(&sa, &wp2);

        let w = client.get_fee_weights();
        assert_eq!(w.planters_bps, 7_000i128);
        assert_eq!(w.verifiers_bps, 2_000i128);
        assert_eq!(w.reserve_bps, 1_000i128);
    }

    #[test]
    fn test_cancel_weights_proposal() {
        let Ctx { env, contract, sa, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let wp_id = client.propose_weights(&sa, &5_000i128, &3_000i128, &2_000i128);
        client.cancel_weights(&sa, &wp_id);
        let p = client.get_weight_proposal(&wp_id);
        assert_eq!(p.status, ProposalStatus::Cancelled);
        // Weights must not have changed
        let w = client.get_fee_weights();
        assert_eq!(w.planters_bps, 5_000i128);
    }

    // ── governance: error paths ───────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "weights must sum to 10000")]
    fn test_propose_weights_invalid_sum_rejected() {
        let Ctx { env, contract, sa, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        client.propose_weights(&sa, &5_000i128, &3_000i128, &1_000i128); // 9000 ≠ 10000
    }

    #[test]
    #[should_panic(expected = "weights must be non-negative")]
    fn test_propose_weights_negative_rejected() {
        let Ctx { env, contract, sa, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        client.propose_weights(&sa, &11_000i128, &-1_000i128, &0i128);
    }

    #[test]
    #[should_panic(expected = "proposer cannot also approve")]
    fn test_weight_proposer_cannot_approve_own_proposal() {
        let Ctx { env, contract, sa, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let wp_id = client.propose_weights(&sa, &5_000i128, &3_000i128, &2_000i128);
        client.approve_weights(&sa, &wp_id);
    }

    #[test]
    #[should_panic(expected = "not a signer")]
    fn test_non_signer_cannot_propose_weights() {
        let Ctx { env, contract, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let outsider = Address::generate(&env);
        client.propose_weights(&outsider, &5_000i128, &3_000i128, &2_000i128);
    }

    #[test]
    #[should_panic(expected = "proposal is not open")]
    fn test_approve_cancelled_weight_proposal_rejected() {
        let Ctx { env, contract, sa, sb, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let wp_id = client.propose_weights(&sa, &5_000i128, &3_000i128, &2_000i128);
        client.cancel_weights(&sa, &wp_id);
        client.approve_weights(&sb, &wp_id);
    }

    #[test]
    #[should_panic(expected = "proposal is not open")]
    fn test_approve_executed_weight_proposal_rejected() {
        let Ctx { env, contract, sa, sb, sc, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let wp_id = client.propose_weights(&sa, &5_000i128, &3_000i128, &2_000i128);
        client.approve_weights(&sb, &wp_id);
        client.approve_weights(&sc, &wp_id); // already executed
    }

    // ── legacy withdrawal multisig (unchanged) ────────────────────────────────

    #[test]
    fn test_propose_and_approve_executes_transfer() {
        let Ctx { env, contract, sa, sb, token, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        mint(&env, &token, &contract, 1_000);

        let recipient = Address::generate(&env);
        let proposal_id = client.propose(&sa, &recipient, &500i128);
        assert_eq!(token_balance(&env, &token, &recipient), 0);

        client.approve(&sb, &proposal_id);

        assert_eq!(token_balance(&env, &token, &recipient), 500);
        assert_eq!(client.get_proposal(&proposal_id).status, ProposalStatus::Executed);
    }

    #[test]
    fn test_third_signer_can_also_approve() {
        let Ctx { env, contract, sa, sc, token, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        mint(&env, &token, &contract, 1_000);
        let recipient = Address::generate(&env);
        let proposal_id = client.propose(&sa, &recipient, &300i128);
        client.approve(&sc, &proposal_id);
        assert_eq!(token_balance(&env, &token, &recipient), 300);
    }

    #[test]
    fn test_deposit_increases_balance() {
        let Ctx { env, contract, token, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        let funder = Address::generate(&env);
        mint(&env, &token, &funder, 2_000);
        client.deposit(&funder, &2_000i128);
        assert_eq!(client.balance(), 2_000);
    }

    #[test]
    fn test_cancel_open_proposal() {
        let Ctx { env, contract, sa, token, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        mint(&env, &token, &contract, 1_000);
        let recipient = Address::generate(&env);
        let proposal_id = client.propose(&sa, &recipient, &100i128);
        client.cancel(&sa, &proposal_id);
        assert_eq!(client.get_proposal(&proposal_id).status, ProposalStatus::Cancelled);
    }

    #[test]
    #[should_panic(expected = "already initialized")]
    fn test_double_init_rejected() {
        let Ctx { env, contract, sa, sb, sc, token, planters, verifiers, reserve } = setup();
        TreasuryClient::new(&env, &contract)
            .initialize(&sa, &sb, &sc, &token, &planters, &verifiers, &reserve);
    }

    #[test]
    #[should_panic(expected = "signers must be distinct")]
    fn test_duplicate_signers_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let token = deploy_token(&env, &admin);
        let sa = Address::generate(&env);
        let p = Address::generate(&env);
        let v = Address::generate(&env);
        let r = Address::generate(&env);
        let contract = env.register(Treasury, ());
        TreasuryClient::new(&env, &contract)
            .initialize(&sa, &sa, &sa, &token, &p, &v, &r);
    }

    #[test]
    #[should_panic(expected = "proposer cannot also approve")]
    fn test_proposer_cannot_approve_own_proposal() {
        let Ctx { env, contract, sa, token, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        mint(&env, &token, &contract, 1_000);
        let recipient = Address::generate(&env);
        let proposal_id = client.propose(&sa, &recipient, &100i128);
        client.approve(&sa, &proposal_id);
    }

    #[test]
    #[should_panic(expected = "not a signer")]
    fn test_non_signer_cannot_propose() {
        let Ctx { env, contract, token, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        mint(&env, &token, &contract, 1_000);
        let outsider = Address::generate(&env);
        let recipient = Address::generate(&env);
        client.propose(&outsider, &recipient, &100i128);
    }

    #[test]
    #[should_panic(expected = "proposal is not open")]
    fn test_approve_cancelled_proposal_rejected() {
        let Ctx { env, contract, sa, sb, token, .. } = setup();
        let client = TreasuryClient::new(&env, &contract);
        mint(&env, &token, &contract, 1_000);
        let recipient = Address::generate(&env);
        let proposal_id = client.propose(&sa, &recipient, &100i128);
        client.cancel(&sa, &proposal_id);
        client.approve(&sb, &proposal_id);
    }
}
