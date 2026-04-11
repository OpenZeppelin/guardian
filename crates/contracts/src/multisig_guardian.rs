//! MultisigGuardian Account Builder
//!
//! This module provides a high-level API for creating accounts with multisig + GUARDIAN authentication.
//! It serves as the single source of truth for MultisigGuardian account creation across the codebase.

use anyhow::{Result, anyhow};
use miden_protocol::{
    Word,
    account::{
        Account, AccountBuilder, AccountStorageMode, AccountType, StorageMap, StorageMapKey,
        StorageSlot, StorageSlotName,
    },
};
use miden_standards::account::wallets::BasicWallet;

use crate::masm_builder::{
    build_multisig_guardian_component, build_multisig_guardian_ecdsa_component,
};
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
    /// Whether GUARDIAN verification is enabled (true = ON, false = OFF).
    pub guardian_enabled: bool,
    /// Signature scheme for the account (Falcon or ECDSA).
    pub signature_scheme: SignatureScheme,
    /// Account storage mode (defaults to Private).
    pub storage_mode: AccountStorageMode,
    /// Optional procedure-specific threshold overrides.
    /// Map from procedure root to threshold.
    pub proc_threshold_overrides: Vec<(Word, u32)>,
}

impl MultisigGuardianConfig {
    /// Creates a new MultisigGuardian configuration.
    ///
    /// # Arguments
    /// * `threshold` - Minimum number of signatures required
    /// * `signer_commitments` - Public key commitments of all signers
    /// * `guardian_commitment` - GUARDIAN server public key commitment
    ///
    /// # Example
    /// ```ignore
    /// let config = MultisigGuardianConfig::new(2, vec![pk1, pk2, pk3], guardian_pk);
    /// ```
    pub fn new(threshold: u32, signer_commitments: Vec<Word>, guardian_commitment: Word) -> Self {
        Self {
            threshold,
            signer_commitments,
            guardian_commitment,
            guardian_enabled: true,
            signature_scheme: SignatureScheme::Falcon,
            storage_mode: AccountStorageMode::Private,
            proc_threshold_overrides: Vec::new(),
        }
    }

    /// Sets whether GUARDIAN verification is enabled.
    pub fn with_guardian_enabled(mut self, enabled: bool) -> Self {
        self.guardian_enabled = enabled;
        self
    }

    /// Sets the signature scheme for the account.
    pub fn with_signature_scheme(mut self, signature_scheme: SignatureScheme) -> Self {
        self.signature_scheme = signature_scheme;
        self
    }

    /// Sets the account storage mode.
    pub fn with_storage_mode(mut self, storage_mode: AccountStorageMode) -> Self {
        self.storage_mode = storage_mode;
        self
    }

    /// Adds procedure-specific threshold overrides.
    pub fn with_proc_threshold_overrides(mut self, overrides: Vec<(Word, u32)>) -> Self {
        self.proc_threshold_overrides = overrides;
        self
    }
}

/// Builder for creating MultisigGuardian accounts.
///
/// This builder provides a fluent API for creating accounts with multisig + GUARDIAN authentication.
///
/// # Storage Layout
///
/// The account uses a single auth component with the following storage layout:
///
/// **Combined Auth Component (6 slots):**
/// - Slot 0: Threshold config `[threshold, num_signers, 0, 0]`
/// - Slot 1: Signer public keys map `[index, 0, 0, 0] => COMMITMENT`
/// - Slot 2: Signer scheme IDs map `[index, 0, 0, 0] => [scheme_id, 0, 0, 0]`
/// - Slot 3: Executed transactions map (for replay protection)
/// - Slot 4: Procedure threshold overrides map
/// - Slot 5: GUARDIAN selector `[1, 0, 0, 0]` (ON) or `[0, 0, 0, 0]` (OFF)
/// - Slot 6: GUARDIAN public key map `[0, 0, 0, 0] => GUARDIAN_COMMITMENT`
/// - Slot 7: GUARDIAN scheme ID map `[0, 0, 0, 0] => [scheme_id, 0, 0, 0]`
///
/// # Example
/// ```ignore
/// use miden_confidential_contracts::multisig_guardian::{MultisigGuardianConfig, MultisigGuardianBuilder};
///
/// let config = MultisigGuardianConfig::new(2, vec![pk1, pk2], guardian_pk);
/// let account = MultisigGuardianBuilder::new(config)
///     .with_seed([0u8; 32])
///     .build()?;
/// ```
pub struct MultisigGuardianBuilder {
    config: MultisigGuardianConfig,
    seed: [u8; 32],
    account_type: AccountType,
    storage_mode: AccountStorageMode,
}

