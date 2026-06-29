#![no_std]

//! NFT Certificate Contract
//!
//! SEP-41 style non-fungible token for donor impact certificates.
//! Each certificate records immutable on-chain metadata: tree count, CO2 offset,
//! planting date, and region. The contract enforces positive impact values and
//! rejects planting dates set in the future.
//!
//! # Storage layout
//!   Instance:
//!     ADMIN  — Address (contract admin)
//!     NAME   — String  (token collection name)
//!     SYMBOL — String  (token collection symbol)
//!   Persistent (keyed by token_id):
//!     TokenData — owner, approved operator, metadata URI

use harvesta_errors::HarvestaError;
use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, Address, Env, String, Symbol,
};

// ── Types ─────────────────────────────────────────────────────────────────────

/// On-chain data stored for each certificate NFT.
#[contracttype]
#[derive(Clone, Debug)]
pub struct TokenData {
    /// Current owner of the certificate
    pub owner: Address,
    /// Approved operator, if any
    pub approved: Option<Address>,
    /// URI pointing to the off-chain JSON metadata
    pub uri: String,
}

/// Storage keys.
#[contracttype]
#[derive(Clone, Debug)]
pub enum DataKey {
    /// Contract admin address
    Admin,
    /// Collection name
    Name,
    /// Collection symbol
    Symbol,
    /// Token data by token id
    Token(String),
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct NftCertificate;

#[contractimpl]
impl NftCertificate {
    /// One-time initialisation.
    ///
    /// `admin`  — address authorized to mint certificates
    /// `name`   — collection name
    /// `symbol` — collection symbol
    pub fn initialize(env: Env, admin: Address, name: String, symbol: String) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, HarvestaError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Name, &name);
        env.storage().instance().set(&DataKey::Symbol, &symbol);

