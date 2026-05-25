//! Projectors that derive structured wire fields from a decoded
//! [`TransactionSummary`]. Used at push time by `build_metadata` to
//! populate the `derived` half of [`DeltaMetadata`], and at read time
//! by [`decode_full`] for the detail endpoint.
//!
//! Surfaces:
//!   - [`project_note_counts`] — cheap, always callable.
//!   - [`project_asset_and_counterparty_from_output_notes`] — first
//!     output note (transfers / creations).
//!   - [`project_asset_and_counterparty_from_input_notes`] — first
//!     input note (consumption).
//!   - [`decode_full`] — full detail projection including note tags
//!     and sender/recipient when derivable from note script + storage.

use miden_protocol::account::AccountId;
use miden_protocol::asset::Asset;
use miden_protocol::note::Note;
use miden_protocol::note::PartialNote;
use miden_protocol::transaction::{RawOutputNote, TransactionSummary};
use miden_standards::note::{P2idNoteStorage, P2ideNoteStorage, StandardNote};

use super::{
    AssetKind, AssetSummary, CounterpartyDirection, CounterpartySummary, DecodeWarning,
    DecodedNote, NoteCounts, NoteTag, StorageChange, VaultChange,
};

/// Note input/output counts. Always cheap, always callable.
pub fn project_note_counts(summary: &TransactionSummary) -> NoteCounts {
    NoteCounts {
        input: summary.input_notes().num_notes() as u32,
        output: summary.output_notes().num_notes() as u32,
    }
}

/// Walk the first output note and extract `(asset, counterparty)` for
/// the listing summary. Best-effort:
///
///   - First output note's first asset, when present, is surfaced as
///     `AssetSummary` (signed `amount` for fungible — negative because
///     the account *sent* the asset out).
///   - Counterparty is the output note's `sender` (per `NoteMetadata`).
///     For an account creating an output note, the sender is the
///     creating account itself; that's not a useful "counterparty"
///     from the dashboard perspective, so for single-key push we
///     leave it `None`. The multisig path overrides counterparty
///     from `proposal.recipient_id` upstream in `build_metadata`,
///     which is the right source.
///
/// Returns `(None, None)` if there are no output notes, the first one
/// is empty, or asset extraction fails.
pub fn project_asset_and_counterparty_from_output_notes(
    summary: &TransactionSummary,
) -> (Option<AssetSummary>, Option<CounterpartySummary>) {
    let outputs = summary.output_notes();
    if outputs.num_notes() == 0 {
        return (None, None);
    }
    let Some(first) = outputs.iter().next() else {
        return (None, None);
    };

    let assets = first.assets();
    let asset_summary = assets
        .iter()
        .next()
        .map(|asset| asset_summary_from_note_asset(asset, false));

    // Counterparty intentionally left None here for single-key push —
    // the output note's `metadata().sender()` is the creating account
    // (i.e. us), which is not a useful "counterparty" on the
    // dashboard. When metadata is available (multisig), build.rs
    // populates this from `proposal.recipient_id`.
    let counterparty = None;

    (asset_summary, counterparty)
}

/// Walk the first input note and extract `(asset, counterparty)` for
/// consumption-style transactions. Best-effort:
///
///   - First input note's first asset surfaces as `AssetSummary` with
///     a positive fungible magnitude (the account reclaimed value from
///     the consumed note).
///   - Counterparty is the consumed note's original sender with
///     direction `in`.
///
/// Returns `(None, None)` when there are no input notes or extraction
/// fails.
pub fn project_asset_and_counterparty_from_input_notes(
    summary: &TransactionSummary,
) -> (Option<AssetSummary>, Option<CounterpartySummary>) {
    let inputs = summary.input_notes();
    if inputs.num_notes() == 0 {
        return (None, None);
    }
    let Some(first) = inputs.iter().next() else {
        return (None, None);
    };
    let note = first.note();
    let assets = note.assets();
    let asset_summary = assets
        .iter()
        .next()
        .map(|asset| asset_summary_from_note_asset(asset, true));
    let counterparty = Some(CounterpartySummary {
        account_id: account_id_hex(note.metadata().sender()),
        direction: CounterpartyDirection::In,
    });
    (asset_summary, counterparty)
}

