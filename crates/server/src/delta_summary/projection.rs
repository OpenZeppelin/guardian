//! Projectors that derive structured wire fields from a decoded
//! [`TransactionSummary`]. Used at push time by `build_metadata` and
//! at read time by [`decode_full`] for the detail endpoint.

use miden_protocol::account::AccountId;
use miden_protocol::asset::{Asset, NonFungibleAsset};
use miden_protocol::note::Note;
use miden_protocol::note::NoteMetadata;
use miden_protocol::note::NoteType as MidenNoteType;
use miden_protocol::note::PartialNote;
use miden_protocol::transaction::{RawOutputNote, TransactionSummary};
use miden_standards::note::{P2idNoteStorage, P2ideNoteStorage, StandardNote};

use super::{
    AssetKind, AssetSummary, CounterpartyDirection, CounterpartySummary, DecodeWarning,
    DecodedNote, NoteCounts, NoteTag, NoteVisibility, StorageChange, VaultChange,
};

/// Whether this carries the `TX_FEE` script. Says nothing about who created it:
/// `TxFeeNote::builder` is public, so a caller can emit a real one, and
/// `StandardNote::from_script` compares script roots. `Partial` notes carry no
/// script and can never match; the tag is not consulted, because a note's tag is
/// chosen by whoever builds it.
fn is_tx_fee_note(raw: &RawOutputNote) -> bool {
    match raw {
        RawOutputNote::Full(note) => {
            StandardNote::from_script(note.script()) == Some(StandardNote::TX_FEE)
        }
        RawOutputNote::Partial(_) => false,
    }
}

/// Position of the fee note `fee::pay_fee` appended, if it can be identified.
///
/// Provenance is not recoverable from the summary. It carries no mark for where
/// the auth procedure's own outputs begin, and a caller-built `TX_FEE` note is
/// indistinguishable from the protocol's by script root. Position is the only
/// remaining signal: `pay_fee` runs at the end of authentication, after the
/// transaction script has created every note it is going to, so the injected fee
/// note is last.
///
/// Getting this wrong in the excluding direction is far worse than getting it
/// wrong in the including direction. A `TX_FEE` note restricts nobody from
/// consuming it — any account may claim its assets — so a caller who could
/// persuade this function to skip a note of their own would be concealing a
/// bearer instrument they can then collect. So it resolves only the
/// unambiguous case: exactly one `TX_FEE` note, in last position. A second one
/// means a caller built at least one, and nothing is excluded.
///
/// Residual, accepted: on a chain that charges no fee there is no injected note,
/// so a caller's `TX_FEE` note placed last matches this and is hidden. Closing
/// that needs a provenance signal the summary does not carry.
fn protocol_fee_note_index(summary: &TransactionSummary) -> Option<usize> {
    let notes = summary.output_notes();
    let mut fee_positions = notes
        .iter()
        .enumerate()
        .filter(|(_, raw)| is_tx_fee_note(raw))
        .map(|(index, _)| index);

    let only = fee_positions.next()?;
    if fee_positions.next().is_some() {
        return None;
    }
    (only + 1 == notes.num_notes()).then_some(only)
}

/// The transaction's own output notes, excluding the fee note the protocol
/// appended. Every listing-level projection uses this so a fee does not reach
/// the dashboard as a transfer. `decode_full` keeps the fee note, so the detail
/// view still shows what actually happened — though it tags the note `Custom`
/// like any other unrecognised script rather than labelling it a fee.
///
/// The cut is inferred, not read: nothing in the summary records which notes the
/// auth procedure added. See [`protocol_fee_note_index`] for how it is inferred
/// and where the inference declines to guess. Note that the protocol's own
/// `num_output_notes_before_auth` is not this boundary — `guardian.masm` consults
/// it only on the rotation path, where it must be zero, and the fee note is inside
/// the signed summary regardless.
pub fn user_output_notes(summary: &TransactionSummary) -> impl Iterator<Item = &RawOutputNote> {
    let injected = protocol_fee_note_index(summary);
    summary
        .output_notes()
        .iter()
        .enumerate()
        .filter(move |(index, _)| Some(*index) != injected)
        .map(|(_, raw)| raw)
}