        env.events().publish(
            (Symbol::new(&env, "Initialize"), admin),
            (name, symbol),
        );
    }

    /// Mint a new certificate NFT to `recipient`.
    ///
    /// Validates impact metadata on-chain before minting:
    /// - `tree_count` must be > 0
    /// - `co2_offset_kg` must be > 0
    /// - `planting_date` must not be in the future
    ///
    /// `token_id` must be unique; duplicates are rejected with `TokenAlreadyMinted`.
    pub fn mint(
        env: Env,
        recipient: Address,
        token_id: String,
        metadata_uri: String,
        tree_count: u32,
        co2_offset_kg: u32,
        planting_date: u64,
    ) {
        Self::require_admin(&env);

        if env.storage().persistent().has(&DataKey::Token(token_id.clone())) {
            panic_with_error!(&env, HarvestaError::TokenAlreadyMinted);
        }

        // On-chain validation of certificate metadata.
        if tree_count == 0 {
            panic_with_error!(&env, HarvestaError::TreeCountMustBePositive);
        }
        if co2_offset_kg == 0 {
            panic_with_error!(&env, HarvestaError::Co2MustBePositive);
        }
        let now = env.ledger().timestamp();
        if planting_date > now {
            panic_with_error!(&env, HarvestaError::InvalidPlantingDate);
        }

        let token_data = TokenData {
            owner: recipient.clone(),
            approved: None,
            uri: metadata_uri.clone(),
        };
        env.storage()
            .persistent()
            .set(&DataKey::Token(token_id.clone()), &token_data);

        env.events().publish(
            (Symbol::new(&env, "CertificateMinted"), recipient),
            (token_id, metadata_uri, tree_count, co2_offset_kg, planting_date),
        );
    }

    /// Returns the owner of `token_id`, if it exists.
    pub fn owner_of(env: Env, token_id: String) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::Token(token_id))
            .map(|t: TokenData| t.owner)
    }

    /// Returns the metadata URI for `token_id`, if it exists.
    pub fn token_uri(env: Env, token_id: String) -> Option<String> {
        env.storage()
            .persistent()
            .get(&DataKey::Token(token_id))
            .map(|t: TokenData| t.uri)
    }

    /// Returns the approved operator for `token_id`, if any.
    pub fn get_approved(env: Env, token_id: String) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::Token(token_id))
            .and_then(|t: TokenData| t.approved)
    }

    /// Returns the collection name.
    pub fn name(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Name)
            .expect("not initialized")
    }

    /// Returns the collection symbol.
    pub fn symbol(env: Env) -> String {
        env.storage()
            .instance()
            .get(&DataKey::Symbol)
            .expect("not initialized")
    }

    /// Approve `operator` to manage `token_id`. Owner only.
    pub fn approve(env: Env, operator: Option<Address>, token_id: String) {
        let mut token: TokenData = env
            .storage()
            .persistent()
            .get(&DataKey::Token(token_id.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::TokenNotFound));

        token.owner.require_auth();
        token.approved = operator.clone();
        env.storage()
            .persistent()
            .set(&DataKey::Token(token_id.clone()), &token);

        env.events().publish(
            (Symbol::new(&env, "Approval"), token.owner, operator),
            token_id,
        );
    }

    /// Transfer `token_id` from `from` to `to`. Owner only.
    pub fn transfer(env: Env, to: Address, token_id: String) {
        let mut token: TokenData = env
            .storage()
            .persistent()
            .get(&DataKey::Token(token_id.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::TokenNotFound));

        token.owner.require_auth();
        let from = token.owner.clone();
        token.owner = to.clone();
        token.approved = None;
        env.storage()
            .persistent()
            .set(&DataKey::Token(token_id.clone()), &token);

        env.events().publish(
            (Symbol::new(&env, "Transfer"), from, to),
            token_id,
        );
    }

    /// Transfer `token_id` from `from` to `to` by an approved operator.
    pub fn transfer_from(env: Env, spender: Address, from: Address, to: Address, token_id: String) {
        let mut token: TokenData = env
            .storage()
            .persistent()
            .get(&DataKey::Token(token_id.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, HarvestaError::TokenNotFound));

        if token.owner != from {
            panic_with_error!(&env, HarvestaError::NotTokenOwner);
        }

        spender.require_auth();
        let is_approved = token
            .approved
            .as_ref()
            .map(|a| a == &spender)
            .unwrap_or(false);
        if token.owner != spender && !is_approved {
            panic_with_error!(&env, HarvestaError::NotTokenOwner);
        }

        token.owner = to.clone();
        token.approved = None;
        env.storage()
            .persistent()
            .set(&DataKey::Token(token_id.clone()), &token);

        env.events().publish(
            (Symbol::new(&env, "Transfer"), from, to),
            token_id,
        );
    }

    // ── Internal ────────────────────────────────────────────────────────────────

    fn require_admin(env: &Env) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, HarvestaError::NotInitialized));
        admin.require_auth();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Env, String};

    fn setup() -> (Env, Address, Address, NftCertificateClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, NftCertificate);
        let client = NftCertificateClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let recipient = Address::generate(&env);

        client.initialize(
            &admin,
            &String::from_str(&env, "Harvesta Impact Certificate"),
            &String::from_str(&env, "HIC"),
        );

        (env, admin, recipient, client)
    }

    fn token_id(env: &Env) -> String {
        String::from_str(env, "abc123")
    }

    fn uri(env: &Env) -> String {
        String::from_str(env, "https://example.com/metadata/abc123.json")
    }

    #[test]
    fn test_initialize() {
        let (env, _, _, client) = setup();
        assert_eq!(client.name(), String::from_str(&env, "Harvesta Impact Certificate"));
        assert_eq!(client.symbol(), String::from_str(&env, "HIC"));
    }

    #[test]
    fn test_mint_valid_certificate() {
        let (env, _, recipient, client) = setup();

        let id = token_id(&env);
        let metadata = uri(&env);
        let now = env.ledger().timestamp();

        client.mint(&recipient, &id, &metadata, &100, &4800, &now);

        assert_eq!(client.owner_of(&id), Some(recipient));
        assert_eq!(client.token_uri(&id), Some(metadata));
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #90)")]
    fn test_duplicate_token_rejected() {
        let (env, _, recipient, client) = setup();

        let id = token_id(&env);
        let metadata = uri(&env);
        let now = env.ledger().timestamp();

        client.mint(&recipient, &id, &metadata, &100, &4800, &now);
        client.mint(&recipient, &id, &metadata, &50, &2400, &now);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #10)")]
    fn test_zero_tree_count_rejected() {
        let (env, _, recipient, client) = setup();

        let id = token_id(&env);
        let metadata = uri(&env);
        let now = env.ledger().timestamp();

        client.mint(&recipient, &id, &metadata, &0, &4800, &now);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #62)")]
    fn test_zero_co2_offset_rejected() {
        let (env, _, recipient, client) = setup();

        let id = token_id(&env);
        let metadata = uri(&env);
        let now = env.ledger().timestamp();

        client.mint(&recipient, &id, &metadata, &100, &0, &now);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #91)")]
    fn test_future_planting_date_rejected() {
        let (env, _, recipient, client) = setup();

        let id = token_id(&env);
        let metadata = uri(&env);
        let now = env.ledger().timestamp();

        client.mint(&recipient, &id, &metadata, &100, &4800, &(now + 1));
    }

    #[test]
    fn test_transfer() {
        let (env, _, recipient, client) = setup();
        let new_owner = Address::generate(&env);

        let id = token_id(&env);
        let metadata = uri(&env);
        let now = env.ledger().timestamp();

        client.mint(&recipient, &id, &metadata, &100, &4800, &now);
        client.transfer(&new_owner, &id);

        assert_eq!(client.owner_of(&id), Some(new_owner));
    }
}
