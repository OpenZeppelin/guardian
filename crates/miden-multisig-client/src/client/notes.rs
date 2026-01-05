//! Note filtering and listing operations for MultisigClient.
//!
//! This module handles listing consumable notes and filtering them
//! by various criteria (faucet, amount, etc.).

use miden_client::note::NoteRelevance;
use miden_objects::account::AccountId;
use miden_objects::asset::Asset;
use miden_objects::note::NoteId;

use super::MultisigClient;
use crate::error::{MultisigError, Result};

/// A wrapper type for a consumable note with simplified information.
#[derive(Debug, Clone)]
pub struct ConsumableNote {
    /// The note ID.
    pub id: NoteId,
    /// Assets contained in the note.
    pub assets: Vec<Asset>,
}

impl ConsumableNote {
    /// Returns the total amount of a specific fungible asset in this note.
    pub fn amount_for_faucet(&self, faucet_id: AccountId) -> u64 {
        self.assets
            .iter()
            .filter_map(|asset| match asset {
                Asset::Fungible(fungible) if fungible.faucet_id() == faucet_id => {
                    Some(fungible.amount())
                }
                _ => None,
            })
            .sum()
    }

    /// Returns true if this note contains fungible assets from the specified faucet.
    pub fn has_faucet(&self, faucet_id: AccountId) -> bool {
        self.assets.iter().any(|asset| match asset {
            Asset::Fungible(fungible) => fungible.faucet_id() == faucet_id,
            Asset::NonFungible(_) => false,
        })
    }
}

/// Filter criteria for listing consumable notes.
///
/// # Validation
///
/// - `min_amount` requires `faucet_id` to be set (amount is per-faucet)
/// - Use `validate()` to check filter validity before use
#[derive(Debug, Clone, Default)]
pub struct NoteFilter {
    /// Only include notes containing assets from this faucet.
    pub faucet_id: Option<AccountId>,
    /// Only include notes with at least this amount (for the specified faucet).
    /// Requires `faucet_id` to be set.
    pub min_amount: Option<u64>,
}

impl NoteFilter {
    /// Creates a new filter for notes from a specific faucet.
    pub fn by_faucet(faucet_id: AccountId) -> Self {
        Self {
            faucet_id: Some(faucet_id),
            min_amount: None,
        }
    }

    /// Creates a new filter for notes from a specific faucet with minimum amount.
    pub fn by_faucet_min_amount(faucet_id: AccountId, min_amount: u64) -> Self {
        Self {
            faucet_id: Some(faucet_id),
            min_amount: Some(min_amount),
        }
    }

    /// Validates the filter configuration.
    ///
    /// Returns an error if `min_amount` is set without `faucet_id`,
    /// since amount filtering requires a specific faucet to check against.
    pub fn validate(&self) -> Result<()> {
        if self.min_amount.is_some() && self.faucet_id.is_none() {
            return Err(MultisigError::InvalidFilter(
                "min_amount requires faucet_id to be set".to_string(),
            ));
        }
        Ok(())
    }
}

impl MultisigClient {
    /// Lists notes that can be consumed by the current account.
    ///
    /// Returns a list of notes that are committed on-chain and can be consumed
    /// immediately by the multisig account.
    pub async fn list_consumable_notes(&mut self) -> Result<Vec<ConsumableNote>> {
        let account_id = self.require_account()?.id();

        let consumable = self
            .miden_client
            .get_consumable_notes(Some(account_id))
            .await
            .map_err(|e| {
                MultisigError::MidenClient(format!("failed to get consumable notes: {}", e))
            })?;

        // Convert to our wrapper type, filtering for notes consumable "Now"
        let notes = consumable
            .into_iter()
            .filter_map(|(record, relevances)| {
                // Only include notes consumable "Now" by our account
                let can_consume_now = relevances
                    .iter()
                    .any(|(id, rel)| *id == account_id && matches!(rel, NoteRelevance::Now));
                if can_consume_now {
                    Some(ConsumableNote {
                        id: record.id(),
                        assets: record.assets().iter().cloned().collect(),
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(notes)
    }

    /// Returns a list of all committed notes (not just consumable).
    pub async fn list_committed_notes(&mut self) -> Result<Vec<ConsumableNote>> {
        let account_id = self.require_account()?.id();

        let notes = self
            .miden_client
            .get_consumable_notes(Some(account_id))
            .await
            .map_err(|e| MultisigError::MidenClient(format!("failed to get notes: {}", e)))?;

        let result = notes
            .into_iter()
            .filter(|(_, relevances)| relevances.iter().any(|(id, _)| *id == account_id))
            .map(|(record, _)| ConsumableNote {
                id: record.id(),
                assets: record.assets().iter().cloned().collect(),
            })
            .collect();

        Ok(result)
    }

    /// Lists consumable notes filtered by the given criteria.
    ///
    /// This is a convenience method that combines `list_consumable_notes` with
    /// filtering. Use this to find notes from a specific faucet or above a
    /// minimum amount.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use miden_multisig_client::NoteFilter;
    ///
    /// // Find notes from a specific faucet with at least 1000 tokens
    /// let filter = NoteFilter {
    ///     faucet_id: Some(my_faucet_id),
    ///     min_amount: Some(1000),
    /// };
    /// let notes = client.list_consumable_notes_filtered(filter).await?;
    /// ```
    pub async fn list_consumable_notes_filtered(
        &mut self,
        filter: NoteFilter,
    ) -> Result<Vec<ConsumableNote>> {
        // Validate filter configuration
        filter.validate()?;

        let notes = self.list_consumable_notes().await?;

        let filtered = notes
            .into_iter()
            .filter(|note| {
                // Filter by faucet
                if let Some(faucet_id) = filter.faucet_id {
                    if !note.has_faucet(faucet_id) {
                        return false;
                    }
                    // Filter by minimum amount (faucet_id is guaranteed to be set if min_amount is)
                    if let Some(min) = filter.min_amount
                        && note.amount_for_faucet(faucet_id) < min
                    {
                        return false;
                    }
                }
                true
            })
            .collect();

        Ok(filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use a regular account ID for filter validation tests (no FungibleAsset creation)
    fn test_account_id() -> AccountId {
        AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").unwrap()
    }

    #[test]
    fn test_note_filter_validate_min_amount_without_faucet() {
        let filter = NoteFilter {
            faucet_id: None,
            min_amount: Some(1000),
        };
        assert!(filter.validate().is_err());
    }

    #[test]
    fn test_note_filter_validate_valid() {
        // No filter
        let filter = NoteFilter::default();
        assert!(filter.validate().is_ok());

        // Faucet only (any account ID works for validation)
        let filter = NoteFilter::by_faucet(test_account_id());
        assert!(filter.validate().is_ok());

        // Faucet + min_amount
        let filter = NoteFilter::by_faucet_min_amount(test_account_id(), 1000);
        assert!(filter.validate().is_ok());
    }

    #[test]
    fn test_consumable_note_empty_assets() {
        // Test with empty assets - amount should be 0, has_faucet should be false
        use miden_objects::Word;
        use miden_objects::note::NoteId;

        let note = ConsumableNote {
            id: NoteId::from(Word::default()),
            assets: vec![],
        };

        assert_eq!(note.amount_for_faucet(test_account_id()), 0);
        assert!(!note.has_faucet(test_account_id()));
    }
}