/// The asset carried by the fee note [`user_output_notes`] excluded, if it
/// excluded one. Whatever is kept out of the listing's assets is reported here,
/// so exclusion never makes value disappear — which matters because a
/// transaction may commit a conversion rate that turns the fee into an
/// arbitrarily large outflow, bounded only by the vault.
///
/// A fee note carries exactly one asset; if that ever changes, the first is
/// reported rather than silently dropping the rest, since under-reporting a
/// fee is the failure this exists to prevent.
pub fn project_fee(summary: &TransactionSummary) -> Option<AssetSummary> {
    let index = protocol_fee_note_index(summary)?;
    summary
        .output_notes()
        .iter()
        .nth(index)?
        .assets()
        .iter()
        .next()
        .map(|asset| asset_summary_from_note_asset(asset, false))
}

pub fn project_note_counts(summary: &TransactionSummary) -> NoteCounts {
    NoteCounts {
        input: summary.input_notes().num_notes() as u32,
        output: user_output_notes(summary).count() as u32,
    }
}

/// Walk the transaction's own output notes and collect `(assets,
/// counterparty)` for the listing summary. Every asset on every such note is
/// included so multi-asset transactions are represented faithfully, but
/// protocol-injected fee notes are excluded. Counterparty is
/// left `None` for single-key pushes; the multisig path overrides it
/// from `proposal.recipient_id` upstream in `build_metadata`.
pub fn project_assets_and_counterparty_from_output_notes(
    summary: &TransactionSummary,
) -> (Vec<AssetSummary>, Option<CounterpartySummary>) {
    let assets: Vec<AssetSummary> = user_output_notes(summary)
        .flat_map(|note| {
            note.assets()
                .iter()
                .map(|asset| asset_summary_from_note_asset(asset, false))
                .collect::<Vec<_>>()
        })
        .collect();
    (assets, None)
}

/// Walk every input note and collect `(assets, counterparty)` for
/// consumption-style transactions. Every asset on every input note is
/// included. The first input note's original sender becomes the
/// counterparty with direction `in`.
pub fn project_assets_and_counterparty_from_input_notes(
    summary: &TransactionSummary,
) -> (Vec<AssetSummary>, Option<CounterpartySummary>) {
    let inputs = summary.input_notes();
    let assets: Vec<AssetSummary> = inputs
        .iter()
        .flat_map(|input_note| {
            input_note
                .note()
                .assets()
                .iter()
                .map(|asset| asset_summary_from_note_asset(asset, true))
                .collect::<Vec<_>>()
        })
        .collect();
    let counterparty = inputs.iter().next().map(|input_note| CounterpartySummary {
        account_id: account_id_hex(input_note.note().metadata().sender()),
        direction: CounterpartyDirection::In,
    });
    (assets, counterparty)
}

/// Return shape for [`decode_full`]: the five per-section vectors
/// projected from a persisted `TransactionSummary` in fixed order:
/// `(input_notes, output_notes, vault_changes, storage_changes, warnings)`.
pub type DecodedFullSections = (
    Vec<DecodedNote>,
    Vec<DecodedNote>,
    Vec<VaultChange>,
    Vec<StorageChange>,
    Vec<DecodeWarning>,
);

/// Decode the full detail-view projection from a persisted
/// `TransactionSummary`. Storage changes carry only `after`; `before`
/// would require reading storage at `prev_commitment`. MAST scripts
/// are not exposed.
pub fn decode_full(summary: &TransactionSummary) -> DecodedFullSections {
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
        note_type: note_visibility(note.metadata()),
        assets: note.assets().iter().map(decoded_asset_from).collect(),
        sender,
        recipient,
    }
}

fn decoded_note_from_partial_note(partial: &PartialNote) -> DecodedNote {
    DecodedNote {
        note_id: partial.id().to_hex(),
        tag: NoteTag::Custom,
        note_type: note_visibility(partial.metadata()),
        assets: partial.assets().iter().map(decoded_asset_from).collect(),
        sender: Some(account_id_hex(partial.metadata().sender())),
        recipient: None,
    }
}

fn note_visibility(metadata: &NoteMetadata) -> NoteVisibility {
    match metadata.note_type() {
        MidenNoteType::Public => NoteVisibility::Public,
        MidenNoteType::Private => NoteVisibility::Private,
    }
}