/// Decode the full detail-view projection from a persisted
/// `TransactionSummary`.
///
/// Returns `(input_notes, output_notes, vault_changes, storage_changes, warnings)`.
///
/// Per-section behavior:
///   - **Notes** — `note_id` (hex), standard note tag when the script
///     matches a Miden standard note, `assets` from `note.assets()`,
///     and `sender` / `recipient` when derivable from note metadata
///     and typed storage (P2ID / P2IDE targets).
///   - **Vault changes** — `added_assets()` and `removed_assets()`
///     are flat iterators that pre-resolve fungible vs. non-fungible.
///     Fungible holdings emit signed-decimal `change`; non-fungible
///     holdings emit `added` / `removed` lists keyed by faucet id.
///   - **Storage changes** — `values()` iterates `(slot_name, new_word)`
///     for slot updates. Only `after` is emitted in v1 (`before` is
///     omitted — prior slot values are not in the delta). Recovering
///     `before` would require account storage at `prev_commitment`.
///
/// MAST scripts are not exposed (US2 scope decision, 2026-05-25).
pub fn decode_full(
    summary: &TransactionSummary,
) -> (
    Vec<DecodedNote>,
    Vec<DecodedNote>,
    Vec<VaultChange>,
    Vec<StorageChange>,
    Vec<DecodeWarning>,
) {
    let warnings: Vec<DecodeWarning> = Vec::new();

    let input_notes: Vec<DecodedNote> = summary
        .input_notes()
        .iter()
        .map(|input_note| decoded_note_from_full_note(input_note.note()))
        .collect();

    let output_notes: Vec<DecodedNote> = summary
        .output_notes()
        .iter()
        .map(decoded_note_from_raw_output)
        .collect();

    let account_delta = summary.account_delta();
    let vault_changes = project_vault_changes(account_delta);
    let storage_changes = project_storage_changes(account_delta);

    // An entirely-empty projection (no notes, no vault changes, no
    // storage changes) is a legitimate state for some deltas
    // (admin ops, Guardian switch) — not an anomaly. No warning.

    (
        input_notes,
        output_notes,
        vault_changes,
        storage_changes,
        warnings,
    )
}

fn decoded_note_from_raw_output(raw: &RawOutputNote) -> DecodedNote {
    match raw {
        RawOutputNote::Full(note) => decoded_note_from_full_note(note),
        RawOutputNote::Partial(partial) => decoded_note_from_partial_note(partial),
    }
}

fn decoded_note_from_full_note(note: &Note) -> DecodedNote {
    let (sender, recipient) = project_parties_from_note(note);
    DecodedNote {
        note_id: note.id().to_hex(),
        tag: classify_note_tag(note),
        assets: note.assets().iter().map(decoded_asset_from).collect(),
        sender,
        recipient,
    }
}

fn decoded_note_from_partial_note(partial: &PartialNote) -> DecodedNote {
    DecodedNote {
        note_id: partial.id().to_hex(),
        tag: NoteTag::Custom,
        assets: partial.assets().iter().map(decoded_asset_from).collect(),
        sender: Some(account_id_hex(partial.metadata().sender())),
        recipient: None,
    }
}

fn classify_note_tag(note: &Note) -> NoteTag {
    match StandardNote::from_script(note.script()) {
        Some(StandardNote::P2ID) => NoteTag::P2id,
        Some(StandardNote::P2IDE) => NoteTag::P2ide,
        Some(StandardNote::SWAP) => NoteTag::Pswap,
        Some(StandardNote::MINT) => NoteTag::Mint,
        Some(StandardNote::BURN) => NoteTag::Burn,
        None => NoteTag::Custom,
    }
}

fn project_parties_from_note(note: &Note) -> (Option<String>, Option<String>) {
    let sender = Some(account_id_hex(note.metadata().sender()));
    let recipient = recipient_account_from_note(note);
    (sender, recipient)
}

fn recipient_account_from_note(note: &Note) -> Option<String> {
    match StandardNote::from_script(note.script())? {
        StandardNote::P2ID => P2idNoteStorage::try_from(note.storage().items())
            .ok()
            .map(|storage| account_id_hex(storage.target())),
        StandardNote::P2IDE => P2ideNoteStorage::try_from(note.storage().items())
            .ok()
            .map(|storage| account_id_hex(storage.target())),
        _ => None,
    }
}

