# Data model: per-signer schemes

## Rust (`crates/server/src/metadata/auth/mod.rs`)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct MidenCosigner {
    pub commitment: String,          // canonical 0x + 64 lowercase hex (validated)
    pub scheme: SignatureScheme,     // serde snake_case: "falcon" | "ecdsa"
}

pub enum Auth {
    MidenFalconRpo { cosigner_commitments: Vec<String> },   // unchanged
    MidenEcdsa     { cosigner_commitments: Vec<String> },   // unchanged
    /// Per-signer schemes and an independent guardian scheme (issue #539).
    Miden { cosigners: Vec<MidenCosigner>, guardian_scheme: SignatureScheme },
    EvmEcdsa { signers: Vec<String> },                      // unchanged
}
```

Accessor changes (all variants):

| Method | Today | After |
| --- | --- | --- |
| `scheme()` | single account scheme | **removed**; callers use one of the three below |
| `guardian_scheme()` | – | legacy ⇒ its scheme; `Miden` ⇒ field; EVM ⇒ `Ecdsa` |
| `schemes_in_use()` | – | distinct cosigner schemes ∪ guardian scheme (allowlist) |
| `auth_scheme_label()` | – | `"falcon"` / `"ecdsa"` / `"mixed"` (D7) |
| `method_label()` | `miden_falcon` / `miden_ecdsa` / `evm` | + `"miden"` |
| `cosigner_commitments()` | `&[String]` | `Vec<String>` (owned; `Miden` has no string slice) |
| `cosigners()` | – | `Vec<MidenCosigner>` for every Miden variant (legacy ⇒ all one scheme) |
| `signer_scheme(&commitment)` | – | `Option<SignatureScheme>` |
| `with_updated_commitments(Vec<String>)` | keeps variant | **replaced by** `with_chain_cosigners(Vec<ChainCosigner>) -> Auth` implementing D6 (`ChainCosigner { commitment, scheme: Option<SignatureScheme> }`) |
| `compute_signer_commitment()` | per variant | `Miden` arm per D4 |
| `validate_canonical_signer_set()` | commitments | same checks over `cosigner_commitments()`; duplicates across schemes rejected |
| `verify()` | per variant | `Miden` arm per D3, reusing `miden_falcon_rpo::verify_request_signature` and `miden_ecdsa::{verify_request_signature, verify_eip712_request_signature}` with the scheme-filtered commitment list |

`TryFrom<AuthConfig>` parses `MidenAuth`: each `MidenCosigner.scheme` and `guardian_scheme` via
`SignatureScheme::from` (case-insensitive); an unknown name is an error (`invalid_input`).

## Wire

HTTP JSON (externally tagged, additive):

```json
{ "Miden": { "cosigners": [ { "commitment": "0x…", "scheme": "ecdsa" },
                            { "commitment": "0x…", "scheme": "falcon" } ],
             "guardian_scheme": "ecdsa" } }
```

gRPC (both `crates/server/proto/guardian.proto` and `crates/client/proto/guardian.proto`; the
copies differ today, add the same messages to each):

```proto
message MidenCosigner { string commitment = 1; string scheme = 2; }   // "falcon" | "ecdsa"
message MidenAuth { repeated MidenCosigner cosigners = 1; string guardian_scheme = 2; }
message AuthConfig { oneof auth_type { … ; MidenAuth miden = 4; } }
// AccountState.auth_scheme comment: "falcon", "ecdsa" or "mixed"
```

`StateObject.auth_scheme` (HTTP `GET /state`, gRPC `AccountState.auth_scheme`) and
`DashboardAccountSummary/Detail.auth_scheme`: `"falcon" | "ecdsa" | "mixed"`.
`DashboardAuthMethodSignerCount.auth_method`: `miden_falcon | miden_ecdsa | miden | evm`.

Proposal payload metadata (client-defined, passed through opaque; lifted into delta metadata):

```json
{ "proposal_type": "add_signer", "target_threshold": 2,
  "signer_commitments": ["0xA", "0xB"], "signer_schemes": ["falcon", "ecdsa"] }
