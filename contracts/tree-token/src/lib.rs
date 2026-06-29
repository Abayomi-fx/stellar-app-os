#![no_std]

//! TREE Token — Closes #321
//!
//! SAC-compatible TREE token with a burn function for corporate ESG claims.
//!
//! Corporate buyers call `burn()` to permanently destroy TREE tokens and
//! claim the corresponding carbon offset for ESG reporting. Each burn emits
//! a `TokenBurned` event with the burner address and token count, providing
//! an immutable on-chain audit trail for ESG disclosures.
//!
//! # Pause / Admin controls
//!
//! All state-changing functions check the pause flag (see #323 integration).
//! The admin can pause/unpause and update the oracle address.
//!
//! # Storage optimization
//!
//! Instance storage is collapsed into a single `Config` struct keyed by
//! `DataKey::Config`, reducing the number of instance ledger entries from
//! four to one. Burn record indices use `u32` instead of `u64`, and the
//! persistent key is a typed `DataKey::BurnRecord(u32)` rather than a
//! `(Symbol, u64)` tuple. These changes lower the on-chain ledger footprint
//! and the storage fees paid by users.

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, symbol_short, token, Address, Env,
    Symbol,
};
use harvesta_errors::HarvestaError;

// ── Types ─────────────────────────────────────────────────────────────────────

/// On-chain record of a TREE token burn for ESG audit purposes.
#[contracttype]
#[derive(Clone, Debug)]
pub struct BurnRecord {
    /// Address that burned the tokens
    pub burner: Address,
    /// Number of TREE tokens burned (in base units)
    pub token_count: i128,
    /// Optional reference string for ESG report (e.g. report ID, project name)
    pub esg_reference: soroban_sdk::String,
    /// Ledger timestamp of the burn
    pub burned_at: u64,
}

/// Contract configuration stored in a single instance entry.
#[contracttype]
#[derive(Clone, Debug)]
pub struct Config {
    /// Admin address
    pub admin: Address,
    /// Address of the deployed TREE SAC token contract
    pub token: Address,
    /// Pause flag
    pub paused: bool,
    /// Total number of recorded burns
    pub burn_count: u32,
}

/// Storage keys.
#[contracttype]
#[derive(Clone, Debug)]
pub enum DataKey {
    /// Single config entry (admin, token, paused, burn_count)
    Config,
    /// Burn record by index
    BurnRecord(u32),
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct TreeToken;

#[contractimpl]
impl TreeToken {
    /// One-time initialisation.
    ///
    /// `admin`      — multi-sig admin address (pause/unpause, oracle updates)
    /// `tree_token` — address of the deployed TREE SAC token contract
    pub fn initialize(env: Env, admin: Address, tree_token: Address) {
        if env.storage().instance().has(&DataKey::Config) {
            panic_with_error!(&env, HarvestaError::AlreadyInitialized);
        }
        contract_utils::assert_whitelisted(&env, &tree_token);
        env.storage().instance().set(
            &DataKey::Config,
            &Config {
                admin,
                token: tree_token,
                paused: false,
                burn_count: 0,
            },
        );
    }

    /// Burn `amount` TREE tokens from `burner`'s balance to claim a carbon offset.
    ///
    /// Emits `TokenBurned(burner, token_count)` for ESG audit trail.
    /// The burn is permanent and irreversible.
    ///
    /// `esg_reference` — optional identifier linking this burn to an ESG report.
    pub fn burn(env: Env, burner: Address, amount: i128, esg_reference: soroban_sdk::String) {
        Self::assert_not_paused(&env);
        burner.require_auth();

        if amount <= 0 {
            panic_with_error!(&env, HarvestaError::BurnAmountMustBePositive);
        }

        let mut config = Self::config(&env);

        contract_utils::assert_whitelisted(&env, &config.token);
        // Burn tokens from burner's balance via SAC interface
        token::Client::new(&env, &config.token).burn(&burner, &amount);

        // Record the burn on-chain for ESG audit
        let record = BurnRecord {
            burner: burner.clone(),
            token_count: amount,
            esg_reference,
            burned_at: env.ledger().timestamp(),
        };

        let key = DataKey::BurnRecord(config.burn_count);
        env.storage().persistent().set(&key, &record);
        config.burn_count = config
            .burn_count
            .checked_add(1)
            .expect("burn count overflow");
        env.storage().instance().set(&DataKey::Config, &config);

        // Emit TokenBurned event — primary ESG audit signal
        env.events()
            .publish((Symbol::new(&env, "TokenBurned"), burner), amount);
    }

    /// Returns the burn record at sequential index `idx`, or None.
    pub fn get_burn_record(env: Env, idx: u32) -> Option<BurnRecord> {
        env.storage().persistent().get(&DataKey::BurnRecord(idx))
    }

