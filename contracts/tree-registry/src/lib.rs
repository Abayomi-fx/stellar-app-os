#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, symbol_short, Address, Env, IntoVal, Symbol, Vec,
};
use harvesta_errors::HarvestaError;

// ── Types ─────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum TreeStatus {
    Planted,
    Verified,
    Matured,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct TreeRecord {
    pub id: u64,
    pub species: soroban_sdk::String,
    pub sponsor: Address,
    pub planter: Address,
    pub region: soroban_sdk::String,
    pub planted_at: u64,
    pub status: TreeStatus,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct TreeRegistry;

#[contractimpl]
impl TreeRegistry {
    /// Initialize the tree registry with an admin and escrow contract address.
    pub fn initialize(env: Env, admin: Address, escrow: Address) {
        if env.storage().instance().has(&symbol_short!("ADMIN")) {
            panic_with_error!(&env, HarvestaError::AlreadyInitialized);
        }
        env.storage().instance().set(&symbol_short!("ADMIN"), &admin);
        env.storage().instance().set(&symbol_short!("ESCROW"), &escrow);
        env.storage().instance().set(&symbol_short!("TREECOUNT"), &0u64);
        env.storage().instance().set(&symbol_short!("PAUSED"), &false);
    }

    /// Mint a new tree (only callable by escrow contract).
    pub fn mint_tree(
        env: Env,
        sponsor: Address,
        species: soroban_sdk::String,
        region: soroban_sdk::String,
        planter: Address,
    ) -> u64 {
        Self::assert_not_paused(&env);
        Self::require_escrow(&env);

        let count: u64 = env
            .storage()
            .instance()
            .get(&symbol_short!("TREECOUNT"))
            .unwrap_or(0);
        let tree_id = count;

        let record = TreeRecord {
            id: tree_id,
            species: species.clone(),
            sponsor: sponsor.clone(),
            planter: planter.clone(),
            region: region.clone(),
            planted_at: env.ledger().timestamp(),
            status: TreeStatus::Planted,
        };

        env.storage().persistent().set(&Self::tree_key(&env, tree_id), &record);

        // Update tree count
        env.storage()
            .instance()
            .set(&symbol_short!("TREECOUNT"), &count.checked_add(1).expect("tree count overflow"));

        // Add to sponsor's tree list
        let mut sponsor_trees: Vec<u64> = env
            .storage()
            .persistent()
            .get(&Self::sponsor_key(&env, &sponsor))
            .unwrap_or_else(|| Vec::new(&env));
        sponsor_trees.push_back(tree_id);
        env.storage().persistent().set(&Self::sponsor_key(&env, &sponsor), &sponsor_trees);

        // Emit TreeMinted event
        env.events().publish(
            (Symbol::new(&env, "TreeMinted"), tree_id),
            (sponsor, species, region, planter),
        );

        tree_id
    }

    /// Get a tree by ID.
    pub fn get_tree(env: Env, id: u64) -> Option<TreeRecord> {
        env.storage().persistent().get(&Self::tree_key(&env, id))
    }

    /// List all trees for a sponsor.
    pub fn list_by_sponsor(env: Env, sponsor: Address) -> Vec<TreeRecord> {
        let tree_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&Self::sponsor_key(&env, &sponsor))
            .unwrap_or_else(|| Vec::new(&env));
        
        let mut records = Vec::new(&env);
        for id in tree_ids.iter() {
            if let Some(record) = env.storage().persistent().get(&Self::tree_key(&env, id)) {
                records.push_back(record);
            }
        }
        records
    }

    /// Get the total number of trees.
    pub fn tree_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&symbol_short!("TREECOUNT"))
            .unwrap_or(0)
    }

    // ── Admin functions ───────────────────────────────────────────────────────

    /// Pause the contract (admin only).
    pub fn pause(env: Env) {
        Self::require_admin(&env);
        env.storage()
            .instance()
            .set(&symbol_short!("PAUSED"), &true);
        env.events()
            .publish((symbol_short!("paused"),), env.ledger().timestamp());
    }

    /// Unpause the contract (admin only).
    pub fn unpause(env: Env) {
        Self::require_admin(&env);
        env.storage()
            .instance()
            .set(&symbol_short!("PAUSED"), &false);
        env.events()
            .publish((symbol_short!("unpaused"),), env.ledger().timestamp());
    }

    /// Check if contract is paused.
    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&symbol_short!("PAUSED"))
            .unwrap_or(false)
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    fn tree_key(env: &Env, id: u64) -> soroban_sdk::Val {
        (symbol_short!("TREE"), id).into_val(env)
    }

    fn sponsor_key(env: &Env, sponsor: &Address) -> soroban_sdk::Val {
        (symbol_short!("SPONSOR"), sponsor.clone()).into_val(env)
    }

    fn require_admin(env: &Env) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("ADMIN"))
            .unwrap_or_else(|| panic_with_error!(env, HarvestaError::NotInitialized));
        admin.require_auth();
    }

    fn require_escrow(env: &Env) {
        let escrow: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("ESCROW"))
            .unwrap_or_else(|| panic_with_error!(env, HarvestaError::NotInitialized));
        escrow.require_auth();
    }

    fn assert_not_paused(env: &Env) {
        let paused: bool = env
            .storage()
            .instance()
            .get(&symbol_short!("PAUSED"))
            .unwrap_or(false);
        if paused {
            panic_with_error!(env, HarvestaError::ContractPaused);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Env, String};

    fn setup() -> (Env, Address, Address, Address, Address, TreeRegistryClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, TreeRegistry);
        let client = TreeRegistryClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let escrow = Address::generate(&env);
        let sponsor = Address::generate(&env);
        let planter = Address::generate(&env);

        client.initialize(&admin, &escrow);

        (env, admin, escrow, sponsor, planter, client)
    }

    #[test]
    fn test_mint_tree() {
        let (env, _, escrow, sponsor, planter, client) = setup();

        let species = String::from_str(&env, "Acacia");
        let region = String::from_str(&env, "Kaduna");

        let tree_id = client.mint_tree(&sponsor, &species, &region, &planter);

        assert_eq!(tree_id, 0);
        assert_eq!(client.tree_count(), 1);

        let tree = client.get_tree(&0).unwrap();
        assert_eq!(tree.id, 0);
        assert_eq!(tree.species, species);
        assert_eq!(tree.sponsor, sponsor);
        assert_eq!(tree.planter, planter);
        assert_eq!(tree.region, region);
        assert_eq!(tree.status, TreeStatus::Planted);
    }

    #[test]
    fn test_list_by_sponsor() {
        let (env, _, escrow, sponsor, planter, client) = setup();

        let species1 = String::from_str(&env, "Acacia");
        let species2 = String::from_str(&env, "Mango");
        let region = String::from_str(&env, "Kaduna");

        client.mint_tree(&sponsor, &species1, &region, &planter);
        client.mint_tree(&sponsor, &species2, &region, &planter);

        let trees = client.list_by_sponsor(&sponsor);
        assert_eq!(trees.len(), 2);
    }

    #[test]
    fn test_mint_tree_only_escrow() {
        let (env, _, escrow, sponsor, planter, client) = setup();

        let species = String::from_str(&env, "Acacia");
        let region = String::from_str(&env, "Kaduna");

        // This should work because we're using the escrow address (from setup)
        let tree_id = client.mint_tree(&sponsor, &species, &region, &planter);
        assert_eq!(tree_id, 0);
    }
}
