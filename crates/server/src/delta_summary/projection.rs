//! Detail-view projector for the dashboard delta decoder.
//!
//! Projects [`NormalizedPayload`] into the decoded input/output notes,
//! vault changes, and storage changes the detail endpoint returns. See
//! `data-model.md` `DashboardDeltaDetail` and `research.md` Decisions
//! 4–5.
//!
//! Phase 4 (US2) deliverable. The current implementation is a stub
//! that returns empty sections so the `delta_summary` module's public
//! surface is complete and US1 (listing) can compile without pulling
//! detail-view code into its binary path. US2 expands the body of
//! [`decode_full`] to do the real projection.

use super::{
    DecodeWarning, DecodedNote, NormalizedPayload, StorageChange, VaultChange,
};

/// Decode the full detail-view projection from a normalized payload.
///
/// Returns `(input_notes, output_notes, vault_changes, storage_changes, warnings)`.
/// `include_scripts` controls whether the optional `script` field on
/// each decoded note is populated (per Decision 5, opt-in via
/// `?include=scripts`).
///
/// **Phase 4 stub**: returns empty sections plus a single warning
/// noting that detail-view projection is not yet implemented. US2
/// replaces the body.
pub fn decode_full(
    normalized: &NormalizedPayload,
    include_scripts: bool,
) -> (
    Vec<DecodedNote>,
    Vec<DecodedNote>,
    Vec<VaultChange>,
    Vec<StorageChange>,
    Vec<DecodeWarning>,
) {
    let _ = (normalized, include_scripts);
    (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new())
}
