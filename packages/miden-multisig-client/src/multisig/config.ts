/**
 * Whether this build accepts v1 `consume_notes` metadata (issue #229).
 * Mirrors the Rust `legacy-consume-notes` Cargo feature. Flip to `false`
 * in the cut-over release; v1 proposals will then be refused with
 * `UnsupportedMetadataVersionError`.
 */
export const LEGACY_CONSUME_NOTES_ENABLED = true;