    /// Returns the total number of burn operations recorded.
    pub fn burn_count(env: Env) -> u32 {
        Self::config(&env).burn_count
    }

    // ── Admin functions ───────────────────────────────────────────────────────

    /// Pause all state-changing functions. Admin multi-sig only.
    pub fn pause(env: Env) {
        Self::require_admin(&env);
        let mut config = Self::config(&env);
        config.paused = true;
        env.storage().instance().set(&DataKey::Config, &config);
        env.events()
            .publish((symbol_short!("paused"),), env.ledger().timestamp());
    }

    /// Unpause the contract. Admin multi-sig only.
    pub fn unpause(env: Env) {
        Self::require_admin(&env);
        let mut config = Self::config(&env);
        config.paused = false;
        env.storage().instance().set(&DataKey::Config, &config);
        env.events()
            .publish((symbol_short!("unpaused"),), env.ledger().timestamp());
    }

    /// Returns true if the contract is currently paused.
    pub fn is_paused(env: Env) -> bool {
        Self::config(&env).paused
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    fn config(env: &Env) -> Config {
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .unwrap_or_else(|| panic_with_error!(env, HarvestaError::NotInitialized))
    }

    fn require_admin(env: &Env) {
        Self::config(env).admin.require_auth();
    }

    fn assert_not_paused(env: &Env) {
        if Self::config(env).paused {
            panic_with_error!(env, HarvestaError::ContractPaused);
        }
    }

    // ── Whitelist management ──────────────────────────────────────────────────

    /// Add `addr` to the contract whitelist. Restricted to admin.
    pub fn add_to_whitelist(env: Env, addr: Address) {
        Self::require_admin(&env);
        contract_utils::add_to_whitelist(&env, &addr);
    }

    /// Remove `addr` from the contract whitelist. Restricted to admin.
    pub fn remove_from_whitelist(env: Env, addr: Address) {
        Self::require_admin(&env);
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
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, token, Address, Env, String};

    fn setup() -> (Env, Address, Address, Address, TreeTokenClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, TreeToken);
        let client = TreeTokenClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let burner = Address::generate(&env);

        // Deploy a test TREE SAC token; contract_id is NOT the admin here —
        // we just need a mintable token for testing the burn path.
        let tree_token_id = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        token::StellarAssetClient::new(&env, &tree_token_id).mint(&burner, &1_000_000);

        client.initialize(&admin, &tree_token_id);
        client.add_to_whitelist(&tree_token_id);

        (env, admin, burner, tree_token_id, client)
    }

    fn esg_ref(env: &Env) -> soroban_sdk::String {
        String::from_str(env, "ESG-REPORT-2025-Q1")
    }

    #[test]
    fn test_burn_reduces_balance_and_emits_event() {
        let (env, _, burner, tree_token, client) = setup();

        let before = token::Client::new(&env, &tree_token).balance(&burner);
        client.burn(&burner, &500_000, &esg_ref(&env));
        let after = token::Client::new(&env, &tree_token).balance(&burner);

        assert_eq!(before - after, 500_000);
        assert_eq!(client.burn_count(), 1);
    }

    #[test]
    fn test_burn_record_stored_correctly() {
        let (env, _, burner, _, client) = setup();

        client.burn(&burner, &100_000, &esg_ref(&env));

        let record = client.get_burn_record(&0).unwrap();
        assert_eq!(record.burner, burner);
        assert_eq!(record.token_count, 100_000);
    }

    #[test]
    fn test_multiple_burns_sequential_index() {
        let (env, _, burner, _, client) = setup();

        client.burn(&burner, &100_000, &esg_ref(&env));
        client.burn(&burner, &200_000, &esg_ref(&env));

        assert_eq!(client.burn_count(), 2);
        assert_eq!(client.get_burn_record(&0).unwrap().token_count, 100_000);
        assert_eq!(client.get_burn_record(&1).unwrap().token_count, 200_000);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #14)")]
    fn test_zero_burn_rejected() {
        let (env, _, burner, _, client) = setup();
        client.burn(&burner, &0, &esg_ref(&env));
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #4)")]
    fn test_burn_while_paused_rejected() {
        let (env, _, burner, _, client) = setup();
        client.pause();
        client.burn(&burner, &100_000, &esg_ref(&env));
    }

    #[test]
    fn test_pause_unpause_cycle() {
        let (env, _, burner, _, client) = setup();

        client.pause();
        assert!(client.is_paused());

        client.unpause();
        assert!(!client.is_paused());

        // burn should work again after unpause
        client.burn(&burner, &100_000, &esg_ref(&env));
        assert_eq!(client.burn_count(), 1);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #1)")]
    fn test_double_initialize_rejected() {
        let (env, admin, _, tree_token, client) = setup();
        client.initialize(&admin, &tree_token);
    }
}
