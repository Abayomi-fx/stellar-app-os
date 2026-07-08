#![no_std]

//! Admin Controls — Closes #323
//!
//! Emergency pause/unpause, admin management, and multi-signature governance
//! for the FarmCredit contract suite.
//!
//! # Design
//!
//! - `pause()` / `unpause()` are restricted to the admin multi-sig address.
//! - All state-changing functions in dependent contracts call `assert_not_paused()`
//!   before executing (enforced via the shared pause flag stored in this contract).
//! - `update_oracle()` allows the admin to rotate the verification oracle address
//!   without redeploying contracts.
//! - `transfer_admin()` supports multi-sig admin rotation with a two-step
//!   propose → accept pattern to prevent accidental lockout.
//!
//! # Multi-Signature Governance
//!
//! - `init_multisig()` sets the signer list and approval threshold (admin-only).
//! - `propose()` lets any registered signer create a governance proposal.
//!   The proposer's approval is counted immediately; if threshold == 1 the
//!   action executes right away.
//! - `approve()` lets any other registered signer approve a pending proposal.
//!   Once approvals reach the threshold the proposal executes automatically.
//! - Signers can be added/removed and the threshold updated — all via proposals.
//! - Events are emitted on creation, approval, and execution of every proposal.

use harvesta_errors::{GovernanceError, HarvestaError};
use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, symbol_short, Address, Env, Vec,
};

// ── Storage key enum ──────────────────────────────────────────────────────────

/// Storage keys used exclusively by the multi-sig layer.
/// Existing single-admin keys continue to use `symbol_short!` literals.
#[contracttype]
#[derive(Clone, Debug)]
pub enum DataKey {
    Signers,
    Threshold,
    NextProposal,
    Proposal(u32),
    Approvals(u32),
}

// ── Multi-sig types ───────────────────────────────────────────────────────────

/// The admin action that a proposal wants to execute.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalKind {
    Pause,
    Unpause,
    UpdateOracle(Address),
    AddSigner(Address),
    RemoveSigner(Address),
    UpdateThreshold(u32),
    ProposeAdmin(Address),
}

/// A governance proposal created by a registered signer.
#[contracttype]
#[derive(Clone, Debug)]
pub struct Proposal {
    pub id: u32,
    pub kind: ProposalKind,
    pub proposer: Address,
    pub executed: bool,
}

// ── Existing types ────────────────────────────────────────────────────────────

/// `Option<Address>` wrapper, imported from the shared crate (#502).
/// Re-exported as `OptAddress` at this path so existing callers / tests
/// continue to compile without modification.
pub use shared::OptAddress;

/// Snapshot of the current admin configuration.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AdminConfig {
    /// Current admin address (multi-sig)
    pub admin: Address,
    /// Pending admin address (set during transfer, cleared on accept)
    pub pending_admin: OptAddress,
    /// Verification oracle address (ZK proof verifier)
    pub oracle: Address,
    /// Whether the contract suite is currently paused
    pub paused: bool,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct AdminControls;

