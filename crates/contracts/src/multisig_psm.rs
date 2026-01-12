//! MultisigPsm Account Builder
//!
//! This module provides a high-level API for creating accounts with multisig + PSM authentication.
//! It serves as the single source of truth for MultisigPsm account creation across the codebase.

use anyhow::{Result, anyhow};
use miden_lib::account::wallets::BasicWallet;
use miden_objects::{
    Word,
    account::{Account, AccountBuilder, AccountStorageMode, AccountType, StorageMap, StorageSlot},
};

use crate::masm_builder::{build_multisig_component, build_psm_component};

/// Configuration for creating a MultisigPsm account.
#[derive(Debug, Clone)]
pub struct MultisigPsmConfig {
    /// The minimum number of signatures required to authorize a transaction.
    pub threshold: u32,
    /// Public key commitments of all signers (as Words).
    pub signer_commitments: Vec<Word>,
    /// PSM public key commitment.
    pub psm_commitment: Word,
    /// Whether PSM verification is enabled (true = ON, false = OFF).
    pub psm_enabled: bool,
    /// Account storage mode (defaults to Private).
    pub storage_mode: AccountStorageMode,
    /// Optional procedure-specific threshold overrides.
    /// Map from procedure root to threshold.
    pub proc_threshold_overrides: Vec<(Word, u32)>,
}

impl MultisigPsmConfig {
    /// Creates a new MultisigPsm configuration.
    ///
    /// # Arguments
    /// * `threshold` - Minimum number of signatures required
    /// * `signer_commitments` - Public key commitments of all signers
    /// * `psm_commitment` - PSM server public key commitment
    ///
    /// # Example
    /// ```ignore
    /// let config = MultisigPsmConfig::new(2, vec![pk1, pk2, pk3], psm_pk);
    /// ```
    pub fn new(threshold: u32, signer_commitments: Vec<Word>, psm_commitment: Word) -> Self {
        Self {
            threshold,
            signer_commitments,
            psm_commitment,
            psm_enabled: true,
            storage_mode: AccountStorageMode::Private,
            proc_threshold_overrides: Vec::new(),
        }
    }

