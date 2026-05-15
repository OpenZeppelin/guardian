# Data Model: Self-Contained consume_notes Proposal Verification

**Feature Key**: `006-consume-notes-metadata` | **Date**: 2026-05-14

This feature does not introduce any persistent server-side entity.
All shape changes live inside the multisig client's
proposal-metadata payload, which the Guardian server stores as
opaque bytes.

## Proposal metadata shape

The `consume_notes` proposal metadata is discriminated by
`proposalType: 'consume_notes'`. This feature adds a second-level
discriminator `metadataVersion` (TS) / `metadata_version` (Rust)
that gates which schema variant is in use.

### v1 — Legacy (existing)

```jsonc
{
  "proposalType": "consume_notes",       // discriminator (unchanged)
  "noteIds": ["0xabc...", "0xdef..."]    // list of note IDs being consumed
  // metadataVersion is absent
  // (other shared proposal-metadata fields elided)
}
```

- Marked legacy by **absence** of `metadataVersion` (FR-003).
- Verification rebuild requires `getInputNote` / `get_input_note`
  against the verifier's local store. Fails with
  `consume_notes_legacy_note_missing` if absent.
- Accepted by transitional clients (Release N). Refused by
  cut-over clients (Release N+1) with
  `consume_notes_unsupported_metadata_version`.

### v2 — New (this feature)

```jsonc
{
  "proposalType": "consume_notes",
  "metadataVersion": 2,                   // NEW: discriminator (FR-002)
  "noteIds": ["0xabc...", "0xdef..."],    // preserved (FR-004)
  "notes": [                              // NEW: embedded note bytes (FR-001)
    "base64(Note.serialize() for noteIds[0])",
    "base64(Note.serialize() for noteIds[1])"
  ]
}
```

Invariants enforced at verification time:
- `notes.length === noteIds.length`. Otherwise:
  `consume_notes_note_binding_mismatch`.
- For each `i`: `Note.deserialize(base64decode(notes[i])).id().toHex() === noteIds[i]`.
  Otherwise: `consume_notes_note_binding_mismatch` (FR-007).
- Serialized JSON metadata size at proposal-creation time
  ≤ `MAX_CONSUME_NOTES_METADATA_BYTES` (256 KiB). Otherwise:
  `consume_notes_metadata_oversize` (FR-011).

## Type declarations

### Rust (`crates/miden-multisig-client/src/proposal.rs`)

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "proposalType", rename_all = "snake_case")]
pub enum ProposalMetadata {
    // ... other variants unchanged ...
    ConsumeNotes(ConsumeNotesProposalMetadata),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsumeNotesProposalMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_version: Option<u32>,            // None => v1, Some(2) => v2
    pub note_ids: Vec<NoteId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<SerializedNote>,               // empty on v1
    // ... shared base fields ...
}

/// Newtype around base64-encoded `Note::to_bytes()` output.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SerializedNote(pub String);

impl SerializedNote {
    pub fn from_note(note: &Note) -> Self { /* ... */ }
    pub fn to_note(&self) -> Result<Note, ProposalError> { /* ... */ }
}

pub const MAX_CONSUME_NOTES_METADATA_BYTES: usize = 256 * 1024;
```

### TypeScript (`packages/miden-multisig-client/src/types/proposal.d.ts`)

```ts
export interface ConsumeNotesProposalMetadata extends BaseProposalMetadata {
    proposalType: 'consume_notes';
    /** Discriminator. Absent => v1 (legacy); 2 => v2 (this feature). */
    metadataVersion?: 2;
    /** Canonical note identifiers (hex strings). Present in both shapes. */
    noteIds: string[];
    /**
     * v2 only: base64-encoded note.serialize() output, aligned by index
     * with noteIds. Empty on v1.
     */
    notes?: string[];
}

export const MAX_CONSUME_NOTES_METADATA_BYTES = 256 * 1024;
```

## Builder API

### Rust

```rust
// Pure: used by v2 verification + execution.
pub fn build_consume_notes_request_from_notes(
    notes: &[Note],
    salt: Word,
    options: SignatureOptions,
) -> TransactionRequest;