impl MultisigGuardianBuilder {
    /// Creates a new MultisigGuardian builder with the given configuration.
    pub fn new(config: MultisigGuardianConfig) -> Self {
        let storage_mode = config.storage_mode;
        Self {
            config,
            seed: [0u8; 32],
            account_type: AccountType::RegularAccountUpdatableCode,
            storage_mode,
        }
    }

    /// Sets the seed used for account ID derivation.
    pub fn with_seed(mut self, seed: [u8; 32]) -> Self {
        self.seed = seed;
        self
    }

    /// Sets the account type.
    pub fn with_account_type(mut self, account_type: AccountType) -> Self {
        self.account_type = account_type;
        self
    }

    /// Sets the storage mode.
    pub fn with_storage_mode(mut self, storage_mode: AccountStorageMode) -> Self {
        self.storage_mode = storage_mode;
        self
    }

    /// Builds the MultisigGuardian account.
    ///
    /// This creates a new account with:
    /// - Multisig authentication component with GUARDIAN procedures
    /// - BasicWallet component for asset management
    pub fn build(self) -> Result<Account> {
        self.validate_config()?;

        let auth_slots = self.build_auth_slots()?;

        let auth_component = match self.config.signature_scheme {
            SignatureScheme::Falcon => build_multisig_guardian_component(auth_slots)?,
            SignatureScheme::Ecdsa => build_multisig_guardian_ecdsa_component(auth_slots)?,
        };

        let account = AccountBuilder::new(self.seed)
            .with_auth_component(auth_component)
            .with_component(BasicWallet)
            .account_type(self.account_type)
            .storage_mode(self.storage_mode)
            .build()
            .map_err(|e| anyhow!("failed to build account: {e}"))?;

        Ok(account)
    }

