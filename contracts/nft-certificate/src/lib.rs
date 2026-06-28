#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, symbol_short, Address, Env, String,
};
use harvesta_errors::HarvestaError;

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum DataKey {
    Admin,
    Owner(String),
    Metadata(String),
}

#[contract]
pub struct NftCertificate;

#[contractimpl]
impl NftCertificate {
    /// One-time initialization. Sets the contract admin.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, HarvestaError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    /// Mints a new Certificate_NFT assigned to the recipient with the provided token_id.
    /// If token_id already exists, panics with TOKEN_ALREADY_MINTED.
    pub fn mint(env: Env, recipient: Address, token_id: String, metadata_uri: String) {
        // Enforce uniqueness across all minted Certificate_NFTs
        let owner_key = DataKey::Owner(token_id.clone());
        if env.storage().persistent().has(&owner_key) {
            panic_with_error!(&env, HarvestaError::TokenAlreadyMinted);
        }

        // Store recipient as the owner and metadata_uri as the metadata for the token_id
        env.storage().persistent().set(&owner_key, &recipient);
        
        let metadata_key = DataKey::Metadata(token_id.clone());
        env.storage().persistent().set(&metadata_key, &metadata_uri);

        // Emit events for transparency / indexers
        env.events().publish(
            (symbol_short!("mint"), recipient.clone()),
            (token_id.clone(), metadata_uri),
        );
    }

    /// Returns the owner of the given token_id.
    pub fn owner_of(env: Env, token_id: String) -> Option<Address> {
        let key = DataKey::Owner(token_id);
        env.storage().persistent().get(&key)
    }

    /// Returns the metadata URI of the given token_id.
    pub fn metadata(env: Env, token_id: String) -> Option<String> {
        let key = DataKey::Metadata(token_id);
        env.storage().persistent().get(&key)
    }

    /// Transfers the ownership of the given token_id to a new recipient.
    /// Only the current owner can initiate this.
    pub fn transfer(env: Env, from: Address, to: Address, token_id: String) {
        from.require_auth();

        let owner_key = DataKey::Owner(token_id.clone());
        let current_owner: Address = env
            .storage()
            .persistent()
            .get(&owner_key)
            .unwrap_or_else(|| panic!("Token does not exist"));

        if current_owner != from {
            panic!("Sender is not the owner of this token");
        }

        env.storage().persistent().set(&owner_key, &to);

        // Emit transfer event
        env.events().publish(
            (symbol_short!("transfer"), from, to),
            token_id,
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Env, String};

    fn setup() -> (Env, Address, NftCertificateClient<'static>) {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, NftCertificate);
        let client = NftCertificateClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&admin);

        (env, admin, client)
    }

    #[test]
    fn test_initialize() {
        let (_env, admin, client) = setup();
        // Try double initialization
        let res = client.try_initialize(&admin);
        assert!(res.is_err());
    }

    #[test]
    fn test_mint_and_query() {
        let (env, _, client) = setup();
        let recipient = Address::generate(&env);
        let token_id = String::from_str(&env, "abcdef123456");
        let metadata_uri = String::from_str(&env, "ipfs://some-hash");

        client.mint(&recipient, &token_id, &metadata_uri);

        assert_eq!(client.owner_of(&token_id), Some(recipient.clone()));
        assert_eq!(client.metadata(&token_id), Some(metadata_uri));
    }

    #[test]
    fn test_cannot_mint_duplicate() {
        let (env, _, client) = setup();
        let recipient = Address::generate(&env);
        let token_id = String::from_str(&env, "abcdef123456");
        let metadata_uri = String::from_str(&env, "ipfs://some-hash");

        client.mint(&recipient, &token_id, &metadata_uri);

        let res = client.try_mint(&recipient, &token_id, &metadata_uri);
        assert!(res.is_err());
    }

    #[test]
    fn test_transfer() {
        let (env, _, client) = setup();
        let owner = Address::generate(&env);
        let recipient = Address::generate(&env);
        let token_id = String::from_str(&env, "abcdef123456");
        let metadata_uri = String::from_str(&env, "ipfs://some-hash");

        client.mint(&owner, &token_id, &metadata_uri);
        client.transfer(&owner, &recipient, &token_id);

        assert_eq!(client.owner_of(&token_id), Some(recipient));
    }
}