// Adapter: used by proposal creation and v1 legacy path only.
pub async fn build_consume_notes_request_from_client(
    client: &mut MidenClient,
    note_ids: &[NoteId],
    salt: Word,
    options: SignatureOptions,
) -> Result<TransactionRequest, ProposalError>;
```

### TypeScript

```ts
// Pure: used by v2 verification + execution.
export function buildConsumeNotesTransactionRequestFromNotes(
    notes: Note[],
    options?: SignatureOptions,
): { request: TransactionRequest; salt: Word };

// Adapter: used by proposal creation and v1 legacy path only.
export async function buildConsumeNotesTransactionRequest(
    client: MidenClient | WasmWebClient,
    noteIds: string[],
    options?: SignatureOptions,
): Promise<{ request: TransactionRequest; salt: Word }>;
```

## Error taxonomy

All four error identifiers are introduced by this feature and
share string codes across Rust and TS (FR-021, FR-022).

| Code | Condition | Surface |
|------|-----------|---------|
| `consume_notes_note_binding_mismatch` | v2 metadata: `notes.length !== noteIds.length`, or any `Note.id() !== noteIds[i]` | Rust `ProposalError::NoteBindingMismatch`; TS `NoteBindingMismatchError` |
| `consume_notes_unsupported_metadata_version` | `metadataVersion` is a value the client does not support (including v1 on a cut-over build) | Rust `ProposalError::UnsupportedMetadataVersion { found: Option<u32> }`; TS `UnsupportedMetadataVersionError` |
| `consume_notes_metadata_oversize` | serialized v2 metadata exceeds `MAX_CONSUME_NOTES_METADATA_BYTES` at creation time | Rust `ProposalError::ConsumeNotesMetadataOversize { limit, actual }`; TS `ConsumeNotesMetadataOversizeError` (`limit` + `actual` fields) |
| `consume_notes_legacy_note_missing` | v1 path: `get_input_note` returned `None` for at least one declared `note_id` | Rust `ProposalError::LegacyConsumeNotesNoteMissing { note_id }`; TS `LegacyConsumeNotesNoteMissingError` (`noteId` field) |

## Version dispatch state machine

```
verifyProposalMetadataBinding(proposal):
    md = proposal.metadata
    match md.proposalType:
        ... other types unchanged ...
        case 'consume_notes':
            match md.metadataVersion:

                undefined | 1:
                    if LEGACY_PATH_ENABLED:
                        notes = []
                        for id in md.noteIds:
                            n = client.getInputNote(id)
                            if n is null:
                                throw LegacyConsumeNotesNoteMissingError(id)
                            notes.push(n.toNote())
                        return buildFromNotes(notes, salt)
                    else:                            // cut-over build
                        throw UnsupportedMetadataVersionError(undefined)

                2:
                    if md.notes.length != md.noteIds.length:
                        throw NoteBindingMismatchError
                    notes = []
                    for i in 0..md.notes.length:
                        n = Note.deserialize(base64decode(md.notes[i]))
                        if n.id().toHex() != md.noteIds[i]:
                            throw NoteBindingMismatchError
                        notes.push(n)
                    return buildFromNotes(notes, salt)

                _:
                    throw UnsupportedMetadataVersionError(md.metadataVersion)
```

## Lifecycle impact

The proposal lifecycle (create → sign → threshold → execute →
canonicalize) is structurally unchanged. The only changes are
inside the verification and execution rebuild steps:

- **Create**: now also serializes `notes` and stamps
  `metadataVersion = 2`.
- **Verify (before signature)**: dispatches by version; the v2 path
  does no local-store read.
- **Sign**: unchanged. The cosigner signature is still over the
  same `TransactionSummary` commitment.
- **Execute**: dispatches by version; the v2 path does no
  local-store read. The executed transaction is bit-identical to
  the one cosigners signed against (FR-013).
- **Canonicalize**: unchanged. The server sees the same
  `delta_proposal` row shape.
