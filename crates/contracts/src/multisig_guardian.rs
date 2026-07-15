//! MultisigGuardian Account Builder
//!
//! High-level API for creating accounts with multisig + GUARDIAN authentication,
//! and the single source of truth for MultisigGuardian account creation.
//!
//! Builds the upstream `miden-standards` `AuthGuardedMultisig` component plus a
//! `BasicWallet`. The guardian is always present (no enable/disable selector) and
//! guardian-key rotation uses the account's default multisig threshold with no
//! current-guardian co-signature, matching `docs/CONCEPTS.md`.

use anyhow::{Result, anyhow};
use miden_protocol::Word;
use miden_protocol::account::auth::{AuthScheme, PublicKeyCommitment};
use miden_protocol::account::{
    Account, AccountBuilder, AccountComponent, AccountProcedureRoot, AccountType,
};
use miden_standards::account::auth::{
    AuthGuardedMultisig, AuthGuardedMultisigConfig, GuardianConfig,
};
use miden_standards::account::wallets::BasicWallet;

use guardian_shared::SignatureScheme;

/// Configuration for creating a MultisigGuardian account.
#[derive(Debug, Clone)]
pub struct MultisigGuardianConfig {
    /// The minimum number of signatures required to authorize a transaction.
    pub threshold: u32,
    /// Public key commitments of all signers (as Words).
    pub signer_commitments: Vec<Word>,
    /// GUARDIAN public key commitment.
    pub guardian_commitment: Word,
    /// Signature scheme for the account (Falcon or ECDSA).
    pub signature_scheme: SignatureScheme,
    /// Account type, which also determines on-chain storage visibility
    /// (`Private` keeps state off-chain; defaults to `Private`).
    pub account_type: AccountType,
    /// Optional procedure-specific threshold overrides (procedure root -> threshold).
    /// Guardian rotation deliberately carries no override and uses the default threshold.
    pub proc_threshold_overrides: Vec<(Word, u32)>,
}

impl MultisigGuardianConfig {
    /// Creates a new MultisigGuardian configuration.
    pub fn new(threshold: u32, signer_commitments: Vec<Word>, guardian_commitment: Word) -> Self {
        Self {
            threshold,
            signer_commitments,
            guardian_commitment,
            signature_scheme: SignatureScheme::Falcon,
            account_type: AccountType::Private,
            proc_threshold_overrides: Vec::new(),
        }
    }

    /// Sets the signature scheme for the account.
    pub fn with_signature_scheme(mut self, signature_scheme: SignatureScheme) -> Self {
        self.signature_scheme = signature_scheme;
        self
    }

    /// Sets the account type (also controls on-chain storage visibility).
    pub fn with_account_type(mut self, account_type: AccountType) -> Self {
        self.account_type = account_type;
        self
    }

    /// Adds procedure-specific threshold overrides.
    pub fn with_proc_threshold_overrides(mut self, overrides: Vec<(Word, u32)>) -> Self {
        self.proc_threshold_overrides = overrides;
        self
    }
}

/// Builder for creating MultisigGuardian accounts from the upstream
/// `AuthGuardedMultisig` component plus a `BasicWallet`.
///
/// # Example
/// ```ignore
/// use miden_confidential_contracts::multisig_guardian::{MultisigGuardianConfig, MultisigGuardianBuilder};
///
/// let config = MultisigGuardianConfig::new(2, vec![pk1, pk2], guardian_pk);
/// let account = MultisigGuardianBuilder::new(config).with_seed([0u8; 32]).build()?;
/// ```
pub struct MultisigGuardianBuilder {
    config: MultisigGuardianConfig,
    seed: [u8; 32],
}

impl MultisigGuardianBuilder {
    /// Creates a new MultisigGuardian builder with the given configuration.
    pub fn new(config: MultisigGuardianConfig) -> Self {
        Self {
            config,
            seed: [0u8; 32],
        }
    }

    /// Sets the seed used for account ID derivation.
    pub fn with_seed(mut self, seed: [u8; 32]) -> Self {
        self.seed = seed;
        self
    }

    /// Sets the account type (also controls on-chain storage visibility).
    pub fn with_account_type(mut self, account_type: AccountType) -> Self {
        self.config.account_type = account_type;
        self
    }

    /// Builds the MultisigGuardian account (fresh, undeployed).
    pub fn build(self) -> Result<Account> {
        let (seed, account_type, component) = self.into_parts()?;
        AccountBuilder::new(seed)
            .with_auth_component(component)
            .with_component(BasicWallet)
            .account_type(account_type)
            .build()
            .map_err(|e| anyhow!("failed to build account: {e}"))
    }

    /// Builds the account using `build_existing()` (for testing with pre-set account state).
    #[cfg(feature = "testing")]
    pub fn build_existing(self) -> Result<Account> {
        let (seed, account_type, component) = self.into_parts()?;
        AccountBuilder::new(seed)
            .with_auth_component(component)
            .with_component(BasicWallet)
            .account_type(account_type)
            .build_existing()
            .map_err(|e| anyhow!("failed to build existing account: {e}"))
    }

    /// Validates the config and assembles the upstream guarded-multisig component.
    fn into_parts(self) -> Result<([u8; 32], AccountType, AccountComponent)> {
        self.validate_config()?;
        let component = self.build_guarded_multisig_component()?;
        Ok((self.seed, self.config.account_type, component))
    }

    fn auth_scheme(&self) -> AuthScheme {
        match self.config.signature_scheme {
            SignatureScheme::Falcon => AuthScheme::Falcon512Poseidon2,
            SignatureScheme::Ecdsa => AuthScheme::EcdsaK256Keccak,
        }
    }