#[contractimpl]
impl AdminControls {
    /// One-time initialisation.
    ///
    /// `admin`  — initial admin address (should be a multi-sig)
    /// `oracle` — initial verification oracle address
    pub fn initialize(env: Env, admin: Address, oracle: Address) {
        if env.storage().instance().has(&symbol_short!("ADMIN")) {
            panic_with_error!(&env, HarvestaError::AlreadyInitialized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("ADMIN"), &admin);
        env.storage()
            .instance()
            .set(&symbol_short!("ORACLE"), &oracle);
        env.storage()
            .instance()
            .set(&symbol_short!("PAUSED"), &false);
    }

    // ── Pause controls ────────────────────────────────────────────────────────

    /// Pause all state-changing operations across the contract suite.
    /// Restricted to admin multi-sig.
    pub fn pause(env: Env) {
        Self::require_admin(&env);
        if Self::_is_paused(&env) {
            panic_with_error!(&env, HarvestaError::AlreadyPaused);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("PAUSED"), &true);
        env.events()
            .publish((symbol_short!("Paused"),), env.ledger().timestamp());
    }

    /// Unpause the contract suite. Restricted to admin multi-sig.
    pub fn unpause(env: Env) {
        Self::require_admin(&env);
        if !Self::_is_paused(&env) {
            panic_with_error!(&env, HarvestaError::NotPaused);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("PAUSED"), &false);
        env.events()
            .publish((symbol_short!("Unpaused"),), env.ledger().timestamp());
    }

    /// Returns true if the contract suite is currently paused.
    pub fn is_paused(env: Env) -> bool {
        Self::_is_paused(&env)
    }

    /// Asserts the contract is not paused. Call this at the top of every
    /// state-changing function in dependent contracts.
    pub fn assert_not_paused(env: Env) {
        if Self::_is_paused(&env) {
            panic_with_error!(&env, HarvestaError::ContractPaused);
        }
    }

    // ── Oracle management ─────────────────────────────────────────────────────

    /// Update the verification oracle address. Restricted to admin multi-sig.
    ///
    /// Emits `OracleUpdated(old_oracle, new_oracle)` for audit trail.
    pub fn update_oracle(env: Env, new_oracle: Address) {
        Self::require_admin(&env);

        let old_oracle: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("ORACLE"))
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::NotInitialized));

        env.storage()
            .instance()
            .set(&symbol_short!("ORACLE"), &new_oracle);

        env.events()
            .publish((symbol_short!("OracleUpd"), old_oracle), new_oracle);
    }

    /// Returns the current oracle address.
    pub fn get_oracle(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&symbol_short!("ORACLE"))
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::NotInitialized))
    }

    // ── Whitelist management ──────────────────────────────────────────────────

    /// Add `addr` to the contract whitelist. Restricted to admin multi-sig.
    pub fn add_to_whitelist(env: Env, addr: Address) {
        Self::require_admin(&env);
        contract_utils::add_to_whitelist(&env, &addr);
        env.events().publish((symbol_short!("WLAdd"),), addr);
    }

    /// Remove `addr` from the contract whitelist. Restricted to admin multi-sig.
    pub fn remove_from_whitelist(env: Env, addr: Address) {
        Self::require_admin(&env);
        contract_utils::remove_from_whitelist(&env, &addr);
        env.events().publish((symbol_short!("WLRemove"),), addr);
    }

    /// Returns `true` if `addr` is whitelisted.
    pub fn is_whitelisted(env: Env, addr: Address) -> bool {
        contract_utils::is_whitelisted(&env, &addr)
    }

    /// Panics if `addr` is not whitelisted.
    pub fn assert_whitelisted(env: Env, addr: Address) {
        contract_utils::assert_whitelisted(&env, &addr);
    }

    // ── Admin rotation (two-step) ─────────────────────────────────────────────

    /// Step 1 — Current admin proposes a new admin address.
    pub fn propose_admin(env: Env, new_admin: Address) {
        Self::require_admin(&env);
        env.storage().instance().set(
            &symbol_short!("PENDADMIN"),
            &OptAddress::Some(new_admin.clone()),
        );
        env.events()
            .publish((symbol_short!("AdminProp"),), new_admin);
    }

    /// Step 2 — Proposed admin accepts the role.
    pub fn accept_admin(env: Env) {
        let pending_opt: OptAddress = env
            .storage()
            .instance()
            .get(&symbol_short!("PENDADMIN"))
            .unwrap_or(OptAddress::None);

        let pending = match pending_opt {
            OptAddress::Some(addr) => addr,
            OptAddress::None => panic_with_error!(&env, HarvestaError::NoPendingAdmin),
        };
        pending.require_auth();

        let old_admin: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("ADMIN"))
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::NotInitialized));

        env.storage()
            .instance()
            .set(&symbol_short!("ADMIN"), &pending);
        env.storage()
            .instance()
            .set(&symbol_short!("PENDADMIN"), &OptAddress::None);

        env.events()
            .publish((symbol_short!("AdminXfer"), old_admin), pending);
    }

    /// Returns the current admin configuration snapshot.
    pub fn get_config(env: Env) -> AdminConfig {
        let admin: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("ADMIN"))
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::NotInitialized));
        let oracle: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("ORACLE"))
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::NotInitialized));
        let paused: bool = env
            .storage()
            .instance()
            .get(&symbol_short!("PAUSED"))
            .unwrap_or(false);
        let pending_admin: OptAddress = env
            .storage()
            .instance()
            .get(&symbol_short!("PENDADMIN"))
            .unwrap_or(OptAddress::None);

        AdminConfig {
            admin,
            pending_admin,
            oracle,
            paused,
        }
    }

    // ── Multi-sig governance ──────────────────────────────────────────────────

    /// Set up multi-sig governance. Admin-only; can be called after `initialize`.
    /// `signers`   — initial list of authorized signers.
    /// `threshold` — number of approvals required to execute a proposal (≥ 1).
    pub fn init_multisig(env: Env, signers: Vec<Address>, threshold: u32) {
        Self::require_admin(&env);

        if signers.len() == 0 {
            panic_with_error!(&env, GovernanceError::MinimumOneSignerRequired);
        }
        if threshold == 0 {
            panic_with_error!(&env, GovernanceError::ThresholdMustBePositive);
        }
        if threshold > signers.len() {
            panic_with_error!(&env, GovernanceError::ThresholdTooHigh);
        }

        env.storage().instance().set(&DataKey::Signers, &signers);
        env.storage().instance().set(&DataKey::Threshold, &threshold);

        let next: Option<u32> = env.storage().instance().get(&DataKey::NextProposal);
        if next.is_none() {
            env.storage().instance().set(&DataKey::NextProposal, &0u32);
        }
    }

    /// Returns the registered signer list.
    pub fn get_signers(env: Env) -> Vec<Address> {
        Self::load_signers(&env)
    }

    /// Returns the current approval threshold.
    pub fn get_threshold(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(0)
    }

    /// Create a governance proposal. The proposer must be a registered signer.
    /// Their approval is counted immediately; if threshold == 1 the action
    /// executes in the same transaction and `executed` is set to `true`.
    ///
    /// Emits `PropCreat(id)` → proposer.
    /// If auto-executed also emits `PropExec(id)` → kind.
    ///
    /// Returns the new proposal ID.
    pub fn propose(env: Env, proposer: Address, kind: ProposalKind) -> u32 {
        proposer.require_auth();

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or_else(|| panic_with_error!(&env, GovernanceError::MultisigNotInitialized));

        let signers = Self::load_signers(&env);
        if !Self::signer_exists(&signers, &proposer) {
            panic_with_error!(&env, GovernanceError::NotASigner);
        }

        let id: u32 = env
            .storage()
            .instance()
            .get(&DataKey::NextProposal)
            .unwrap_or(0);

        let mut approvals: Vec<Address> = Vec::new(&env);
        approvals.push_back(proposer.clone());

        let auto_execute = approvals.len() >= threshold;

        let proposal = Proposal {
            id,
            kind: kind.clone(),
            proposer: proposer.clone(),
            executed: auto_execute,
        };

        env.storage().instance().set(&DataKey::Proposal(id), &proposal);
        env.storage().instance().set(&DataKey::Approvals(id), &approvals);
        env.storage().instance().set(&DataKey::NextProposal, &(id + 1));

        env.events()
            .publish((symbol_short!("PropCreat"), id), proposer);

        if auto_execute {
            Self::run_action(&env, &kind);
            env.events()
                .publish((symbol_short!("PropExec"), id), kind);
        }

        id
    }

    /// Approve a pending proposal. The approver must be a registered signer and
    /// must not have already approved this proposal.
    /// When approvals reach the threshold the proposal executes immediately.
    ///
    /// Emits `PropAppro(id)` → approver.
    /// If execution is triggered also emits `PropExec(id)` → kind.
    pub fn approve(env: Env, approver: Address, proposal_id: u32) {
        approver.require_auth();

        let signers = Self::load_signers(&env);
        if !Self::signer_exists(&signers, &approver) {
            panic_with_error!(&env, GovernanceError::NotASigner);
        }

        let mut proposal: Proposal = env
            .storage()
            .instance()
            .get(&DataKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, GovernanceError::ProposalNotFound));

        if proposal.executed {
            panic_with_error!(&env, GovernanceError::ProposalAlreadyExecuted);
        }

        let mut approvals: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Approvals(proposal_id))
            .unwrap_or_else(|| Vec::new(&env));

        if Self::signer_exists(&approvals, &approver) {
            panic_with_error!(&env, GovernanceError::AlreadyApproved);
        }

        approvals.push_back(approver.clone());
        env.storage()
            .instance()
            .set(&DataKey::Approvals(proposal_id), &approvals);

        env.events()
            .publish((symbol_short!("PropAppro"), proposal_id), approver);

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(1);

        if approvals.len() >= threshold {
            let kind = proposal.kind.clone();
            proposal.executed = true;
            env.storage()
                .instance()
                .set(&DataKey::Proposal(proposal_id), &proposal);
            Self::run_action(&env, &kind);
            env.events()
                .publish((symbol_short!("PropExec"), proposal_id), kind);
        }
    }

    /// Returns a proposal by ID. Panics if not found.
    pub fn get_proposal(env: Env, proposal_id: u32) -> Proposal {
        env.storage()
            .instance()
            .get(&DataKey::Proposal(proposal_id))
            .unwrap_or_else(|| panic_with_error!(&env, GovernanceError::ProposalNotFound))
    }

    // ── internal ──────────────────────────────────────────────────────────────

    fn require_admin(env: &Env) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("ADMIN"))
            .unwrap_or_else(|| panic_with_error!(env, HarvestaError::NotInitialized));
        admin.require_auth();
    }

    fn _is_paused(env: &Env) -> bool {
        env.storage()
            .instance()
            .get(&symbol_short!("PAUSED"))
            .unwrap_or(false)
    }

    fn load_signers(env: &Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::Signers)
            .unwrap_or_else(|| Vec::new(env))
    }

    fn signer_exists(signers: &Vec<Address>, addr: &Address) -> bool {
        for s in signers.iter() {
            if s == *addr {
                return true;
            }
        }
        false
    }

    fn remove_from_vec(env: &Env, signers: &Vec<Address>, target: &Address) -> Vec<Address> {
        let mut result = Vec::new(env);
        for s in signers.iter() {
            if s != *target {
                result.push_back(s);
            }
        }
        result
    }

    /// Execute the action encoded in a proposal after threshold approvals.
    fn run_action(env: &Env, kind: &ProposalKind) {
        match kind {
            ProposalKind::Pause => {
                if Self::_is_paused(env) {
                    panic_with_error!(env, HarvestaError::AlreadyPaused);
                }
                env.storage()
                    .instance()
                    .set(&symbol_short!("PAUSED"), &true);
                env.events()
                    .publish((symbol_short!("Paused"),), env.ledger().timestamp());
            }
            ProposalKind::Unpause => {
                if !Self::_is_paused(env) {
                    panic_with_error!(env, HarvestaError::NotPaused);
                }
                env.storage()
                    .instance()
                    .set(&symbol_short!("PAUSED"), &false);
                env.events()
                    .publish((symbol_short!("Unpaused"),), env.ledger().timestamp());
            }
            ProposalKind::UpdateOracle(new_oracle) => {
                let old_oracle: Address = env
                    .storage()
                    .instance()
                    .get(&symbol_short!("ORACLE"))
                    .unwrap_or_else(|| panic_with_error!(env, HarvestaError::NotInitialized));
                env.storage()
                    .instance()
                    .set(&symbol_short!("ORACLE"), new_oracle);
                env.events()
                    .publish((symbol_short!("OracleUpd"), old_oracle), new_oracle.clone());
            }
            ProposalKind::AddSigner(signer) => {
                let mut signers = Self::load_signers(env);
                if Self::signer_exists(&signers, signer) {
                    panic_with_error!(env, GovernanceError::SignerAlreadyExists);
                }
                signers.push_back(signer.clone());
                env.storage().instance().set(&DataKey::Signers, &signers);
                env.events()
                    .publish((symbol_short!("SignAdd"),), signer.clone());
            }
            ProposalKind::RemoveSigner(signer) => {
                let signers = Self::load_signers(env);
                if !Self::signer_exists(&signers, signer) {
                    panic_with_error!(env, GovernanceError::SignerNotFound);
                }
                let threshold: u32 = env
                    .storage()
                    .instance()
                    .get(&DataKey::Threshold)
                    .unwrap_or(1);
                if signers.len() - 1 < threshold {
                    panic_with_error!(env, GovernanceError::ThresholdTooHigh);
                }
                let new_signers = Self::remove_from_vec(env, &signers, signer);
                env.storage().instance().set(&DataKey::Signers, &new_signers);
                env.events()
                    .publish((symbol_short!("SignRem"),), signer.clone());
            }
            ProposalKind::UpdateThreshold(new_threshold) => {
                if *new_threshold == 0 {
                    panic_with_error!(env, GovernanceError::ThresholdMustBePositive);
                }
                let signers = Self::load_signers(env);
                if *new_threshold > signers.len() {
                    panic_with_error!(env, GovernanceError::ThresholdTooHigh);
                }
                env.storage()
                    .instance()
                    .set(&DataKey::Threshold, new_threshold);
            }
            ProposalKind::ProposeAdmin(new_admin) => {
                env.storage().instance().set(
                    &symbol_short!("PENDADMIN"),
                    &OptAddress::Some(new_admin.clone()),
                );
                env.events()
                    .publish((symbol_short!("AdminProp"),), new_admin.clone());
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, vec, Address, Env};

    fn setup() -> (Env, Address, Address, AdminControlsClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, AdminControls);
        let client = AdminControlsClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let oracle = Address::generate(&env);
        client.initialize(&admin, &oracle);
        (env, admin, oracle, client)
    }

    /// Setup with multi-sig enabled: 2-of-2 threshold.
    fn setup_multisig() -> (Env, Address, Address, AdminControlsClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, AdminControls);
        let client = AdminControlsClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        let oracle = Address::generate(&env);
        client.initialize(&admin, &oracle);
        let signer1 = Address::generate(&env);
        let signer2 = Address::generate(&env);
        client.init_multisig(&vec![&env, signer1.clone(), signer2.clone()], &2);
        (env, signer1, signer2, client)
    }

    // ── Existing tests (unchanged) ────────────────────────────────────────────

    #[test]
    fn test_initial_state() {
        let (_, admin, oracle, client) = setup();
        let config = client.get_config();
        assert_eq!(config.admin, admin);
        assert_eq!(config.oracle, oracle);
        assert!(!config.paused);
        assert!(config.pending_admin.is_none());
    }

    #[test]
    fn test_pause_unpause() {
        let (_, _, _, client) = setup();

        assert!(!client.is_paused());
        client.pause();
        assert!(client.is_paused());
        client.unpause();
        assert!(!client.is_paused());
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #4)")]
    fn test_assert_not_paused_panics_when_paused() {
        let (_, _, _, client) = setup();
        client.pause();
        client.assert_not_paused();
    }

    #[test]
    fn test_assert_not_paused_passes_when_unpaused() {
        let (_, _, _, client) = setup();
        client.assert_not_paused();
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #5)")]
    fn test_double_pause_rejected() {
        let (_, _, _, client) = setup();
        client.pause();
        client.pause();
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #6)")]
    fn test_unpause_when_not_paused_rejected() {
        let (_, _, _, client) = setup();
        client.unpause();
    }

    #[test]
    fn test_update_oracle() {
        let (env, _, _, client) = setup();
        let new_oracle = Address::generate(&env);

        client.update_oracle(&new_oracle);
        assert_eq!(client.get_oracle(), new_oracle);
        assert_eq!(client.get_config().oracle, new_oracle);
    }

    #[test]
    fn test_two_step_admin_transfer() {
        let (env, _old_admin, _, client) = setup();
        let new_admin = Address::generate(&env);

        client.propose_admin(&new_admin);
        let config = client.get_config();
        assert_eq!(config.pending_admin, OptAddress::Some(new_admin.clone()));

        client.accept_admin();
        let config = client.get_config();
        assert_eq!(config.admin, new_admin);
        assert!(config.pending_admin.is_none());
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #7)")]
    fn test_accept_admin_without_proposal_rejected() {
        let (_, _, _, client) = setup();
        client.accept_admin();
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1)")]
    fn test_double_initialize_rejected() {
        let (env, admin, oracle, client) = setup();
        client.initialize(&admin, &oracle);
    }

    // ── Whitelist tests ───────────────────────────────────────────────────────

    #[test]
    fn test_add_to_whitelist() {
        let (env, _, _, client) = setup();
        let allowed = Address::generate(&env);
        assert!(!client.is_whitelisted(&allowed));
        client.add_to_whitelist(&allowed);
        assert!(client.is_whitelisted(&allowed));
    }

    #[test]
    fn test_remove_from_whitelist() {
        let (env, _, _, client) = setup();
        let allowed = Address::generate(&env);
        client.add_to_whitelist(&allowed);
        assert!(client.is_whitelisted(&allowed));
        client.remove_from_whitelist(&allowed);
        assert!(!client.is_whitelisted(&allowed));
    }

    #[test]
    fn test_is_whitelisted_returns_false_for_unlisted() {
        let (env, _, _, client) = setup();
        let unknown = Address::generate(&env);
        assert!(!client.is_whitelisted(&unknown));
    }

    #[test]
    fn test_assert_whitelisted_passes_for_listed() {
        let (env, _, _, client) = setup();
        let allowed = Address::generate(&env);
        client.add_to_whitelist(&allowed);
        client.assert_whitelisted(&allowed);
    }

    #[test]
    #[should_panic(expected = "address not whitelisted")]
    fn test_assert_whitelisted_panics_for_unlisted() {
        let (env, _, _, client) = setup();
        let unknown = Address::generate(&env);
        client.assert_whitelisted(&unknown);
    }

    #[test]
    fn test_whitelist_is_independent_per_contract() {
        let (env, _, _, _) = setup();
        let addr = Address::generate(&env);

        let contract_a = env.register_contract(None, AdminControls);
        let client_a = AdminControlsClient::new(&env, &contract_a);
        let admin_a = Address::generate(&env);
        let oracle_a = Address::generate(&env);
        client_a.initialize(&admin_a, &oracle_a);

        let contract_b = env.register_contract(None, AdminControls);
        let client_b = AdminControlsClient::new(&env, &contract_b);
        let admin_b = Address::generate(&env);
        let oracle_b = Address::generate(&env);
        client_b.initialize(&admin_b, &oracle_b);

        client_a.add_to_whitelist(&addr);
        assert!(client_a.is_whitelisted(&addr));
        assert!(!client_b.is_whitelisted(&addr));
    }

    // ── Multi-sig tests ───────────────────────────────────────────────────────

    #[test]
    fn test_init_multisig_stores_signers_and_threshold() {
        let (env, signer1, signer2, client) = setup_multisig();
        let signers = client.get_signers();
        assert_eq!(signers.len(), 2);
        assert!(AdminControls::signer_exists(&signers, &signer1));
        assert!(AdminControls::signer_exists(&signers, &signer2));
        assert_eq!(client.get_threshold(), 2);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #2)")]
    fn test_init_multisig_rejects_empty_signers() {
        let (env, _, _, client) = setup();
        let empty: Vec<Address> = Vec::new(&env);
        client.init_multisig(&empty, &1);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #3)")]
    fn test_init_multisig_rejects_zero_threshold() {
        let (env, _, _, client) = setup();
        let signer = Address::generate(&env);
        client.init_multisig(&vec![&env, signer], &0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #4)")]
    fn test_init_multisig_rejects_threshold_exceeding_signers() {
        let (env, _, _, client) = setup();
        let signer = Address::generate(&env);
        client.init_multisig(&vec![&env, signer], &2);
    }

    #[test]
    fn test_propose_creates_proposal_and_emits_event() {
        let (env, signer1, _signer2, client) = setup_multisig();
        let id = client.propose(&signer1, &ProposalKind::Pause);
        assert_eq!(id, 0);
        let proposal = client.get_proposal(&0);
        assert_eq!(proposal.id, 0);
        assert_eq!(proposal.kind, ProposalKind::Pause);
        assert_eq!(proposal.proposer, signer1);
        assert!(!proposal.executed);
    }

    #[test]
    fn test_propose_auto_executes_when_threshold_is_one() {
        let (env, _, _, client) = setup();
        let signer = Address::generate(&env);
        client.init_multisig(&vec![&env, signer.clone()], &1);

        assert!(!client.is_paused());
        client.propose(&signer, &ProposalKind::Pause);
        assert!(client.is_paused());

        let proposal = client.get_proposal(&0);
        assert!(proposal.executed);
    }

    #[test]
    fn test_approve_executes_at_threshold() {
        let (_, signer1, signer2, client) = setup_multisig();

        let id = client.propose(&signer1, &ProposalKind::Pause);
        assert!(!client.is_paused());

        client.approve(&signer2, &id);
        assert!(client.is_paused());

        let proposal = client.get_proposal(&id);
        assert!(proposal.executed);
    }

    #[test]
    fn test_approve_does_not_execute_below_threshold() {
        let (env, signer1, signer2, client) = setup_multisig();
        let signer3 = Address::generate(&env);
        let admin = Address::generate(&env);
        let oracle = Address::generate(&env);

        let contract_id2 = env.register_contract(None, AdminControls);
        let client2 = AdminControlsClient::new(&env, &contract_id2);
        client2.initialize(&admin, &oracle);
        client2.init_multisig(
            &vec![&env, signer1.clone(), signer2.clone(), signer3.clone()],
            &3,
        );

        let id = client2.propose(&signer1, &ProposalKind::Pause);
        assert!(!client2.is_paused());
        client2.approve(&signer2, &id);
        assert!(!client2.is_paused());
        client2.approve(&signer3, &id);
        assert!(client2.is_paused());
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #6)")]
    fn test_propose_rejected_for_non_signer() {
        let (env, _, _, client) = setup_multisig();
        let outsider = Address::generate(&env);
        client.propose(&outsider, &ProposalKind::Pause);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #6)")]
    fn test_approve_rejected_for_non_signer() {
        let (env, signer1, _, client) = setup_multisig();
        let id = client.propose(&signer1, &ProposalKind::Pause);
        let outsider = Address::generate(&env);
        client.approve(&outsider, &id);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #9)")]
    fn test_approve_rejected_when_already_approved() {
        let (_, signer1, _, client) = setup_multisig();
        let id = client.propose(&signer1, &ProposalKind::Pause);
        client.approve(&signer1, &id);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #8)")]
    fn test_approve_rejected_when_proposal_already_executed() {
        let (env, _, _, client) = setup();
        let signer = Address::generate(&env);
        client.init_multisig(&vec![&env, signer.clone()], &1);
        let id = client.propose(&signer, &ProposalKind::Pause);
        client.approve(&signer, &id);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #7)")]
    fn test_get_proposal_panics_for_unknown_id() {
        let (_, _, _, client) = setup_multisig();
        client.get_proposal(&99);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #5)")]
    fn test_propose_panics_when_multisig_not_initialized() {
        let (env, _, _, client) = setup();
        let signer = Address::generate(&env);
        client.propose(&signer, &ProposalKind::Pause);
    }

    #[test]
    fn test_add_signer_via_proposal() {
        let (env, signer1, signer2, client) = setup_multisig();
        let signer3 = Address::generate(&env);

        let id = client.propose(&signer1, &ProposalKind::AddSigner(signer3.clone()));
        client.approve(&signer2, &id);

        let signers = client.get_signers();
        assert_eq!(signers.len(), 3);
        assert!(AdminControls::signer_exists(&signers, &signer3));
    }

    #[test]
    fn test_remove_signer_via_proposal() {
        let (env, signer1, signer2, client) = setup_multisig();
        let signer3 = Address::generate(&env);
        let id_add = client.propose(&signer1, &ProposalKind::AddSigner(signer3.clone()));
        client.approve(&signer2, &id_add);

        let id_rem = client.propose(&signer1, &ProposalKind::RemoveSigner(signer3.clone()));
        client.approve(&signer2, &id_rem);

        let signers = client.get_signers();
        assert_eq!(signers.len(), 2);
        assert!(!AdminControls::signer_exists(&signers, &signer3));
    }

    #[test]
    fn test_update_threshold_via_proposal() {
        let (_, signer1, signer2, client) = setup_multisig();

        let id = client.propose(&signer1, &ProposalKind::UpdateThreshold(1));
        client.approve(&signer2, &id);

        assert_eq!(client.get_threshold(), 1);
    }

    #[test]
    fn test_update_oracle_via_proposal() {
        let (env, signer1, signer2, client) = setup_multisig();
        let new_oracle = Address::generate(&env);

        let id = client.propose(&signer1, &ProposalKind::UpdateOracle(new_oracle.clone()));
        client.approve(&signer2, &id);

        assert_eq!(client.get_oracle(), new_oracle);
    }

    #[test]
    fn test_propose_admin_via_proposal() {
        let (env, signer1, signer2, client) = setup_multisig();
        let new_admin = Address::generate(&env);

        let id = client.propose(&signer1, &ProposalKind::ProposeAdmin(new_admin.clone()));
        client.approve(&signer2, &id);

        let config = client.get_config();
        assert_eq!(config.pending_admin, OptAddress::Some(new_admin));
    }

    #[test]
    fn test_unpause_via_proposal() {
        let (env, signer1, signer2, client) = setup_multisig();

        let id_pause = client.propose(&signer1, &ProposalKind::Pause);
        client.approve(&signer2, &id_pause);
        assert!(client.is_paused());

        let id_unpause = client.propose(&signer1, &ProposalKind::Unpause);
        client.approve(&signer2, &id_unpause);
        assert!(!client.is_paused());
    }

    #[test]
    fn test_proposal_ids_increment() {
        let (_, signer1, signer2, client) = setup_multisig();

        let id0 = client.propose(&signer1, &ProposalKind::Pause);
        client.approve(&signer2, &id0);
        client.propose(&signer1, &ProposalKind::Unpause);

        assert_eq!(id0, 0);
        let p1 = client.get_proposal(&1);
        assert_eq!(p1.id, 1);
    }
}