fn asset_summary_from_note_asset(asset: &Asset, consumed: bool) -> AssetSummary {
    match asset {
        Asset::Fungible(a) => {
            let magnitude = a.amount();
            let signed = if consumed {
                format!("+{magnitude}")
            } else {
                format!("-{magnitude}")
            };
            AssetSummary {
                asset_id: a.faucet_id().to_hex(),
                kind: AssetKind::Fungible,
                amount: Some(signed),
            }
        }
        Asset::NonFungible(a) => AssetSummary {
            asset_id: a.faucet_id().to_hex(),
            kind: AssetKind::NonFungible,
            amount: None,
        },
    }
}

fn account_id_hex(account_id: AccountId) -> String {
    account_id.to_hex()
}

fn decoded_asset_from(asset: &Asset) -> super::DecodedAsset {
    use miden_protocol::asset::Asset;
    match asset {
        Asset::Fungible(a) => super::DecodedAsset {
            asset_id: a.faucet_id().to_hex(),
            kind: AssetKind::Fungible,
            amount: Some(a.amount().to_string()),
        },
        Asset::NonFungible(a) => super::DecodedAsset {
            asset_id: a.faucet_id().to_hex(),
            kind: AssetKind::NonFungible,
            amount: None,
        },
    }
}

fn project_vault_changes(delta: &miden_protocol::account::delta::AccountDelta) -> Vec<VaultChange> {
    use miden_protocol::asset::Asset;
    use std::collections::BTreeMap;

    let vault = delta.vault();
    let mut out: Vec<VaultChange> = Vec::new();

    // Fungibles — `added_assets` + `removed_assets` come pre-classified
    // as Fungible(_) or NonFungible(_). Group by faucet id and net the
    // signed delta.
    let mut fungible_net: BTreeMap<String, i128> = BTreeMap::new();
    for asset in vault.added_assets() {
        if let Asset::Fungible(a) = asset {
            *fungible_net.entry(a.faucet_id().to_hex()).or_insert(0) += a.amount() as i128;
        }
    }
    for asset in vault.removed_assets() {
        if let Asset::Fungible(a) = asset {
            *fungible_net.entry(a.faucet_id().to_hex()).or_insert(0) -= a.amount() as i128;
        }
    }
    for (asset_id, net) in fungible_net {
        if net == 0 {
            continue;
        }
        let change = if net > 0 {
            format!("+{net}")
        } else {
            format!("{net}") // already has the leading `-`
        };
        out.push(VaultChange::Fungible { asset_id, change });
    }

    // Non-fungibles — group added/removed by faucet id; emit per-faucet
    // lists.
    let mut nf_added: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut nf_removed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for asset in vault.added_assets() {
        if let Asset::NonFungible(a) = asset {
            let faucet = a.faucet_id().to_hex();
            // The asset's vault key uniquely identifies it within the
            // faucet — best-available stable "asset id" string.
            let id = format!("{:?}", a.vault_key());
            nf_added.entry(faucet).or_default().push(id);
        }
    }
    for asset in vault.removed_assets() {
        if let Asset::NonFungible(a) = asset {
            let faucet = a.faucet_id().to_hex();
            let id = format!("{:?}", a.vault_key());
            nf_removed.entry(faucet).or_default().push(id);
        }
    }
    let mut nf_faucets: std::collections::BTreeSet<String> = Default::default();
    nf_faucets.extend(nf_added.keys().cloned());
    nf_faucets.extend(nf_removed.keys().cloned());
    for faucet in nf_faucets {
        out.push(VaultChange::NonFungible {
            asset_id: faucet.clone(),
            added: nf_added.remove(&faucet).unwrap_or_default(),
            removed: nf_removed.remove(&faucet).unwrap_or_default(),
        });
    }

    out
}