    fn build_guarded_multisig_component(&self) -> Result<AccountComponent> {
        let scheme = self.auth_scheme();

        let approvers: Vec<(PublicKeyCommitment, AuthScheme)> = self
            .config
            .signer_commitments
            .iter()
            .map(|commitment| (PublicKeyCommitment::from(*commitment), scheme))
            .collect();

        let guardian = GuardianConfig::new(
            PublicKeyCommitment::from(self.config.guardian_commitment),
            scheme,
        );

        let mut cfg = AuthGuardedMultisigConfig::new(approvers, self.config.threshold, guardian)
            .map_err(|e| anyhow!("invalid guarded-multisig config: {e}"))?;

        if !self.config.proc_threshold_overrides.is_empty() {
            let overrides: Vec<(AccountProcedureRoot, u32)> = self
                .config
                .proc_threshold_overrides
                .iter()
                .map(|(root, threshold)| (AccountProcedureRoot::from_raw(*root), *threshold))
                .collect();
            cfg = cfg
                .with_proc_thresholds(overrides)
                .map_err(|e| anyhow!("invalid procedure thresholds: {e}"))?;
        }

        let component = AuthGuardedMultisig::new(cfg)
            .map_err(|e| anyhow!("failed to build guarded-multisig component: {e}"))?;
        Ok(component.into())
    }

    fn validate_config(&self) -> Result<()> {
        if self.config.threshold == 0 {
            return Err(anyhow!("threshold must be greater than 0"));
        }
        if self.config.signer_commitments.is_empty() {
            return Err(anyhow!("at least one signer commitment is required"));
        }
        if self.config.threshold > self.config.signer_commitments.len() as u32 {
            return Err(anyhow!(
                "threshold ({}) cannot exceed number of signers ({})",
                self.config.threshold,
                self.config.signer_commitments.len()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guardian_shared::hex::{FromHex, IntoHex};

    fn mock_commitment(seed: u8) -> Word {
        Word::from([
            seed as u32,
            seed as u32 + 1,
            seed as u32 + 2,
            seed as u32 + 3,
        ])
    }

    #[test]
    fn test_config_creation() {
        let config = MultisigGuardianConfig::new(
            2,
            vec![mock_commitment(1), mock_commitment(2), mock_commitment(3)],
            mock_commitment(10),
        );

        assert_eq!(config.threshold, 2);
        assert_eq!(config.signer_commitments.len(), 3);
        assert!(config.proc_threshold_overrides.is_empty());
    }

    #[test]
    fn test_validation_zero_threshold() {
        let config = MultisigGuardianConfig::new(0, vec![mock_commitment(1)], mock_commitment(10));
        let result = MultisigGuardianBuilder::new(config).build();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("threshold must be greater than 0")
        );
    }

    #[test]
    fn test_validation_empty_signers() {
        let config = MultisigGuardianConfig::new(1, vec![], mock_commitment(10));
        let result = MultisigGuardianBuilder::new(config).build();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("at least one signer commitment")
        );
    }

    #[test]
    fn test_validation_threshold_exceeds_signers() {
        let config = MultisigGuardianConfig::new(
            3,
            vec![mock_commitment(1), mock_commitment(2)],
            mock_commitment(10),
        );
        let result = MultisigGuardianBuilder::new(config).build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot exceed"));
    }

    #[test]
    fn test_build_account() {
        let config = MultisigGuardianConfig::new(
            2,
            vec![mock_commitment(1), mock_commitment(2)],
            mock_commitment(10),
        );
        let account = MultisigGuardianBuilder::new(config)
            .with_seed([42u8; 32])
            .build();
        assert!(account.is_ok());
    }

    #[test]
    fn test_auth_procedure_is_first_in_account_code() {
        let config = MultisigGuardianConfig::new(
            2,
            vec![mock_commitment(1), mock_commitment(2)],
            mock_commitment(10),
        );

        let component = MultisigGuardianBuilder::new(config.clone())
            .build_guarded_multisig_component()
            .expect("component");
        let auth_procedures = component
            .procedures()
            .filter_map(|(root, is_auth)| is_auth.then_some(root))
            .collect::<Vec<_>>();
        assert_eq!(auth_procedures.len(), 1);
        let auth_root = auth_procedures[0];

        let account = MultisigGuardianBuilder::new(config)
            .build_existing()
            .expect("account");
        assert_eq!(account.code().procedures()[0], auth_root);
    }

    #[test]
    fn test_browser_deterministic_account_matches_rust_builder() {
        let signer_commitment =
            Word::from_hex("0x260a375ca01f1f05cd7bf22298b40c47290fc09f209011d39049b7f2ef61387b")
                .expect("signer commitment");
        let guardian_commitment =
            Word::from_hex("0xc35d79423c41d46b5289aafef48be2364e9ea494c6b14d6aefad10f1a46e6d7c")
                .expect("guardian commitment");

        let config = MultisigGuardianConfig::new(1, vec![signer_commitment], guardian_commitment);
        let account = MultisigGuardianBuilder::new(config)
            .with_seed([9u8; 32])
            .build()
            .expect("account");

        // Cross-SDK parity: the TypeScript builder must derive these same identity
        // values from the same pinned miden-standards version; regenerate both if
        // the pin changes.
        assert_eq!(account.id().to_hex(), "0x8fc3d82cee89e3614b5e3e215db370");
        assert_eq!(
            account.to_commitment().into_hex(),
            "0x9fa18826a999fa5ac79c615a00905b3e09e5e0a703a65f167d1c836e51e8e08e"
        );
    }
}
