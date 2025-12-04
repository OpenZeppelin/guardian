//! Multisig account wrapper with storage inspection helpers.

use miden_client::Serializable;
use miden_objects::Word;
use miden_objects::account::{Account, AccountId};

use crate::error::{MultisigError, Result};

/// Wrapper around a Miden Account with multisig-specific helpers.
///
/// This provides convenient access to multisig configuration stored in account storage:
/// - Slot 0: Threshold config `[threshold, num_signers, 0, 0]`
/// - Slot 1: Cosigner commitments map `[index, 0, 0, 0] => COMMITMENT`
/// - Slot 4: PSM selector `[1, 0, 0, 0]` (ON) or `[0, 0, 0, 0]` (OFF)
/// - Slot 5: PSM public key map
#[derive(Debug, Clone)]
pub struct MultisigAccount {
    account: Account,
    psm_endpoint: String,
}

impl MultisigAccount {
    /// Creates a new MultisigAccount wrapper.
    pub fn new(account: Account, psm_endpoint: impl Into<String>) -> Self {
        Self {
            account,
            psm_endpoint: psm_endpoint.into(),
        }
    }

    /// Returns the account ID.
    pub fn id(&self) -> AccountId {
        self.account.id()
    }

    /// Returns the account nonce.
    pub fn nonce(&self) -> u64 {
        self.account.nonce().as_int()
    }

    /// Returns the account commitment (hash).
    pub fn commitment(&self) -> Word {
        self.account.commitment()
    }

    /// Returns the associated PSM endpoint.
    pub fn psm_endpoint(&self) -> &str {
        &self.psm_endpoint
    }

    /// Returns a reference to the underlying Account.
    pub fn inner(&self) -> &Account {
        &self.account
    }

    /// Consumes self and returns the underlying Account.
    pub fn into_inner(self) -> Account {
        self.account
    }

    /// Returns the multisig threshold from storage slot 0.
    pub fn threshold(&self) -> Result<u32> {
        let slot_value = self
            .account
            .storage()
            .get_item(0)
            .map_err(|e| MultisigError::AccountStorage(e.to_string()))?;

        Ok(slot_value[0].as_int() as u32)
    }

    /// Returns the number of signers from storage slot 0.
    pub fn num_signers(&self) -> Result<u32> {
        let slot_value = self
            .account
            .storage()
            .get_item(0)
            .map_err(|e| MultisigError::AccountStorage(e.to_string()))?;

        Ok(slot_value[1].as_int() as u32)
    }

    /// Extracts cosigner commitments from storage slot 1.
    ///
    /// Returns a vector of commitment Words. Returns empty vector if
    /// slot 1 is empty or has no entries.
    pub fn cosigner_commitments(&self) -> Vec<Word> {
        let mut commitments = Vec::new();

        let key_zero = Word::from([0u32, 0, 0, 0]);
        let first_entry = self.account.storage().get_map_item(1, key_zero);

        if first_entry.is_err() || first_entry.as_ref().unwrap() == &Word::default() {
            return commitments;
        }

        let mut index = 0u32;
        loop {
            let key = Word::from([index, 0, 0, 0]);
            match self.account.storage().get_map_item(1, key) {
                Ok(value) if value != Word::default() => {
                    commitments.push(value);
                    index += 1;
                }
                _ => break,
            }
        }

        commitments
    }

    /// Extracts cosigner commitments as hex strings with 0x prefix.
    pub fn cosigner_commitments_hex(&self) -> Vec<String> {
        self.cosigner_commitments()
            .into_iter()
            .map(|word| format!("0x{}", hex::encode(word.to_bytes())))
            .collect()
    }

    /// Checks if the given commitment is a cosigner of this account.
    pub fn is_cosigner(&self, commitment: &Word) -> bool {
        self.cosigner_commitments().contains(commitment)
    }

    /// Returns whether PSM verification is enabled (storage slot 4).
    pub fn psm_enabled(&self) -> Result<bool> {
        let slot_value = self
            .account
            .storage()
            .get_item(4)
            .map_err(|e| MultisigError::AccountStorage(e.to_string()))?;

        Ok(slot_value[0].as_int() == 1)
    }

    /// Returns the PSM server commitment from storage slot 5.
    pub fn psm_commitment(&self) -> Result<Word> {
        let key = Word::from([0u32, 0, 0, 0]);
        self.account
            .storage()
            .get_map_item(5, key)
            .map_err(|e| MultisigError::AccountStorage(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    // Note: Full tests require creating actual multisig accounts which needs
    // the miden-confidential-contracts crate. Integration tests will cover this.
}
