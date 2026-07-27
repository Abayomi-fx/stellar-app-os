#![no_std]

use soroban_sdk::{
    contract, contractclient, contracterror, contractimpl, contracttype, panic_with_error,
    symbol_short, Address, Env, IntoVal, Symbol, Val, Vec,
};
use harvesta_errors::HarvestaError;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum TreeRegistryError {
    NotFound = 85,
    InvalidStatus = 86,
    NotAuthorized = 87,
    SpeciesNotFound = 88,
    SpeciesAlreadyExists = 89,
    InvalidSpeciesName = 90,
}

const ONE_YEAR_SECS: u64 = 31_536_000;
const CO2_KG_PER_YEAR: i128 = 48;
const DEFAULT_PERSISTENT_TTL: u32 = 1_576_800;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum TreeStatus {
    Planted,
    Verified,
    Matured,
    Rejected,
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
    pub notes_hash: Option<soroban_sdk::String>,
    pub milestone_claims: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct SpeciesInfo {
    pub co2_scaled: i128,
    pub maturity_years: u32,
    pub updated_at: u64,
}

#[contract]
pub struct TreeRegistry;

#[contractimpl]
impl TreeRegistry {
    pub fn initialize(env: Env, admin: Address, escrow: Address) {
        if env.storage().instance().has(&symbol_short!("ADMIN")) {
            panic_with_error!(&env, HarvestaError::AlreadyInitialized);
        }
        env.storage().instance().set(&symbol_short!("ADMIN"), &admin);
        env.storage().instance().set(&symbol_short!("ESCROW"), &escrow);
        env.storage().instance().set(&symbol_short!("TREECOUNT"), &0u64);
        env.storage().instance().set(&symbol_short!("PAUSED"), &false);
        env.storage().instance().set(&symbol_short!("VERIFIERS"), &Vec::<Address>::new(&env));
    }

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
            notes_hash: None,
            milestone_claims: 0,
        };

        env.storage().persistent().set(&Self::tree_key(env, tree_id), &record);
        Self::extend_ttl(env, &Self::tree_key(env, tree_id));

        env.storage()
            .instance()
            .set(&symbol_short!("TREECOUNT"), &count.checked_add(1).expect("tree count overflow"));

        let mut sponsor_trees: Vec<u64> = env
            .storage()
            .persistent()
            .get(&Self::sponsor_key(env, &sponsor))
            .unwrap_or_else(|| Vec::new(&env));
        sponsor_trees.push_back(tree_id);
        env.storage().persistent().set(&Self::sponsor_key(env, &sponsor), &sponsor_trees);
        Self::extend_ttl(env, &Self::sponsor_key(env, &sponsor));

        let mut species_list: Vec<soroban_sdk::String> = env
            .storage()
            .instance()
            .get(&Self::species_list_key(env))
            .unwrap_or_else(|| Vec::new(&env));

        if !species_list.contains(&species) {
            species_list.push_back(species.clone());
            env.storage().instance().set(&Self::species_list_key(env), &species_list);
        }

        let mut species_trees: Vec<u64> = env
            .storage()
            .persistent()
            .get(&Self::species_trees_key(env, &species))
            .unwrap_or_else(|| Vec::new(&env));
        species_trees.push_back(tree_id);
        env.storage().persistent().set(&Self::species_trees_key(env, &species), &species_trees);
        Self::extend_ttl(env, &Self::species_trees_key(env, &species));

        let mut species_region_trees: Vec<u64> = env
            .storage()
            .persistent()
            .get(&Self::species_region_key(env, &species, &region))
            .unwrap_or_else(|| Vec::new(&env));
        species_region_trees.push_back(tree_id);
        env.storage().persistent().set(&Self::species_region_key(env, &species, &region), &species_region_trees);
        Self::extend_ttl(env, &Self::species_region_key(env, &species, &region));

        let mut species_status_trees: Vec<u64> = env
            .storage()
            .persistent()
            .get(&Self::species_status_key(env, &species, &TreeStatus::Planted))
            .unwrap_or_else(|| Vec::new(&env));
        species_status_trees.push_back(tree_id);
        env.storage().persistent().set(&Self::species_status_key(env, &species, &TreeStatus::Planted), &species_status_trees);
        Self::extend_ttl(env, &Self::species_status_key(env, &species, &TreeStatus::Planted));

        let mut region_species: Vec<soroban_sdk::String> = env
            .storage()
            .persistent()
            .get(&Self::region_species_key(env, &region))
            .unwrap_or_else(|| Vec::new(&env));
        if !region_species.contains(&species) {
            region_species.push_back(species.clone());
        }
        env.storage().persistent().set(&Self::region_species_key(env, &region), &region_species);
        Self::extend_ttl(env, &Self::region_species_key(env, &region));

        env.events().publish(
            (Symbol::new(&env, "TreeMinted"), tree_id),
            (sponsor, species, region, planter),
        );

        tree_id
    }

    pub fn add_verifier(env: Env, verifier: Address) {
        Self::require_admin(&env);
        let mut verifiers: Vec<Address> = env
            .storage()
            .instance()
            .get(&symbol_short!("VERIFIERS"))
            .unwrap_or_else(|| Vec::new(&env));
        if !verifiers.contains(&verifier) {
            verifiers.push_back(verifier.clone());
            env.storage().instance().set(&symbol_short!("VERIFIERS"), &verifiers);
            env.events().publish((Symbol::new(&env, "VerifierAdded"),), verifier);
        }
    }

    pub fn remove_verifier(env: Env, verifier: Address) {
        Self::require_admin(&env);
        let verifiers: Vec<Address> = env
            .storage()
            .instance()
            .get(&symbol_short!("VERIFIERS"))
            .unwrap_or_else(|| Vec::new(&env));
        let mut new_verifiers = Vec::new(&env);
        for v in verifiers.iter() {
            if v != verifier {
                new_verifiers.push_back(v.clone());
            }
        }
        env.storage().instance().set(&symbol_short!("VERIFIERS"), &new_verifiers);
        env.events().publish((Symbol::new(&env, "VerifierRemoved"),), verifier);
    }

    pub fn get_verifiers(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&symbol_short!("VERIFIERS"))
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub fn get_planter_score(env: Env, planter: Address) -> u64 {
        env.storage()
            .persistent()
            .get(&Self::planter_score_key(env, &planter))
            .unwrap_or(0)
    }

    pub fn verify_tree(
        env: Env,
        verifier: Address,
        tree_id: u64,
        approved: bool,
        notes_hash: Option<soroban_sdk::String>,
    ) {
        Self::assert_not_paused(&env);
        Self::require_verifier(&env, &verifier);

        let tree_key = Self::tree_key(env, tree_id);
        let mut tree_record: TreeRecord = env
            .storage()
            .persistent()
            .get(&tree_key)
            .unwrap_or_else(|| panic_with_error!(&env, TreeRegistryError::NotFound));

        if tree_record.status != TreeStatus::Planted {
            panic_with_error!(&env, TreeRegistryError::InvalidStatus);
        }

        tree_record.notes_hash = notes_hash.clone();

        if approved {
            tree_record.status = TreeStatus::Verified;

            let score_key = Self::planter_score_key(env, &tree_record.planter);
            let current_score: u64 = env.storage().persistent().get(&score_key).unwrap_or(0);
            env.storage().persistent().set(&score_key, &(current_score + 1));
            Self::extend_ttl(env, &score_key);

            let escrow: Address = env
                .storage()
                .instance()
                .get(&symbol_short!("ESCROW"))
                .unwrap();

            #[allow(dead_code)]
            #[contractclient(name = "EscrowClient")]
            trait EscrowTrait {
                fn release(env: Env, tree_id: u64);
            }

            let escrow_client = EscrowClient::new(&env, &escrow);
            escrow_client.release(&tree_id);

            env.events().publish(
                (Symbol::new(&env, "TreeVerified"), tree_id),
                (verifier, notes_hash),
            );
        } else {
            tree_record.status = TreeStatus::Rejected;

            env.events().publish(
                (Symbol::new(&env, "TreeRejected"), tree_id),
                (verifier, notes_hash),
            );
        }

        env.storage().persistent().set(&tree_key, &tree_record);
        Self::extend_ttl(env, &tree_key);
    }

    pub fn get_tree(env: Env, id: u64) -> Option<TreeRecord> {
        env.storage().persistent().get(&Self::tree_key(env, id))
    }

    pub fn list_by_sponsor(env: Env, sponsor: Address) -> Vec<TreeRecord> {
        let tree_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&Self::sponsor_key(env, &sponsor))
            .unwrap_or_else(|| Vec::new(&env));
        
        let mut records = Vec::new(&env);
        for id in tree_ids.iter() {
            if let Some(record) = env.storage().persistent().get(&Self::tree_key(env, id)) {
                records.push_back(record);
            }
        }
        records
    }

    pub fn claim_milestone(
        env: Env,
        sponsor: Address,
        tree_id: u64,
        milestone_years: u64,
    ) -> i128 {
        Self::assert_not_paused(&env);
        sponsor.require_auth();

        let tree_key = Self::tree_key(env, tree_id);
        let mut tree_record: TreeRecord = env
            .storage()
            .persistent()
            .get(&tree_key)
            .unwrap_or_else(|| panic_with_error!(&env, TreeRegistryError::NotFound));

        if tree_record.sponsor != sponsor {
            panic_with_error!(&env, TreeRegistryError::NotAuthorized);
        }
        if tree_record.status == TreeStatus::Rejected {
            panic_with_error!(&env, TreeRegistryError::InvalidStatus);
        }

        let flag = Self::milestone_flag(milestone_years)
            .unwrap_or_else(|| panic_with_error!(&env, TreeRegistryError::InvalidStatus));
        if tree_record.milestone_claims & flag != 0 {
            panic_with_error!(&env, TreeRegistryError::InvalidStatus);
        }

        let required_timestamp = tree_record
            .planted_at
            .checked_add(
                milestone_years
                    .checked_mul(ONE_YEAR_SECS)
                    .expect("milestone multiplication overflow"),
            )
            .expect("timestamp overflow");

        if env.ledger().timestamp() < required_timestamp {
            panic_with_error!(&env, TreeRegistryError::InvalidStatus);
        }

        tree_record.milestone_claims |= flag;
        if tree_record.milestone_claims == 0b111 {
            tree_record.status = TreeStatus::Matured;
        }

        env.storage().persistent().set(&tree_key, &tree_record);
        Self::extend_ttl(env, &tree_key);

        let amount = Self::co2_credits_for_years(milestone_years);
        env.events().publish(
            (Symbol::new(&env, "MilestoneClaimed"), tree_id),
            (sponsor, milestone_years, amount),
        );

        amount
    }

    pub fn tree_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&symbol_short!("TREECOUNT"))
            .unwrap_or(0)
    }

    pub fn register_species(env: Env, slug: Symbol, co2_scaled: i128, maturity_years: u32) {
        Self::require_admin(&env);

        if co2_scaled <= 0 {
            panic_with_error!(&env, HarvestaError::Co2MustBePositive);
        }
        if maturity_years == 0 {
            panic_with_error!(&env, HarvestaError::MaturityYearsMustBePositive);
        }

        let existing: Option<SpeciesInfo> = env
            .storage()
            .persistent()
            .get(&Self::species_info_key(env, &slug));
        if existing.is_some() {
            panic_with_error!(&env, TreeRegistryError::SpeciesAlreadyExists);
        }

        let info = SpeciesInfo {
            co2_scaled,
            maturity_years,
            updated_at: env.ledger().timestamp(),
        };

        env.storage().persistent().set(&Self::species_info_key(env, &slug), &info);
        Self::extend_ttl(env, &Self::species_info_key(env, &slug));

        env.events().publish(
            (symbol_short!("species"), symbol_short!("register")),
            (slug, co2_scaled, maturity_years),
        );
    }

    pub fn update_species(env: Env, slug: Symbol, co2_scaled: i128, maturity_years: u32) {
        Self::require_admin(&env);

        if co2_scaled <= 0 {
            panic_with_error!(&env, HarvestaError::Co2MustBePositive);
        }
        if maturity_years == 0 {
            panic_with_error!(&env, HarvestaError::MaturityYearsMustBePositive);
        }

        let _existing: SpeciesInfo = env
            .storage()
            .persistent()
            .get(&Self::species_info_key(env, &slug))
            .unwrap_or_else(|| panic_with_error!(&env, TreeRegistryError::SpeciesNotFound));

        let updated = SpeciesInfo {
            co2_scaled,
            maturity_years,
            updated_at: env.ledger().timestamp(),
        };

        env.storage().persistent().set(&Self::species_info_key(env, &slug), &updated);
        Self::extend_ttl(env, &Self::species_info_key(env, &slug));

        env.events().publish(
            (symbol_short!("species"), symbol_short!("update")),
            (slug, co2_scaled, maturity_years),
        );
    }

    pub fn get_species_info(env: Env, slug: Symbol) -> Option<SpeciesInfo> {
        env.storage()
            .persistent()
            .get(&Self::species_info_key(env, &slug))
    }

    pub fn unregister_species(env: Env, slug: Symbol) {
        Self::require_admin(&env);

        let slug_str = Self::symbol_to_string(env, &slug);
        let has_trees: bool = env
            .storage()
            .persistent()
            .get(&Self::species_trees_key(env, &slug_str))
            .map(|v: Vec<u64>| !v.is_empty())
            .unwrap_or(false);
        if has_trees {
            panic_with_error!(&env, TreeRegistryError::InvalidStatus);
        }

        env.storage()
            .persistent()
            .remove(&Self::species_info_key(env, &slug));

        env.events().publish(
            (symbol_short!("species"), symbol_short!("unregister")),
            slug,
        );
    }

    pub fn get_distinct_species(env: Env) -> Vec<soroban_sdk::String> {
        env.storage()
            .instance()
            .get(&Self::species_list_key(env))
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub fn get_tree_ids_by_species(env: Env, species: soroban_sdk::String) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&Self::species_trees_key(env, &species))
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub fn get_species_count(env: Env, species: soroban_sdk::String) -> u64 {
        env.storage()
            .persistent()
            .get(&Self::species_trees_key(env, &species))
            .map(|v: Vec<u64>| v.len() as u64)
            .unwrap_or(0u64)
    }

    pub fn get_tree_ids_by_species_and_region(
        env: Env,
        species: soroban_sdk::String,
        region: soroban_sdk::String,
    ) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&Self::species_region_key(env, &species, &region))
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub fn get_tree_ids_by_species_and_status(
        env: Env,
        species: soroban_sdk::String,
        status: TreeStatus,
    ) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&Self::species_status_key(env, &species, &status))
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub fn get_species_in_region(env: Env, region: soroban_sdk::String) -> Vec<soroban_sdk::String> {
        env.storage()
            .persistent()
            .get(&Self::region_species_key(env, &region))
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub fn pause(env: Env) {
        Self::require_admin(&env);
        env.storage()
            .instance()
            .set(&symbol_short!("PAUSED"), &true);
        env.events()
            .publish((symbol_short!("paused"),), env.ledger().timestamp());
    }

    pub fn unpause(env: Env) {
        Self::require_admin(&env);
        env.storage()
            .instance()
            .set(&symbol_short!("PAUSED"), &false);
        env.events()
            .publish((symbol_short!("unpaused"),), env.ledger().timestamp());
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&symbol_short!("PAUSED"))
            .unwrap_or(false)
    }

    fn tree_key(env: &Env, id: u64) -> (Symbol, u64) {
        (symbol_short!("TREE"), id)
    }

    fn sponsor_key(env: &Env, sponsor: &Address) -> (Symbol, Address) {
        (symbol_short!("SPONSOR"), sponsor.clone())
    }

    fn planter_score_key(env: &Env, planter: &Address) -> (Symbol, Address) {
        (symbol_short!("SCORE"), planter.clone())
    }

    fn species_list_key(env: &Env) -> Symbol {
        Symbol::new(env, "SPLIST")
    }

    fn species_trees_key(env: &Env, species: &soroban_sdk::String) -> (Symbol, soroban_sdk::String) {
        (symbol_short!("SPTREES"), species.clone())
    }

    fn species_info_key(env: &Env, slug: &Symbol) -> (Symbol, Symbol) {
        (symbol_short!("SPINFO"), slug.clone())
    }

    fn species_region_key(env: &Env, species: &soroban_sdk::String, region: &soroban_sdk::String) -> (Symbol, soroban_sdk::String, soroban_sdk::String) {
        (symbol_short!("SPRGN"), species.clone(), region.clone())
    }

    fn species_status_key(env: &Env, species: &soroban_sdk::String, status: &TreeStatus) -> (Symbol, soroban_sdk::String, TreeStatus) {
        (symbol_short!("SPSTAT"), species.clone(), status.clone())
    }

    fn region_species_key(env: &Env, region: &soroban_sdk::String) -> (Symbol, soroban_sdk::String) {
        (symbol_short!("RGLST"), region.clone())
    }

    fn milestone_flag(milestone_years: u64) -> Option<u32> {
        match milestone_years {
            1 => Some(1),
            5 => Some(2),
            10 => Some(4),
            _ => None,
        }
    }

    fn co2_credits_for_years(years: u64) -> i128 {
        CO2_KG_PER_YEAR
            .checked_mul(i128::from(years))
            .expect("CO2 credit overflow")
    }

    fn extend_ttl(env: &Env, key: &(impl IntoVal<Env, Val> + ?Sized)) {
        env.storage()
            .persistent()
            .extend_ttl(key, DEFAULT_PERSISTENT_TTL, DEFAULT_PERSISTENT_TTL);
    }

    fn symbol_to_string(env: &Env, symbol: &Symbol) -> soroban_sdk::String {
        let s: &str = &symbol.clone().into_string();
        soroban_sdk::String::from_str(env, s)
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

    fn require_verifier(env: &Env, verifier: &Address) {
        verifier.require_auth();
        let verifiers: Vec<Address> = env
            .storage()
            .instance()
            .get(&symbol_short!("VERIFIERS"))
            .unwrap_or_else(|| Vec::new(env));
        if !verifiers.contains(verifier) {
            panic_with_error!(env, TreeRegistryError::NotAuthorized);
        }
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