fn project_storage_changes(
    delta: &miden_protocol::account::delta::AccountDelta,
) -> Vec<StorageChange> {
    let storage = delta.storage();
    storage
        .values()
        .map(|(slot_name, word)| StorageChange {
            // `StorageSlotName` is `{ name: Arc<str>, id: StorageSlotId }`
            // — slots are identified by a human-readable string, not
            // a numeric index. Surface the name verbatim.
            slot_name: slot_name.as_str().to_string(),
            before: None,
            after: Some(format!("0x{}", hex::encode(word.as_bytes()))),
        })
        .collect()
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use miden_protocol::account::AccountId;
    use miden_protocol::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
    use miden_protocol::asset::FungibleAsset;
    use miden_protocol::crypto::rand::RandomCoin;
    use miden_protocol::note::NoteType;
    use miden_protocol::transaction::InputNote;
    use miden_protocol::transaction::{InputNotes, RawOutputNotes, TransactionSummary};
    use miden_protocol::{Felt, Word, ZERO};
    use miden_standards::note::P2idNote;

    const CONSUMER: &str = "0x9d03b229c1a649905f70588309fe71";
    const NOTE_SENDER: &str = "0x7bfb0f38b0fafa103f86a805594170";
    const FAUCET: &str = "0x16f6c85d5652c9200879145bfdda93";

    fn summary_with_consumed_p2id_note() -> TransactionSummary {
        let sender = AccountId::from_hex(NOTE_SENDER).expect("sender");
        let consumer = AccountId::from_hex(CONSUMER).expect("consumer");
        let faucet = AccountId::from_hex(FAUCET).expect("faucet");
        let asset = FungibleAsset::new(faucet, 100_000_000)
            .expect("fungible asset")
            .into();
        let mut rng = RandomCoin::new(Word::from([1u32, 2, 3, 4]));
        let note = P2idNote::create(
            sender,
            consumer,
            vec![asset],
            NoteType::Public,
            Default::default(),
            &mut rng,
        )
        .expect("p2id note");
        let input = InputNote::unauthenticated(note);
        let delta = AccountDelta::new(
            consumer,
            AccountStorageDelta::default(),
            AccountVaultDelta::default(),
            Felt::ZERO,
        )
        .expect("account delta");
        TransactionSummary::new(
            delta,
            InputNotes::new(vec![input]).expect("input notes"),
            RawOutputNotes::new(Vec::new()).expect("output notes"),
            Word::from([ZERO; 4]),
        )
    }

    #[test]
    fn project_input_notes_surfaces_consumed_asset_and_sender_counterparty() {
        let summary = summary_with_consumed_p2id_note();
        let (asset, counterparty) = project_asset_and_counterparty_from_input_notes(&summary);
        let asset = asset.expect("asset from consumed note");
        assert_eq!(asset.kind, AssetKind::Fungible);
        assert_eq!(asset.asset_id, FAUCET);
        assert_eq!(asset.amount.as_deref(), Some("+100000000"));
        let cp = counterparty.expect("counterparty");
        assert_eq!(cp.account_id, NOTE_SENDER);
        assert_eq!(cp.direction, CounterpartyDirection::In);
    }

    #[test]
    fn decode_full_classifies_p2id_input_note_tag_and_parties() {
        let summary = summary_with_consumed_p2id_note();
        let (inputs, outputs, _, storage, warnings) = decode_full(&summary);
        assert!(warnings.is_empty());
        assert_eq!(inputs.len(), 1);
        assert!(outputs.is_empty());
        assert!(storage.is_empty());
        assert_eq!(inputs[0].tag, NoteTag::P2id);
        assert_eq!(inputs[0].sender.as_deref(), Some(NOTE_SENDER));
        assert_eq!(inputs[0].recipient.as_deref(), Some(CONSUMER));
        assert_eq!(inputs[0].assets[0].amount.as_deref(), Some("100000000"));
    }

    #[test]
    fn storage_change_json_omits_before_when_unpopulated() {
        let change = StorageChange {
            slot_name: "openzeppelin::multisig::threshold_config".to_string(),
            before: None,
            after: Some("0x0200".to_string()),
        };
        let json = serde_json::to_value(&change).expect("serializable");
        assert!(json.get("before").is_none());
        assert_eq!(json.get("after").and_then(|v| v.as_str()), Some("0x0200"));
    }
}