    /// Sets whether PSM verification is enabled.
    pub fn with_psm_enabled(mut self, enabled: bool) -> Self {
        self.psm_enabled = enabled;
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

/// Builder for creating MultisigPsm accounts.
///
/// This builder provides a fluent API for creating accounts with multisig + PSM authentication.
///
/// # Storage Layout
///
/// The account uses two components with the following storage layout:
///
/// **Multisig Component (4 slots):**
/// - Slot 0: Threshold config `[threshold, num_signers, 0, 0]`
/// - Slot 1: Signer public keys map `[index, 0, 0, 0] => COMMITMENT`
/// - Slot 2: Executed transactions map (for replay protection)
/// - Slot 3: Procedure threshold overrides map
///
/// **PSM Component (2 slots, offset by 4):**
/// - Slot 4: PSM selector `[1, 0, 0, 0]` (ON) or `[0, 0, 0, 0]` (OFF)
/// - Slot 5: PSM public key map `[0, 0, 0, 0] => PSM_COMMITMENT`
///
/// # Example
/// ```ignore
/// use miden_confidential_contracts::multisig_psm::{MultisigPsmConfig, MultisigPsmBuilder};
///
/// let config = MultisigPsmConfig::new(2, vec![pk1, pk2], psm_pk);
/// let account = MultisigPsmBuilder::new(config)
///     .with_seed([0u8; 32])
///     .build()?;
/// ```
pub struct MultisigPsmBuilder {
    config: MultisigPsmConfig,
    seed: [u8; 32],
    account_type: AccountType,
    storage_mode: AccountStorageMode,
}

impl MultisigPsmBuilder {
    /// Creates a new MultisigPsm builder with the given configuration.
    pub fn new(config: MultisigPsmConfig) -> Self {
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

    /// Builds the MultisigPsm account.
    ///
    /// This creates a new account with:
    /// - Multisig authentication component
    /// - PSM verification component
    /// - BasicWallet component for asset management
    pub fn build(self) -> Result<Account> {
        self.validate_config()?;

        let multisig_slots = self.build_multisig_slots()?;
        let psm_slots = self.build_psm_slots()?;

        let multisig_component = build_multisig_component(multisig_slots)?;
        let psm_component = build_psm_component(psm_slots)?;

        let account = AccountBuilder::new(self.seed)
            .with_auth_component(multisig_component)
            .with_component(psm_component)
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

        let multisig_slots = self.build_multisig_slots()?;
        let psm_slots = self.build_psm_slots()?;

        let multisig_component = build_multisig_component(multisig_slots)?;
        let psm_component = build_psm_component(psm_slots)?;

        let account = AccountBuilder::new(self.seed)
            .with_auth_component(multisig_component)
            .with_component(psm_component)
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

        // Slot 0: Threshold config
        let slot_0 = StorageSlot::Value(Word::from([self.config.threshold, num_signers, 0, 0]));

        // Slot 1: Signer public keys map
        let map_entries = self
            .config
            .signer_commitments
            .iter()
            .enumerate()
            .map(|(i, commitment)| (Word::from([i as u32, 0, 0, 0]), *commitment));
        let slot_1 = StorageSlot::Map(
            StorageMap::with_entries(map_entries)
                .map_err(|e| anyhow!("failed to create signer keys map: {e}"))?,
        );

        // Slot 2: Executed transactions map (empty)
        let slot_2 = StorageSlot::Map(StorageMap::default());

        // Slot 3: Procedure threshold overrides
        let proc_overrides = self
            .config
            .proc_threshold_overrides
            .iter()
            .map(|(proc_root, threshold)| (*proc_root, Word::from([*threshold, 0, 0, 0])));
        let slot_3 = StorageSlot::Map(
            StorageMap::with_entries(proc_overrides)
                .map_err(|e| anyhow!("failed to create proc threshold map: {e}"))?,
        );

        Ok(vec![slot_0, slot_1, slot_2, slot_3])
    }

    fn build_psm_slots(&self) -> Result<Vec<StorageSlot>> {
        // Slot 0: PSM selector
        let selector = if self.config.psm_enabled { 1u32 } else { 0u32 };
        let slot_0 = StorageSlot::Value(Word::from([selector, 0, 0, 0]));

        // Slot 1: PSM public key map
        let psm_key_entries = vec![(Word::from([0u32, 0, 0, 0]), self.config.psm_commitment)];
        let slot_1 = StorageSlot::Map(
            StorageMap::with_entries(psm_key_entries)
                .map_err(|e| anyhow!("failed to create PSM key map: {e}"))?,
        );

        Ok(vec![slot_0, slot_1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let config = MultisigPsmConfig::new(
            2,
            vec![mock_commitment(1), mock_commitment(2), mock_commitment(3)],
            mock_commitment(10),
        );

        assert_eq!(config.threshold, 2);
        assert_eq!(config.signer_commitments.len(), 3);
        assert!(config.psm_enabled);
        assert!(config.proc_threshold_overrides.is_empty());
    }

    #[test]
    fn test_config_with_psm_disabled() {
        let config = MultisigPsmConfig::new(1, vec![mock_commitment(1)], mock_commitment(10))
            .with_psm_enabled(false);

        assert!(!config.psm_enabled);
    }

    #[test]
    fn test_validation_zero_threshold() {
        let config = MultisigPsmConfig::new(0, vec![mock_commitment(1)], mock_commitment(10));

        let result = MultisigPsmBuilder::new(config).build();
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
        let config = MultisigPsmConfig::new(1, vec![], mock_commitment(10));

        let result = MultisigPsmBuilder::new(config).build();
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
        let config = MultisigPsmConfig::new(
            3,
            vec![mock_commitment(1), mock_commitment(2)],
            mock_commitment(10),
        );

        let result = MultisigPsmBuilder::new(config).build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot exceed"));
    }

    #[test]
    fn test_build_account() {
        let config = MultisigPsmConfig::new(
            2,
            vec![mock_commitment(1), mock_commitment(2)],
            mock_commitment(10),
        );

        let account = MultisigPsmBuilder::new(config)
            .with_seed([42u8; 32])
            .build();

        assert!(account.is_ok());
    }
}