fn classify_note_tag(note: &Note) -> NoteTag {
    match StandardNote::from_script(note.script()) {
        Some(StandardNote::P2ID) => NoteTag::P2id,
        Some(StandardNote::P2IDE) => NoteTag::P2ide,
        Some(StandardNote::SWAP) => NoteTag::Pswap,
        Some(StandardNote::PSWAP) => NoteTag::Pswap,
        Some(StandardNote::MINT) => NoteTag::Mint,
        Some(StandardNote::BURN) => NoteTag::Burn,
        Some(StandardNote::CONSTANT_FEE_POLICY_CONFIG)
        | Some(StandardNote::FAUCET_POLICY_CONFIG)
        | Some(StandardNote::FAUCET_METADATA_CONFIG)
        | Some(StandardNote::MIN_BURN_AMOUNT_CONFIG)
        | Some(StandardNote::ALLOWLIST_CONFIG)
        | Some(StandardNote::BLOCKLIST_CONFIG)
        | Some(StandardNote::PAUSE_CONFIG)
        | Some(StandardNote::OWNER_CONFIG)
        | Some(StandardNote::RBAC_CONFIG)
        | Some(StandardNote::NETWORK_ACCOUNT_CONFIG)
        | Some(StandardNote::FEE_SPONSORSHIP)
        | Some(StandardNote::TX_FEE) => NoteTag::Custom,
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

    let mut fungible_net: BTreeMap<String, i128> = BTreeMap::new();
    for asset in vault.added_assets() {
        if let Asset::Fungible(a) = asset {
            *fungible_net.entry(a.faucet_id().to_hex()).or_insert(0) += a.amount().as_u64() as i128;
        }
    }
    for asset in vault.removed_assets() {
        if let Asset::Fungible(a) = asset {
            *fungible_net.entry(a.faucet_id().to_hex()).or_insert(0) -= a.amount().as_u64() as i128;
        }
    }
    for (asset_id, net) in fungible_net {
        if net == 0 {
            continue;
        }
        let change = if net > 0 {
            format!("+{net}")
        } else {
            format!("{net}")
        };
        out.push(VaultChange::Fungible { asset_id, change });
    }

    let mut nf_added: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut nf_removed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for asset in vault.added_assets() {
        if let Asset::NonFungible(a) = asset {
            let faucet = a.faucet_id().to_hex();
            let id = canonical_non_fungible_asset_id_hex(a);
            nf_added.entry(faucet).or_default().push(id);
        }
    }
    for asset in vault.removed_assets() {
        if let Asset::NonFungible(a) = asset {
            let faucet = a.faucet_id().to_hex();
            let id = canonical_non_fungible_asset_id_hex(a);
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

fn canonical_non_fungible_asset_id_hex(asset: NonFungibleAsset) -> String {
    format!("0x{}", hex::encode(asset.id().to_word().as_bytes()))
}

fn project_storage_changes(
    delta: &miden_protocol::account::delta::AccountDelta,
) -> Vec<StorageChange> {
    let storage = delta.storage();
    let mut out: Vec<StorageChange> = storage
        .values()
        .map(|(slot_name, value_patch)| StorageChange {
            slot_name: slot_name.as_str().to_string(),
            key: None,
            before: None,
            after: value_patch
                .value()
                .map(|word| format!("0x{}", hex::encode(word.as_bytes()))),
        })
        .collect();
    for (slot_name, map_patch) in storage.maps() {
        let Some(entries) = map_patch.entries() else {
            out.push(StorageChange {
                slot_name: slot_name.as_str().to_string(),
                key: None,
                before: None,
                after: None,
            });
            continue;
        };
        for (map_key, word) in entries.as_map() {
            out.push(StorageChange {
                slot_name: slot_name.as_str().to_string(),
                key: Some(format!("0x{}", hex::encode(map_key.as_bytes()))),
                before: None,
                after: (!word.is_empty()).then(|| format!("0x{}", hex::encode(word.as_bytes()))),
            });
        }
    }
    out
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use miden_protocol::account::AccountId;
    use miden_protocol::account::delta::{AccountDelta, AccountVaultDelta};
    use miden_protocol::asset::FungibleAsset;
    use miden_protocol::crypto::rand::RandomCoin;
    use miden_protocol::note::NoteType;
    use miden_protocol::transaction::InputNote;
    use miden_protocol::transaction::{
        InputNotes, RawOutputNotes, TransactionSummary, TransactionSummaryUserParams,
    };
    use miden_protocol::{Felt, Word, ZERO};
    use miden_standards::note::{P2idNote, TxFeeNote};

    const CONSUMER: &str = "0x2e2e2e2e2e2e2e012e2e2e2e2e2e2e";
    const NOTE_SENDER: &str = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
    const FAUCET: &str = "0x3f3f3f3e3f3f3f013f3f3f3f3f3f3f";

    fn summary_with_consumed_p2id_note() -> TransactionSummary {
        let sender = AccountId::from_hex(NOTE_SENDER).expect("sender");
        let consumer = AccountId::from_hex(CONSUMER).expect("consumer");
        let faucet = AccountId::from_hex(FAUCET).expect("faucet");
        let asset: miden_protocol::asset::Asset = FungibleAsset::new(faucet, 100_000_000)
            .expect("fungible asset")
            .into();
        let mut rng = RandomCoin::new(Word::from([1u32, 2, 3, 4]));
        let note = miden_protocol::note::Note::from(
            P2idNote::builder()
                .sender(sender)
                .target(consumer)
                .assets(vec![asset])
                .note_type(NoteType::Public)
                .generate_serial_number(&mut rng)
                .build()
                .expect("p2id note"),
        );
        let input = InputNote::unauthenticated(note);
        let delta = AccountDelta::new(
            consumer,
            miden_protocol::account::AccountStoragePatch::default(),
            AccountVaultDelta::default(),
            None,
            Felt::ZERO,
        )
        .expect("account delta");
        TransactionSummary::new(
            delta,
            InputNotes::new(vec![input]).expect("input notes"),
            RawOutputNotes::new(Vec::new()).expect("output notes"),
            Word::from([ZERO; 4]),
            0,
            TransactionSummaryUserParams::new([ZERO; 7]),
        )
    }

    /// A P2ID output note of `p2id_amount` plus the fee note `pay_fee` injects,
    /// as every transaction carries on a fee-charging chain.
    fn summary_with_p2id_output_and_fee_note(
        p2id_amount: u64,
        fee_amount: u64,
    ) -> TransactionSummary {
        let sender = AccountId::from_hex(CONSUMER).expect("sender");
        let target = AccountId::from_hex(NOTE_SENDER).expect("target");
        let faucet = AccountId::from_hex(FAUCET).expect("faucet");
        let mut rng = RandomCoin::new(Word::from([9u32, 8, 7, 6]));

        let p2id = miden_protocol::note::Note::from(
            P2idNote::builder()
                .sender(sender)
                .target(target)
                .assets(vec![miden_protocol::asset::Asset::from(
                    FungibleAsset::new(faucet, p2id_amount).expect("p2id asset"),
                )])
                .note_type(NoteType::Public)
                .generate_serial_number(&mut rng)
                .build()
                .expect("p2id note"),
        );
        let fee = miden_protocol::note::Note::from(
            TxFeeNote::builder()
                .sender(sender)
                .asset(FungibleAsset::new(faucet, fee_amount).expect("fee asset"))
                .generate_serial_number(&mut rng)
                .build()
                .expect("fee note"),
        );

        summary_with_notes(sender, Vec::new(), vec![p2id, fee])
    }

    fn summary_with_notes(
        account: AccountId,
        inputs: Vec<InputNote>,
        outputs: Vec<miden_protocol::note::Note>,
    ) -> TransactionSummary {
        let delta = AccountDelta::new(
            account,
            miden_protocol::account::AccountStoragePatch::default(),
            AccountVaultDelta::default(),
            None,
            Felt::ZERO,
        )
        .expect("account delta");
        TransactionSummary::new(
            delta,
            InputNotes::new(inputs).expect("input notes"),
            RawOutputNotes::new(outputs.into_iter().map(RawOutputNote::Full).collect())
                .expect("output notes"),
            Word::from([ZERO; 4]),
            0,
            TransactionSummaryUserParams::new([ZERO; 7]),
        )
    }

    fn summary_with_raw_outputs(
        account: AccountId,
        outputs: Vec<RawOutputNote>,
    ) -> TransactionSummary {
        let delta = AccountDelta::new(
            account,
            miden_protocol::account::AccountStoragePatch::default(),
            AccountVaultDelta::default(),
            None,
            Felt::ZERO,
        )
        .expect("account delta");
        TransactionSummary::new(
            delta,
            InputNotes::new(Vec::new()).expect("input notes"),
            RawOutputNotes::new(outputs).expect("output notes"),
            Word::from([ZERO; 4]),
            0,
            TransactionSummaryUserParams::new([ZERO; 7]),
        )
    }

    /// A note's tag is chosen by whoever builds it, and `createCustomProposal`
    /// executes caller-supplied request bytes as given. If the fee filter
    /// matched the reserved `0xfee` tag, a caller could hide a real asset
    /// movement from the listing by tagging it as a fee.
    #[test]
    fn a_caller_cannot_hide_an_output_note_by_tagging_it_as_a_fee() {
        let sender = AccountId::from_hex(CONSUMER).expect("sender");
        let faucet = AccountId::from_hex(FAUCET).expect("faucet");

        let disguised = PartialNote::new(
            miden_protocol::note::PartialNoteMetadata::new(sender, NoteType::Public)
                .with_tag(TxFeeNote::TAG),
            Word::from([1u32, 2, 3, 4]),
            miden_protocol::note::NoteAssets::new(vec![miden_protocol::asset::Asset::from(
                FungibleAsset::new(faucet, 1_000_000).expect("asset"),
            )])
            .expect("assets"),
            miden_protocol::note::NoteAttachments::default(),
        );

        let summary = summary_with_raw_outputs(sender, vec![RawOutputNote::Partial(disguised)]);

        assert_eq!(project_note_counts(&summary).output, 1);
        let (assets, _) = project_assets_and_counterparty_from_output_notes(&summary);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].amount.as_deref(), Some("-1000000"));
    }

    fn tx_fee_note(sender: AccountId, faucet: AccountId, amount: u64, seed: u32) -> Note {
        let mut rng = RandomCoin::new(Word::from([seed, seed, seed, seed]));
        Note::from(
            TxFeeNote::builder()
                .sender(sender)
                .asset(FungibleAsset::new(faucet, amount).expect("fee asset"))
                .generate_serial_number(&mut rng)
                .build()
                .expect("fee note"),
        )
    }

    /// `TxFeeNote::builder` is public and a `TX_FEE` note may be claimed by any
    /// account, so a caller who could get one excluded would be concealing value
    /// they can then collect. `pay_fee` appends its note last, so a `TX_FEE` note
    /// in any earlier position was not injected by the protocol.
    #[test]
    fn a_caller_built_fee_note_before_the_injected_one_stays_visible() {
        let sender = AccountId::from_hex(CONSUMER).expect("sender");
        let faucet = AccountId::from_hex(FAUCET).expect("faucet");

        let summary = summary_with_notes(
            sender,
            Vec::new(),
            vec![
                tx_fee_note(sender, faucet, 1_000_000, 21),
                tx_fee_note(sender, faucet, 50, 22),
            ],
        );

        assert_eq!(
            project_note_counts(&summary).output,
            2,
            "two TX_FEE notes are ambiguous, so neither may be excluded"
        );
        let (assets, _) = project_assets_and_counterparty_from_output_notes(&summary);
        assert!(
            assets
                .iter()
                .any(|a| a.amount.as_deref() == Some("-1000000")),
            "the caller's note must not be concealed"
        );
    }

    /// The trailing position is the whole signal, so a `TX_FEE` note that is not
    /// last is a caller's.
    #[test]
    fn a_fee_note_that_is_not_last_stays_visible() {
        let sender = AccountId::from_hex(CONSUMER).expect("sender");
        let target = AccountId::from_hex(NOTE_SENDER).expect("target");
        let faucet = AccountId::from_hex(FAUCET).expect("faucet");
        let mut rng = RandomCoin::new(Word::from([31u32, 32, 33, 34]));

        let p2id = Note::from(
            P2idNote::builder()
                .sender(sender)
                .target(target)
                .assets(vec![miden_protocol::asset::Asset::from(
                    FungibleAsset::new(faucet, 7).expect("p2id asset"),
                )])
                .note_type(NoteType::Public)
                .generate_serial_number(&mut rng)
                .build()
                .expect("p2id note"),
        );

        let summary = summary_with_notes(
            sender,
            Vec::new(),
            vec![tx_fee_note(sender, faucet, 1_000_000, 41), p2id],
        );

        assert_eq!(project_note_counts(&summary).output, 2);
    }

    /// Whatever leaves the asset list has to reappear here, or the listing
    /// becomes a place where value disappears.
    #[test]
    fn the_excluded_fee_is_reported_rather_than_dropped() {
        let summary = summary_with_p2id_output_and_fee_note(100, 50);
        let fee = project_fee(&summary).expect("the excluded note must be reported");
        assert_eq!(fee.amount.as_deref(), Some("-50"));
    }

    /// A custom proposal picks its own conversion rate, and `convert_amount` is
    /// bounded only by the vault, so a fee note can carry an arbitrarily large
    /// outflow. It is excluded from `assets` either way, which is exactly why it
    /// has to be reported.
    #[test]
    fn a_fee_inflated_by_the_conversion_rate_is_still_reported() {
        let summary = summary_with_p2id_output_and_fee_note(10, 950_000);
        let (assets, _) = project_assets_and_counterparty_from_output_notes(&summary);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].amount.as_deref(), Some("-10"));
        assert_eq!(
            project_fee(&summary).and_then(|f| f.amount),
            Some("-950000".to_string()),
            "a drain via the fee must not be invisible"
        );
    }

    #[test]
    fn nothing_is_reported_as_a_fee_when_nothing_was_excluded() {
        let sender = AccountId::from_hex(CONSUMER).expect("sender");
        let faucet = AccountId::from_hex(FAUCET).expect("faucet");
        let summary = summary_with_notes(
            sender,
            Vec::new(),
            vec![
                tx_fee_note(sender, faucet, 1_000_000, 51),
                tx_fee_note(sender, faucet, 50, 52),
            ],
        );
        assert_eq!(project_fee(&summary), None);
    }

    #[test]
    fn note_counts_exclude_the_protocol_fee_note() {
        let summary = summary_with_p2id_output_and_fee_note(100, 50);
        assert_eq!(summary.output_notes().num_notes(), 2);
        assert_eq!(project_note_counts(&summary).output, 1);
    }

    #[test]
    fn listing_assets_exclude_the_protocol_fee_note() {
        let summary = summary_with_p2id_output_and_fee_note(100, 50);
        let (assets, _) = project_assets_and_counterparty_from_output_notes(&summary);
        assert_eq!(
            assets.len(),
            1,
            "the fee must not be listed as a transferred asset"
        );
        assert_eq!(assets[0].amount.as_deref(), Some("-100"));
    }

    #[test]
    fn decode_full_still_shows_the_protocol_fee_note() {
        let summary = summary_with_p2id_output_and_fee_note(100, 50);
        let (_, outputs, _, _, _) = decode_full(&summary);
        assert_eq!(outputs.len(), 2, "the detail view keeps the fee note");
        assert_eq!(outputs[1].tag, NoteTag::Custom);
        assert_eq!(outputs[1].assets[0].amount.as_deref(), Some("50"));
    }

    /// Guards the consumer in `category.rs`: on a fee-charging chain a
    /// consume-only transaction still emits a fee note, and counting it as an
    /// output would report every consume as an asset transfer.
    #[test]
    fn category_inference_ignores_the_protocol_fee_note() {
        use crate::delta_summary::DashboardDeltaCategory;
        use crate::delta_summary::category::infer_category_from_summary;

        let account = AccountId::from_hex(CONSUMER).expect("consumer");
        let faucet = AccountId::from_hex(FAUCET).expect("faucet");
        let mut rng = RandomCoin::new(Word::from([4u32, 3, 2, 1]));
        let fee = miden_protocol::note::Note::from(
            TxFeeNote::builder()
                .sender(account)
                .asset(FungibleAsset::new(faucet, 50).expect("fee asset"))
                .generate_serial_number(&mut rng)
                .build()
                .expect("fee note"),
        );
        let consumed = match summary_with_consumed_p2id_note()
            .input_notes()
            .iter()
            .next()
        {
            Some(input) => input.clone(),
            None => unreachable!("fixture has one input note"),
        };

        let summary = summary_with_notes(account, vec![consumed], vec![fee]);

        assert_eq!(
            infer_category_from_summary(&summary),
            DashboardDeltaCategory::NoteConsumption
        );
    }

    #[test]
    fn project_input_notes_surfaces_consumed_assets_and_sender_counterparty() {
        let summary = summary_with_consumed_p2id_note();
        let (assets, counterparty) = project_assets_and_counterparty_from_input_notes(&summary);
        assert_eq!(assets.len(), 1);
        let asset = &assets[0];
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
            slot_name: "miden::standards::auth::multisig::threshold_config".to_string(),
            key: None,
            before: None,
            after: Some("0x0200".to_string()),
        };
        let json = serde_json::to_value(&change).expect("serializable");
        assert!(json.get("before").is_none());
        assert!(json.get("key").is_none());
        assert_eq!(json.get("after").and_then(|v| v.as_str()), Some("0x0200"));
    }

    #[test]
    fn project_storage_changes_emits_one_entry_per_map_key() {
        use miden_protocol::account::{
            AccountStoragePatch, StorageMapKey, StorageMapPatch, StorageSlotName, StorageSlotPatch,
        };

        let proc_root =
            Word::parse("0x6d30df4312a2c44ec842db1bee227cc045396ca91e2c47d756dcb607f2bf5f89")
                .expect("proc root");
        let threshold_word = Word::from([Felt::new_unchecked(1), ZERO, ZERO, ZERO]);

        let map_patch =
            StorageMapPatch::from_iters([], [(StorageMapKey::new(proc_root), threshold_word)]);

        let slot_name =
            StorageSlotName::new("miden::standards::auth::multisig::procedure_thresholds").unwrap();
        let storage =
            AccountStoragePatch::from_raw([(slot_name, StorageSlotPatch::Map(map_patch))].into())
                .expect("storage patch");
        let delta = AccountDelta::new(
            AccountId::from_hex(CONSUMER).expect("acct"),
            storage,
            AccountVaultDelta::default(),
            None,
            Felt::new_unchecked(1),
        )
        .expect("delta");

        let changes = project_storage_changes(&delta);
        assert_eq!(changes.len(), 1);
        let c = &changes[0];
        assert_eq!(
            c.slot_name,
            "miden::standards::auth::multisig::procedure_thresholds"
        );
        assert_eq!(
            c.key.as_deref(),
            Some("0x6d30df4312a2c44ec842db1bee227cc045396ca91e2c47d756dcb607f2bf5f89")
        );
        assert!(c.after.is_some());
    }

    #[test]
    fn project_storage_changes_represents_cleared_map_entry_as_removal() {
        use miden_protocol::account::{
            AccountStoragePatch, StorageMapKey, StorageMapPatch, StorageSlotName, StorageSlotPatch,
        };

        let proc_root =
            Word::parse("0x6d30df4312a2c44ec842db1bee227cc045396ca91e2c47d756dcb607f2bf5f89")
                .expect("proc root");
        let no_updates: [(StorageMapKey, Word); 0] = [];

        let map_patch = StorageMapPatch::from_iters([StorageMapKey::new(proc_root)], no_updates);

        let slot_name =
            StorageSlotName::new("miden::standards::auth::multisig::procedure_thresholds").unwrap();
        let storage =
            AccountStoragePatch::from_raw([(slot_name, StorageSlotPatch::Map(map_patch))].into())
                .expect("storage patch");
        let delta = AccountDelta::new(
            AccountId::from_hex(CONSUMER).expect("acct"),
            storage,
            AccountVaultDelta::default(),
            None,
            Felt::new_unchecked(1),
        )
        .expect("delta");

        let changes = project_storage_changes(&delta);
        assert_eq!(changes.len(), 1);
        let c = &changes[0];
        assert_eq!(
            c.key.as_deref(),
            Some("0x6d30df4312a2c44ec842db1bee227cc045396ca91e2c47d756dcb607f2bf5f89")
        );
        assert!(c.after.is_none());
    }

    #[test]
    fn project_vault_changes_uses_canonical_non_fungible_asset_id() {
        let account_id = AccountId::from_hex(CONSUMER).expect("acct");
        let asset = NonFungibleAsset::mock(b"guardian-dashboard-canonical-id");
        let faucet_id = match asset {
            Asset::NonFungible(asset) => asset.faucet_id().to_hex(),
            Asset::Fungible(_) => unreachable!("mock should create a non-fungible asset"),
        };
        let mut vault = AccountVaultDelta::default();
        vault.add_asset(asset).expect("asset delta");
        let delta = AccountDelta::new(
            account_id,
            miden_protocol::account::AccountStoragePatch::default(),
            vault,
            None,
            Felt::ONE,
        )
        .expect("account delta");

        let changes = project_vault_changes(&delta);

        assert_eq!(
            changes,
            vec![VaultChange::NonFungible {
                asset_id: faucet_id,
                added: vec![
                    "0xf1433e1e588f04cbcbee98fc5f0c2ab600ef000000dd000011ca0000000000bc"
                        .to_string(),
                ],
                removed: Vec::new(),
            }]
        );
    }
}
