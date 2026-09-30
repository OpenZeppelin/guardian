use anyhow::{Context, anyhow};
use miden_protocol::account::{
    Account, AccountBuilder, AccountId, AccountType, auth::PublicKeyCommitment,
};
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_standards::account::auth::{Approver, AuthSingleSig};
use miden_standards::account::wallets::BasicWallet;

use crate::manifest::NetworkName;

const TREASURY_KEY_ENV: &str = "QUAL_TREASURY_KEY";

/// The treasury is a plain single-signature wallet, deliberately not an account
/// of the kind this suite tests. A treasury of that kind would share a failure
/// mode with the subject: a regression in the proposal lifecycle would break
/// funding first, and every scenario would report a setup failure instead of
/// the defect.
pub struct Treasury {
    pub account: Account,
    pub network: NetworkName,
    secret_key: SecretKey,
}

impl Treasury {
    /// Rebuilds a treasury of the superseded private shape. Kept only so funds
    /// left in one can be swept into the public account that replaced it.
    pub fn legacy_private(secret_hex: &str, network: NetworkName) -> anyhow::Result<Self> {
        Self::build(secret_hex, network, AccountType::Private)
    }

    /// Rebuilds the treasury from its secret alone. The account is derived
    /// deterministically, so nothing about it has to be carried between runs.
    pub fn from_secret_hex(secret_hex: &str, network: NetworkName) -> anyhow::Result<Self> {
        Self::build(secret_hex, network, AccountType::Public)
    }

    fn build(
        secret_hex: &str,
        network: NetworkName,
        account_type: AccountType,
    ) -> anyhow::Result<Self> {
        let bytes = hex::decode(secret_hex.trim().trim_start_matches("0x"))
            .context("the treasury key is not hex")?;
        let secret_key = SecretKey::read_from_bytes(&bytes)
            .map_err(|error| anyhow!("the treasury key is not a Falcon key: {error}"))?;
        let account = build_wallet(&secret_key, account_type)?;
        Ok(Self {
            account,
            network,
            secret_key,
        })
    }

    pub fn from_env(network: NetworkName) -> anyhow::Result<Self> {
        let secret = std::env::var(TREASURY_KEY_ENV).map_err(|_| {
            anyhow!("{TREASURY_KEY_ENV} is not set; the live profile cannot fund anything")
        })?;
        Self::from_secret_hex(&secret, network)
    }

    pub fn generate(network: NetworkName) -> anyhow::Result<(Self, String)> {
        let secret_key = SecretKey::new();
        let secret_hex = hex::encode(secret_key.to_bytes());
        let treasury = Self::from_secret_hex(&secret_hex, network)?;
        Ok((treasury, secret_hex))
    }

    pub fn id(&self) -> AccountId {
        self.account.id()
    }

    pub fn secret_key(&self) -> &SecretKey {
        &self.secret_key
    }

    /// The bech32m form, which is what faucets accept.
    pub fn address(&self) -> String {
        self.account.id().to_bech32(self.network.to_network_id())
    }
}

fn build_wallet(secret_key: &SecretKey, account_type: AccountType) -> anyhow::Result<Account> {
    let commitment = PublicKeyCommitment::from(secret_key.public_key().to_commitment());
    let seed = account_seed(secret_key);
    // Public, so the node serves the full state. A private treasury's state
    // lives only in the local store, which a fresh CI runner does not have, and
    // the next transaction would then build on a stale nonce.
    AccountBuilder::new(seed)
        .account_type(account_type)
        .with_component(AuthSingleSig::new(Approver::new(
            commitment,
            miden_protocol::account::auth::AuthScheme::Falcon512Poseidon2,
        )))
        .with_component(BasicWallet)
        .build()
        .map_err(|error| anyhow!("cannot build the treasury wallet: {error}"))
}

/// Derived from the key so the same secret always yields the same account, and
/// nothing about the treasury has to be carried between runs.
fn account_seed(secret_key: &SecretKey) -> [u8; 32] {
    let bytes = secret_key.public_key().to_commitment().to_bytes();
    let mut seed = [0u8; 32];
    let len = seed.len().min(bytes.len());
    seed[..len].copy_from_slice(&bytes[..len]);
    seed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_secret_always_derives_the_same_account() {
        let (treasury, secret) = Treasury::generate(NetworkName::Testnet).expect("generates");
        let rebuilt = Treasury::from_secret_hex(&secret, NetworkName::Testnet).expect("rebuilds");
        assert_eq!(treasury.id(), rebuilt.id());
    }

    #[test]
    fn different_secrets_derive_different_accounts() {
        let (first, _) = Treasury::generate(NetworkName::Testnet).expect("generates");
        let (second, _) = Treasury::generate(NetworkName::Testnet).expect("generates");
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn a_malformed_secret_is_rejected() {
        assert!(Treasury::from_secret_hex("not-hex", NetworkName::Testnet).is_err());
    }

    #[test]
    fn the_address_is_network_scoped() {
        let (_, secret) = Treasury::generate(NetworkName::Testnet).expect("generates");
        let testnet = Treasury::from_secret_hex(&secret, NetworkName::Testnet).expect("t");
        let devnet = Treasury::from_secret_hex(&secret, NetworkName::Devnet).expect("d");
        assert_eq!(testnet.id(), devnet.id());
        assert_ne!(testnet.address(), devnet.address());
    }
}