```

`delta_summary::ProposalMetadata.signer_schemes: Vec<String>` (`#[serde(default,
skip_serializing_if = "Vec::is_empty")]`), index-aligned with `signer_commitments`; lifted by the
existing serde decode in `delta_summary/build.rs::decode_proposal_metadata`.
`packages/guardian-client`: `ServerProposalMetadata.signer_schemes?: string[]` and the camelCase
mirror; `crates/miden-multisig-client/src/payload.rs::ProposalMetadataPayload.signer_schemes`.

## Storage

- `account_metadata.auth` JSONB / filesystem `accounts.json`: serde shape above. No row migration.
- Postgres lookup index migration `crates/server/migrations/2026-10-20-000001_miden_auth_cosigner_index/`
  (a second index; the existing one is untouched so old replicas keep using it):

```sql
-- up.sql
CREATE INDEX idx_account_metadata_miden_cosigners
ON account_metadata USING GIN ((auth -> 'Miden' -> 'cosigners') jsonb_path_ops);
-- down.sql
DROP INDEX IF EXISTS idx_account_metadata_miden_cosigners;
```

  Query (`metadata/postgres.rs::COSIGNER_LOOKUP_SQL`), each arm equal to its index expression:

```sql
WHERE COALESCE(<legacy arms>, '[]'::jsonb) @> to_jsonb($1::text)
   OR (auth -> 'Miden' -> 'cosigners') @> jsonb_build_array(jsonb_build_object('commitment', $1::text))
```

- Replay state (`account_auth_state`, keyed by `(account_id, signer_commitment)`) is unchanged;
  the filesystem expansion in `metadata/filesystem.rs:221` already iterates
  `cosigner_commitments()`.

## Network layer

- `NetworkClient::validate_guardian_commitment(state, expected_commitment, expected_scheme)`:
  also requires the guardian scheme slot to store `expected_scheme`.
- `AppliedState.cosigners: Vec<ChainCosigner>` (replaces `cosigner_commitments`), read in the
  same decode as the state; `updated_auth` feeds it to `with_chain_cosigners`.
- `AuthBinding { signers: Vec<ChainCosigner> (sorted by commitment), guardian, guardian_scheme }`;
  the refusal log reports `signers_changed`, `signer_schemes_changed`, `guardian_changed` and
  `guardian_scheme_changed`.

## Account inspector (`crates/server/src/network/miden/account_inspector.rs`)

- `extract_approver_schemes() -> Vec<Option<SignatureScheme>>` from
  `AuthGuardedMultisig::approver_scheme_ids_slot()` (`[i,0,0,0] => [scheme_id,0,0,0]`), same index
  walk as `extract_map_pubkeys`, stopping at the first default word.
- `extract_guardian_scheme() -> Option<SignatureScheme>` from
  `AuthGuardedMultisig::guardian_scheme_id_slot()` key `[0,0,0,0]`.
- `SignatureScheme::from_auth_scheme_id(u64) -> Option<SignatureScheme>` (shared crate), the
  inverse of `auth_scheme_id` (Falcon512Poseidon2 = 2, EcdsaK256Keccak = 1).
- `extract_chain_cosigners() -> Vec<ChainCosigner>` zipping keys and schemes; a missing or unknown
  scheme entry reads as `None` (see D6).

## Contracts crate (`crates/contracts/src/multisig_guardian.rs`)

`MultisigGuardianConfig` gains `signer_schemes: Vec<SignatureScheme>` (empty = every signer uses
`signature_scheme`; otherwise index-aligned with `signer_commitments`, validated in
`validate_config`) and `guardian_scheme: Option<SignatureScheme>` (None = `signature_scheme`).
`build_guarded_multisig_component` passes each approver its own `AuthScheme` and the guardian its
own. Builders: `with_signer_schemes(Vec<SignatureScheme>)`, `with_guardian_scheme(SignatureScheme)`.
