#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, Env, IntoVal, Symbol, Vec,
};

/// 90 days in seconds
const REFUND_WINDOW: u64 = 90 * 24 * 60 * 60;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum EscrowError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    AmountMustBePositive = 3,
    EscrowAlreadyFunded = 4,
    EscrowNotFound = 5,
    EscrowAlreadySettled = 6,
    RefundWindowNotOpen = 7,
    InsufficientDonation = 8,
    NoPlantersAvailable = 9,
    InvalidSpecies = 10,
    TreeRegistryNotSet = 11,
    PlanterRegistryNotSet = 12,
    TreeMintingFailed = 13,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum EscrowStatus {
    Pending,
    Released,
    Refunded,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowRecord {
    pub sponsor: Option<Address>,
    pub planter: Address,
    pub token: Address,
    pub amount: i128,
    pub deposit_time: u64,
    pub status: EscrowStatus,
    pub species: Option<Symbol>,
    pub region: Option<Symbol>,
    pub is_anonymous: bool,
}

#[contract]
pub struct Escrow;

#[contractimpl]
impl Escrow {
    pub fn initialize(env: Env, verifier: Address) {
        if env.storage().instance().has(&symbol_short!("VERIFIER")) {
            panic_with_error!(&env, EscrowError::AlreadyInitialized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("VERIFIER"), &verifier);
    }

    pub fn initialize_registries(
        env: Env,
        tree_registry: Address,
        planter_registry: Address,
    ) {
        Self::require_verifier(&env);
        env.storage()
            .instance()
            .set(&symbol_short!("TREE_REG"), &tree_registry);
        env.storage()
            .instance()
            .set(&symbol_short!("PLANT_REG"), &planter_registry);
    }

    pub fn deposit(
        env: Env,
        sponsor: Address,
        planter: Address,
        tree_id: u64,
        token: Address,
        amount: i128,
    ) {
        sponsor.require_auth();
        if amount <= 0 {
            panic_with_error!(&env, EscrowError::AmountMustBePositive);
        }
        let key = Self::escrow_key(&env, tree_id);
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, EscrowError::EscrowAlreadyFunded);
        }
        token::Client::new(&env, &token).transfer(
            &sponsor,
            &env.current_contract_address(),
            &amount,
        );
        env.storage().persistent().set(
            &key,
            &EscrowRecord {
                sponsor: Some(sponsor.clone()),
                planter,
                token: token.clone(),
                amount,
                deposit_time: env.ledger().timestamp(),
                status: EscrowStatus::Pending,
                species: None,
                region: None,
                is_anonymous: false,
            },
        );
        env.events().publish(
            (symbol_short!("FundsDep"), tree_id),
            (sponsor, token, amount),
        );
    }

    pub fn donate_anonymous(
        env: Env,
        amount: i128,
        token: Address,
        species: Symbol,
        region: Symbol,
    ) -> (u64, Address) {
        let species_cost = Self::get_species_cost(&env, species);
        if amount < species_cost {
            panic_with_error!(&env, EscrowError::InsufficientDonation);
        }
        let planter = Self::assign_planter(&env, region);
        token::Client::new(&env, &token).transfer(
            &env.invoker(),
            &env.current_contract_address(),
            &amount,
        );
        let tree_id = Self::mint_anonymous_tree(&env, species, region, planter.clone());
        let key = Self::escrow_key(&env, tree_id);
        env.storage().persistent().set(
            &key,
            &EscrowRecord {
                sponsor: None,
                planter: planter.clone(),
                token: token.clone(),
                amount,
                deposit_time: env.ledger().timestamp(),
                status: EscrowStatus::Pending,
                species: Some(species),
                region: Some(region),
                is_anonymous: true,
            },
        );
        Self::increment_planter_workload(&env, planter.clone());
        env.events().publish(
            (symbol_short!("AnonDep"), tree_id),
            (species, region, amount, token, planter.clone()),
        );
        (tree_id, planter)
    }

    pub fn release(env: Env, tree_id: u64) {
        Self::require_verifier(&env);
        let key = Self::escrow_key(&env, tree_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::EscrowNotFound));
        if record.status != EscrowStatus::Pending {
            panic_with_error!(&env, EscrowError::EscrowAlreadySettled);
        }
        token::Client::new(&env, &record.token).transfer(
            &env.current_contract_address(),
            &record.planter,
            &record.amount,
        );
        record.status = EscrowStatus::Released;
        env.storage().persistent().set(&key, &record);
        if record.is_anonymous {
            Self::decrement_planter_workload(&env, record.planter.clone());
        }
        env.events().publish(
            (symbol_short!("FundsRel"), tree_id),
            (record.planter, record.amount),
        );
    }

    pub fn refund(env: Env, tree_id: u64) {
        let key = Self::escrow_key(&env, tree_id);
        let mut record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::EscrowNotFound));
        if record.status != EscrowStatus::Pending {
            panic_with_error!(&env, EscrowError::EscrowAlreadySettled);
        }
        let sponsor = record.sponsor.clone().unwrap_or_else(|| {
            panic_with_error!(&env, EscrowError::EscrowAlreadySettled);
        });
        sponsor.require_auth();
        let elapsed = env.ledger().timestamp().saturating_sub(record.deposit_time);
        if elapsed < REFUND_WINDOW {
            panic_with_error!(&env, EscrowError::RefundWindowNotOpen);
        }
        token::Client::new(&env, &record.token).transfer(
            &env.current_contract_address(),
            &sponsor,
            &record.amount,
        );
        record.status = EscrowStatus::Refunded;
        env.storage().persistent().set(&key, &record);
        env.events().publish(
            (symbol_short!("FundsRef"), tree_id),
            (sponsor, record.amount),
        );
    }

    pub fn get_escrow(env: Env, tree_id: u64) -> Option<EscrowRecord> {
        env.storage()
            .persistent()
            .get(&Self::escrow_key(&env, tree_id))
    }

    pub fn get_species_cost(env: Env, species: Symbol) -> i128 {
        if species == symbol_short!("teak") { 50_0000000i128 }
        else if species == symbol_short!("moringa") { 10_0000000i128 }
        else if species == symbol_short!("eucalyptus") { 35_0000000i128 }
        else if species == symbol_short!("mangrove") { 25_0000000i128 }
        else if species == symbol_short!("acacia") { 15_0000000i128 }
        else if species == symbol_short!("bamboo") { 8_0000000i128 }
        else { panic_with_error!(&env, EscrowError::InvalidSpecies); }
    }

    fn assign_planter(env: &Env, region: Symbol) -> Address {
        let planter_registry: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("PLANT_REG"))
            .unwrap_or_else(|| panic_with_error!(env, EscrowError::PlanterRegistryNotSet));
        let planters: Vec<Address> = env.invoke_contract(
            &planter_registry,
            &symbol_short!("get_avail"),
            Vec::from_array(env, [region.into_val(env)]),
        );
        if planters.is_empty() {
            panic_with_error!(env, EscrowError::NoPlantersAvailable);
        }
        planters.get(0).unwrap()
    }

    fn mint_anonymous_tree(env: &Env, species: Symbol, region: Symbol, planter: Address) -> u64 {
        let tree_registry: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("TREE_REG"))
            .unwrap_or_else(|| panic_with_error!(env, EscrowError::TreeRegistryNotSet));
        env.invoke_contract(
            &tree_registry,
            &symbol_short!("mint_anon"),
            Vec::from_array(env, [
                species.into_val(env),
                region.into_val(env),
                planter.into_val(env),
            ]),
        )
    }

    fn increment_planter_workload(env: &Env, planter: Address) {
        let planter_registry: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("PLANT_REG"))
            .unwrap_or_else(|| panic_with_error!(env, EscrowError::PlanterRegistryNotSet));
        env.invoke_contract(
            &planter_registry,
            &symbol_short!("inc_work"),
            Vec::from_array(env, [planter.into_val(env)]),
        );
    }

    fn decrement_planter_workload(env: &Env, planter: Address) {
        let planter_registry: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("PLANT_REG"))
            .unwrap_or_else(|| panic_with_error!(env, EscrowError::PlanterRegistryNotSet));
        env.invoke_contract(
            &planter_registry,
            &symbol_short!("dec_work"),
            Vec::from_array(env, [planter.into_val(env)]),
        );
    }

    fn escrow_key(env: &Env, tree_id: u64) -> soroban_sdk::Val {
        (symbol_short!("ESC"), tree_id).into_val(env)
    }

    fn require_verifier(env: &Env) {
        let verifier: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("VERIFIER"))
            .unwrap_or_else(|| panic_with_error!(env, EscrowError::NotInitialized));
        verifier.require_auth();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger as _},
        token, vec, Address, Env,
    };

    fn setup() -> (Env, Address, Address, Address, Address, EscrowClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, Escrow);
        let client = EscrowClient::new(&env, &contract_id);
        let verifier = Address::generate(&env);
        let sponsor = Address::generate(&env);
        let planter = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token = env.register_stellar_asset_contract(token_admin.clone());
        token::StellarAssetClient::new(&env, &token).mint(&sponsor, &1_000_000);
        client.initialize(&verifier);
        (env, verifier, sponsor, planter, token, client)
    }

    fn setup_with_registries() -> (Env, Address, Address, Address, Address, Address, Address, EscrowClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, Escrow);
        let client = EscrowClient::new(&env, &contract_id);
        let verifier = Address::generate(&env);
        let sponsor = Address::generate(&env);
        let planter = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let tree_registry = Address::generate(&env);
        let planter_registry = Address::generate(&env);
        let token = env.register_stellar_asset_contract(token_admin.clone());
        token::StellarAssetClient::new(&env, &token).mint(&sponsor, &1_000_000_000);
        client.initialize(&verifier);
        client.initialize_registries(&tree_registry, &planter_registry);
        (env, verifier, sponsor, planter, token, tree_registry, planter_registry, client)
    }

    #[test]
    fn test_deposit_stores_record() {
        let (_env, _verifier, sponsor, planter, token, client) = setup();
        client.deposit(&sponsor, &planter, &1u64, &token, &10_000);
        let rec = client.get_escrow(&1u64).unwrap();
        assert_eq!(rec.amount, 10_000);
        assert_eq!(rec.sponsor, Some(sponsor));
        assert_eq!(rec.planter, planter);
        assert_eq!(rec.status, EscrowStatus::Pending);
        assert!(!rec.is_anonymous);
    }

    #[test]
    fn test_release_transfers_to_planter() {
        let (env, _verifier, sponsor, planter, token, client) = setup();
        client.deposit(&sponsor, &planter, &1u64, &token, &10_000);
        let before = token::Client::new(&env, &token).balance(&planter);
        client.release(&1u64);
        let after = token::Client::new(&env, &token).balance(&planter);
        assert_eq!(after - before, 10_000);
        let rec = client.get_escrow(&1u64).unwrap();
        assert_eq!(rec.status, EscrowStatus::Released);
    }

    #[test]
    fn test_refund_after_90_days_returns_to_sponsor() {
        let (env, _verifier, sponsor, planter, token, client) = setup();
        client.deposit(&sponsor, &planter, &1u64, &token, &10_000);
        env.ledger().with_mut(|l| l.timestamp += REFUND_WINDOW + 1);
        let before = token::Client::new(&env, &token).balance(&sponsor);
        client.refund(&1u64);
        let after = token::Client::new(&env, &token).balance(&sponsor);
        assert_eq!(after - before, 10_000);
        let rec = client.get_escrow(&1u64).unwrap();
        assert_eq!(rec.status, EscrowStatus::Refunded);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #7)")]
    fn test_refund_before_90_days_panics() {
        let (env, _verifier, sponsor, planter, token, client) = setup();
        client.deposit(&sponsor, &planter, &1u64, &token, &10_000);
        env.ledger().with_mut(|l| l.timestamp += REFUND_WINDOW - 1);
        client.refund(&1u64);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #4)")]
    fn test_double_deposit_rejected() {
        let (_env, _verifier, sponsor, planter, token, client) = setup();
        client.deposit(&sponsor, &planter, &1u64, &token, &10_000);
        client.deposit(&sponsor, &planter, &1u64, &token, &5_000);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #6)")]
    fn test_release_twice_panics() {
        let (_env, _verifier, sponsor, planter, token, client) = setup();
        client.deposit(&sponsor, &planter, &1u64, &token, &10_000);
        client.release(&1u64);
        client.release(&1u64);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #6)")]
    fn test_refund_after_release_panics() {
        let (env, _verifier, sponsor, planter, token, client) = setup();
        client.deposit(&sponsor, &planter, &1u64, &token, &10_000);
        client.release(&1u64);
        env.ledger().with_mut(|l| l.timestamp += REFUND_WINDOW + 1);
        client.refund(&1u64);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #5)")]
    fn test_release_nonexistent_panics() {
        let (_env, _verifier, _sponsor, _planter, _token, client) = setup();
        client.release(&999u64);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #3)")]
    fn test_zero_amount_rejected() {
        let (_env, _verifier, sponsor, planter, token, client) = setup();
        client.deposit(&sponsor, &planter, &1u64, &token, &0);
    }

    #[test]
    fn test_different_tree_ids_are_independent() {
        let (_env, _verifier, sponsor, planter, token, client) = setup();
        client.deposit(&sponsor, &planter, &1u64, &token, &1_000);
        client.deposit(&sponsor, &planter, &2u64, &token, &2_000);
        client.release(&1u64);
        let rec1 = client.get_escrow(&1u64).unwrap();
        let rec2 = client.get_escrow(&2u64).unwrap();
        assert_eq!(rec1.status, EscrowStatus::Released);
        assert_eq!(rec2.status, EscrowStatus::Pending);
    }

    #[test]
    fn test_donate_anonymous_success() {
        let (env, _verifier, sponsor, _planter, token, tree_reg, plant_reg, client) = setup_with_registries();
        env.register_contract(&tree_reg, MockTreeRegistry);
        env.register_contract(&plant_reg, MockPlanterRegistry);
        let amount = 50_0000000i128;
        token::Client::new(&env, &token).approve(&sponsor, &client.address, &amount, &999999);
        let (tree_id, assigned_planter) = client.donate_anonymous(&amount, &token, &symbol_short!("teak"), &symbol_short!("kenya"));
        assert_eq!(tree_id, 1u64);
        let rec = client.get_escrow(&tree_id).unwrap();
        assert_eq!(rec.sponsor, None);
        assert_eq!(rec.amount, amount);
        assert_eq!(rec.species, Some(symbol_short!("teak")));
        assert_eq!(rec.region, Some(symbol_short!("kenya")));
        assert!(rec.is_anonymous);
        assert_eq!(rec.status, EscrowStatus::Pending);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #8)")]
    fn test_donate_anonymous_insufficient_funds() {
        let (env, _verifier, _sponsor, _planter, token, tree_reg, plant_reg, client) = setup_with_registries();
        env.register_contract(&tree_reg, MockTreeRegistry);
        env.register_contract(&plant_reg, MockPlanterRegistry);
        let amount = 5_0000000i128;
        client.donate_anonymous(&amount, &token, &symbol_short!("teak"), &symbol_short!("kenya"));
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #9)")]
    fn test_donate_anonymous_no_planters() {
        let (env, _verifier, _sponsor, _planter, token, tree_reg, plant_reg, client) = setup_with_registries();
        env.register_contract(&tree_reg, MockTreeRegistry);
        env.register_contract(&plant_reg, MockEmptyPlanterRegistry);
        let amount = 50_0000000i128;
        client.donate_anonymous(&amount, &token, &symbol_short!("teak"), &symbol_short!("antarctica"));
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #10)")]
    fn test_donate_anonymous_invalid_species() {
        let (env, _verifier, _sponsor, _planter, token, tree_reg, plant_reg, client) = setup_with_registries();
        env.register_contract(&tree_reg, MockTreeRegistry);
        env.register_contract(&plant_reg, MockPlanterRegistry);
        let amount = 50_0000000i128;
        client.donate_anonymous(&amount, &token, &symbol_short!("alien"), &symbol_short!("kenya"));
    }

    #[test]
    fn test_species_costs() {
        let (_env, _verifier, _sponsor, _planter, _token, _tree_reg, _plant_reg, client) = setup_with_registries();
        assert_eq!(client.get_species_cost(&symbol_short!("teak")), 50_0000000i128);
        assert_eq!(client.get_species_cost(&symbol_short!("moringa")), 10_0000000i128);
        assert_eq!(client.get_species_cost(&symbol_short!("eucalyptus")), 35_0000000i128);
        assert_eq!(client.get_species_cost(&symbol_short!("mangrove")), 25_0000000i128);
        assert_eq!(client.get_species_cost(&symbol_short!("acacia")), 15_0000000i128);
        assert_eq!(client.get_species_cost(&symbol_short!("bamboo")), 8_0000000i128);
    }

    #[test]
    fn test_anonymous_release_works() {
        let (env, _verifier, sponsor, planter, token, tree_reg, plant_reg, client) = setup_with_registries();
        env.register_contract(&tree_reg, MockTreeRegistry);
        env.register_contract(&plant_reg, MockPlanterRegistry);
        let amount = 50_0000000i128;
        token::Client::new(&env, &token).approve(&sponsor, &client.address, &amount, &999999);
        let (tree_id, _) = client.donate_anonymous(&amount, &token, &symbol_short!("teak"), &symbol_short!("kenya"));
        let before = token::Client::new(&env, &token).balance(&planter);
        client.release(&tree_id);
        let after = token::Client::new(&env, &token).balance(&planter);
        assert_eq!(after - before, amount);
        let rec = client.get_escrow(&tree_id).unwrap();
        assert_eq!(rec.status, EscrowStatus::Released);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #6)")]
    fn test_anonymous_refund_rejected() {
        let (env, _verifier, sponsor, _planter, token, tree_reg, plant_reg, client) = setup_with_registries();
        env.register_contract(&tree_reg, MockTreeRegistry);
        env.register_contract(&plant_reg, MockPlanterRegistry);
        let amount = 50_0000000i128;
        token::Client::new(&env, &token).approve(&sponsor, &client.address, &amount, &999999);
        let (tree_id, _) = client.donate_anonymous(&amount, &token, &symbol_short!("teak"), &symbol_short!("kenya"));
        env.ledger().with_mut(|l| l.timestamp += REFUND_WINDOW + 1);
        client.refund(&tree_id);
    }

    use soroban_sdk::{contract, contractimpl};

    #[contract]
    pub struct MockPlanterRegistry;
    #[contractimpl]
    impl MockPlanterRegistry {
        pub fn get_avail(env: Env, _region: Symbol) -> Vec<Address> {
            vec![&env, Address::generate(&env)]
        }
        pub fn inc_work(_env: Env, _planter: Address) {}
        pub fn dec_work(_env: Env, _planter: Address) {}
    }

    #[contract]
    pub struct MockEmptyPlanterRegistry;
    #[contractimpl]
    impl MockEmptyPlanterRegistry {
        pub fn get_avail(env: Env, _region: Symbol) -> Vec<Address> {
            vec![&env]
        }
    }

    #[contract]
    pub struct MockTreeRegistry;
    #[contractimpl]
    impl MockTreeRegistry {
        pub fn mint_anon(_env: Env, _species: Symbol, _region: Symbol, _planter: Address) -> u64 {
            1u64
        }
    }
}