    /// Builds the account using `build_existing()` (for testing with pre-set account state).
    pub fn build_existing(self) -> Result<Account> {
        self.validate_config()?;

        let auth_slots = self.build_auth_slots()?;

        let auth_component = match self.config.signature_scheme {
            SignatureScheme::Falcon => build_multisig_guardian_component(auth_slots)?,
            SignatureScheme::Ecdsa => build_multisig_guardian_ecdsa_component(auth_slots)?,
        };

        let account = AccountBuilder::new(self.seed)
            .with_auth_component(auth_component)
            .with_component(BasicWallet)
            .account_type(self.account_type)
            .storage_mode(self.storage_mode)
            .build_existing()
            .map_err(|e| anyhow!("failed to build existing account: {e}"))?;

        Ok(account)
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

    fn build_multisig_slots(&self) -> Result<Vec<StorageSlot>> {
        let num_signers = self.config.signer_commitments.len() as u32;
        let scheme_id = match self.config.signature_scheme {
            SignatureScheme::Falcon => 2u32,
            SignatureScheme::Ecdsa => 1u32,
        };

        // Slot 0: Threshold config
        let threshold_config_name =
            StorageSlotName::new("openzeppelin::multisig::threshold_config")
                .map_err(|e| anyhow!("failed to create storage slot name: {e}"))?;
        let slot_0 = StorageSlot::with_value(
            threshold_config_name,
            Word::from([self.config.threshold, num_signers, 0, 0]),
        );

        // Slot 1: Signer public keys map
        let signer_keys_name = StorageSlotName::new("openzeppelin::multisig::signer_public_keys")
            .map_err(|e| anyhow!("failed to create storage slot name: {e}"))?;
        let map_entries = self
            .config
            .signer_commitments
            .iter()
            .enumerate()
            .map(|(i, commitment)| (StorageMapKey::from_index(i as u32), *commitment));
        let slot_1 = StorageSlot::with_map(
            signer_keys_name,
            StorageMap::with_entries(map_entries)
                .map_err(|e| anyhow!("failed to create signer keys map: {e}"))?,
        );

        // Slot 2: Signer scheme IDs map
        let signer_scheme_ids_name =
            StorageSlotName::new("openzeppelin::multisig::signer_scheme_ids")
                .map_err(|e| anyhow!("failed to create storage slot name: {e}"))?;
        let scheme_entries = (0..num_signers).map(|i| {
            (
                StorageMapKey::from_index(i),
                Word::from([scheme_id, 0, 0, 0]),
            )
        });
        let slot_2 = StorageSlot::with_map(
            signer_scheme_ids_name,
            StorageMap::with_entries(scheme_entries)
                .map_err(|e| anyhow!("failed to create signer scheme map: {e}"))?,
        );

        // Slot 3: Executed transactions map (empty)
        let executed_txs_name =
            StorageSlotName::new("openzeppelin::multisig::executed_transactions")
                .map_err(|e| anyhow!("failed to create storage slot name: {e}"))?;
        let slot_3 = StorageSlot::with_map(executed_txs_name, StorageMap::default());

        // Slot 4: Procedure threshold overrides
        let proc_thresholds_name =
            StorageSlotName::new("openzeppelin::multisig::procedure_thresholds")
                .map_err(|e| anyhow!("failed to create storage slot name: {e}"))?;
        let proc_overrides =
            self.config
                .proc_threshold_overrides
                .iter()
                .map(|(proc_root, threshold)| {
                    (
                        StorageMapKey::new(*proc_root),
                        Word::from([*threshold, 0, 0, 0]),
                    )
                });
        let slot_4 = StorageSlot::with_map(
            proc_thresholds_name,
            StorageMap::with_entries(proc_overrides)
                .map_err(|e| anyhow!("failed to create proc threshold map: {e}"))?,
        );

        Ok(vec![slot_0, slot_1, slot_2, slot_3, slot_4])
    }

    fn build_auth_slots(&self) -> Result<Vec<StorageSlot>> {
        let mut slots = self.build_multisig_slots()?;
        slots.extend(self.build_guardian_slots()?);
        Ok(slots)
    }

    fn build_guardian_slots(&self) -> Result<Vec<StorageSlot>> {
        let scheme_id = match self.config.signature_scheme {
            SignatureScheme::Falcon => 2u32,
            SignatureScheme::Ecdsa => 1u32,
        };

        // Slot 0: GUARDIAN selector
        let guardian_selector_name = StorageSlotName::new("openzeppelin::guardian::selector")
            .map_err(|e| anyhow!("failed to create storage slot name: {e}"))?;
        let selector = if self.config.guardian_enabled {
            1u32
        } else {
            0u32
        };
        let slot_0 =
            StorageSlot::with_value(guardian_selector_name, Word::from([selector, 0, 0, 0]));

        // Slot 1: GUARDIAN public key map
        let guardian_public_key_name =
            StorageSlotName::new("openzeppelin::guardian::public_key")
                .map_err(|e| anyhow!("failed to create storage slot name: {e}"))?;
        let guardian_key_entries = vec![(
            StorageMapKey::from_index(0),
            self.config.guardian_commitment,
        )];
        let slot_1 = StorageSlot::with_map(
            guardian_public_key_name,
            StorageMap::with_entries(guardian_key_entries)
                .map_err(|e| anyhow!("failed to create GUARDIAN key map: {e}"))?,
        );

        let guardian_scheme_id_name = StorageSlotName::new("openzeppelin::guardian::scheme_id")
            .map_err(|e| anyhow!("failed to create storage slot name: {e}"))?;
        let guardian_scheme_entries = vec![(
            StorageMapKey::from_index(0),
            Word::from([scheme_id, 0, 0, 0]),
        )];
        let slot_2 = StorageSlot::with_map(
            guardian_scheme_id_name,
            StorageMap::with_entries(guardian_scheme_entries)
                .map_err(|e| anyhow!("failed to create GUARDIAN scheme map: {e}"))?,
        );

        Ok(vec![slot_0, slot_1, slot_2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::masm_builder::build_multisig_guardian_component;
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
        assert!(config.guardian_enabled);
        assert!(config.proc_threshold_overrides.is_empty());
    }

    #[test]
    fn test_config_with_guardian_disabled() {
        let config = MultisigGuardianConfig::new(1, vec![mock_commitment(1)], mock_commitment(10))
            .with_guardian_enabled(false);

        assert!(!config.guardian_enabled);
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
    fn test_guardian_auth_procedure_is_first_in_account_code() {
        let config = MultisigGuardianConfig::new(
            2,
            vec![mock_commitment(1), mock_commitment(2)],
            mock_commitment(10),
        )
        .with_storage_mode(AccountStorageMode::Public);

        let builder = MultisigGuardianBuilder::new(config.clone());
        let auth_slots = builder.build_auth_slots().expect("auth slots");
        let component = build_multisig_guardian_component(auth_slots).expect("guardian component");
        let auth_procedures = component
            .procedures()
            .filter_map(|(root, is_auth)| is_auth.then_some(root))
            .collect::<Vec<_>>();

        assert_eq!(auth_procedures.len(), 1);

        let auth_root = auth_procedures[0];

        let account = MultisigGuardianBuilder::new(config)
            .build_existing()
            .expect("guardian account");

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

        assert_eq!(account.id().to_hex(), "0x4c053cea120ba890494eba281a8e5c");
        assert_eq!(
            account.to_commitment().into_hex(),
            "0x49f0b7a53c9104ae8b370ac5db29a0ad04348b1aa4b104f5ec260775cf6bd5b9"
        );
    }
}
