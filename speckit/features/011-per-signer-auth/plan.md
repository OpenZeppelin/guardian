# Implementation plan: per-signer schemes (PR A: server + contracts + base clients + docs)

Read `spec.md` (decisions D1–D14) and `data-model.md` first. Status: sections 1–5 and 7 are done
on the branch; section 6 lists the follow-up PRs. Where this plan and `data-model.md` differ,
`data-model.md` describes what was built (separate lookup index, total chain-sync rule,
`AppliedState` schemes, scheme-aware sessions and queue binding). Work bottom-up per `AGENTS.md`
§4. Keep legacy variants' code paths untouched; add arms rather than refactoring.

## 0. Prerequisites and ground rules

- Worktree `guardian-worktrees/539-server-per-signer-schemes`, branch
  `539-server-per-signer-schemes` off `origin/main` (ca503704). Do not touch
  `packages/miden-multisig-client` in PR A (PR #540 is open there; it is PR C's job).
- macOS: cargo link needs the Xcode-license workaround (pin `CC`/`AR`/linker to
  CommandLineTools; see memory `macos-xcode-license-cargo-workaround`). Node 24 for TS tests.
- No Claude attribution lines in commits or PR text. Never commit live smoke harnesses.

## 1. Shared + contracts

1. `crates/shared/src/lib.rs`: nothing new required; `SignatureScheme` already has `from`,
   `as_str`, `auth_scheme_id`, serde snake_case. Add `SignatureScheme::from_auth_scheme_id(u64)
   -> Option<Self>` next to `auth_scheme_id` for the inspector.
2. `crates/contracts/src/multisig_guardian.rs`: `signer_schemes` + `guardian_scheme` per
   data-model; `validate_config` rejects a length mismatch; unit tests build a Falcon+ECDSA
   account and read back `approver_scheme_ids_slot` / `guardian_scheme_id_slot` entries. Existing
   callers (`generate_fixtures.rs`, `apply_delta_bench.rs`, e2e tests, Rust SDK) compile unchanged.

## 2. Server core (`crates/server`)

3. `src/metadata/auth/mod.rs`: `MidenCosigner`, `Auth::Miden`, accessors and `verify` arm per
   data-model. Implementation notes:
   - D3 raw dispatch: `hex::decode(signature)`; `len == 1524` ⇒ Falcon; `len == 65` ⇒ ECDSA
     (reuse the constant from `lookup.rs`, lift it to the module root). Build the filtered
     commitment `Vec<String>` per scheme and pass it to the existing verifiers; they already
     return the signer commitment.
   - EIP-712 on a `Miden` account ⇒ ECDSA path with the ECDSA list; on a Falcon-only `Miden`
     account the list is empty and verification fails with "not authorized".
   - `with_cosigners` (D6) lives here so promotion is unit-tested without a network client.
   - `validate_canonical_signer_set` keeps the exact error strings for legacy variants.
   - Tests: serde round-trip for the three Miden variants and the legacy JSON fixtures (assert
     identical bytes); `TryFrom<AuthConfig>` happy/unknown-scheme; `compute_signer_commitment`
     for commitment / ECDSA pubkey / Falcon pubkey / garbage; `verify` with
     `testing::helpers::{TestSigner, TestEcdsaSigner}`: Falcon raw ok, ECDSA raw ok, ECDSA
     EIP-712 ok, cross-scheme attempts fail, legacy variants unchanged; `with_cosigners`
     promotion matrix; `auth_scheme_label` / `method_label` / `schemes_in_use`.
4. `src/network/miden/account_inspector.rs`: `extract_approver_schemes`,
   `extract_guardian_scheme`, `extract_cosigners` per data-model, with tests on an account built
   by the contracts crate (mixed) and on the existing fixture (uniform).
5. `src/network/mod.rs` + `src/network/miden/mod.rs` (+ the mock impls in
   `src/testing/mocks.rs`, `jobs/release_sweep/worker.rs`, `jobs/canonicalization/processor.rs`
   test doubles):
   - `validate_guardian_commitment(state, expected_commitment, expected_scheme: SignatureScheme)`:
     also read `extract_guardian_scheme()` and reject when it differs (message names both). Mock:
     add `with_guardian_scheme_result` or make the mock ignore the scheme; keep existing tests green.
   - `should_update_auth`: `inspector.extract_cosigners()?` then `current_auth.with_cosigners(..)`;
     empty key map ⇒ `Ok(None)` as today.
   - `validate_credential` is unchanged (dispatch is inside `compute_signer_commitment`).
6. `src/services/configure_account.rs`:
   - allowlist: `for scheme in params.auth.schemes_in_use() { reject_disallowed_scheme(..)? }`
     (first-time registrations only, as today);
   - `let guardian_scheme = params.auth.guardian_scheme();` for `expected_guardian_commitment`,
     `validate_guardian_commitment(.., guardian_scheme)`, `ack_pubkey` / `ack_commitment`;
   - `StateObject.auth_scheme = params.auth.auth_scheme_label()`;
   - mismatch error: "declared auth does not match the signer set or schemes in initial_state:
     expected {expected:?}, provided {provided:?}" (keep `cosigner_commitments` in the text so the
     existing assertion `msg.contains("cosigner_commitments")` still holds, or update that test).
   - Tests: mixed happy path with `MockNetworkClient::with_should_update_auth(Ok(Some(Miden{..})))`;
     allowlist rejects on the Falcon cosigner of a `Miden{falcon, ecdsa}` set with `meta.scheme =
     "falcon"`; guardian scheme slot mismatch rejected; legacy tests untouched.
7. Replace every `metadata.auth.scheme()`: `push_delta.rs:112` (ack + `ack_scheme` string ⇒
   `guardian_scheme`), `release_on_switch.rs:254`, `jobs/release_sweep/sweep.rs:664`,
   `get_state.rs:40` (`auth_scheme_label`), `dashboard_accounts.rs:261,286`
   (`auth_scheme_label`), `dashboard_accounts.rs::normalized_authorized_signer_ids` and
   `dashboard/stats.rs::normalized_authorized_signer_count` (use `cosigner_commitments()`),
   `metadata/filesystem.rs::find_by_cosigner_commitment` (iterate `cosigners()` for Miden
   variants, skip EVM). `rg 'auth\.scheme\(\)' crates/server` must end empty.
8. `src/metadata/postgres.rs::find_by_cosigner_commitment` + migration
   `2026-10-21-000001_miden_auth_lookup_index/{up,down}.sql` per data-model. Keep the comment
   that the expression text must equal the index. Add an ignored live test under a module path
   containing `postgres` that inserts a `Miden` row and resolves it by a cosigner commitment
   (`scripts/test-postgres.sh` discovers it).
9. `src/delta_summary/mod.rs`: `signer_schemes` field; `build.rs` test extends the
   `add_signer` case with `signer_schemes` and asserts the lift; a payload without the key lifts
   an empty vec.
10. `proto/guardian.proto` (server) and `crates/client/proto/guardian.proto`: `MidenCosigner`,
    `MidenAuth`, `miden = 4`, and the `auth_scheme` comment. `crates/client` re-exports
    `proto::*`, so no further Rust client code is needed; add a `client_tests.rs` case that
    builds `AuthType::Miden(..)` and round-trips `configure` against the test server, signed by a
    Falcon signer, then `get_state` signed by the ECDSA signer (`crates/client` exports `FalconRpoSigner` and `EcdsaSigner` from `src/auth/`).
11. `src/api/grpc.rs` / `src/api/http.rs`: no handler changes; confirm `#[utoipa::path]` bodies
    pick up the new `oneOf` arm and regenerate:
    `cargo run --features evm --bin gen-openapi -- docs` (CI checks drift with `--check`).

## 3. Server tests beyond unit

12. New e2e `src/testing/e2e/mixed_scheme_account.rs` (register in `e2e/mod.rs`, feature `e2e`,
    pattern of `switch_guardian_canonicalization.rs`): build the mixed account with the contracts
    crate (guardian = `state.ack.commitment(&Ecdsa)`, `with_guardian_scheme(Ecdsa)`,
    `with_signer_schemes([Falcon, Ecdsa])`, `build_existing()`), wrap the real
    `MidenNetworkClient::lazy_for_test` in `IntegrationMockNetworkClient`, then drive spec
    scenarios 1–7 through `configure_account`, `resolve_account`/`get_state`, a canonicalized
    delta that adds a Falcon approver to a `MidenEcdsa` account (promotion), and the legacy
    declaration rejection.
13. Integration (feature `integration`): `auth_http.rs` and `auth_grpc.rs` cases for a `Miden`
    account: ECDSA raw, ECDSA EIP-712 (`x-auth-format: eip712`), Falcon raw, and the two
    cross-scheme failures with `authentication_failed`; `lookup_http.rs`/`lookup_grpc.rs`
    resolve a `Miden` row by both cosigners.
14. Run the full CI matrix locally before pushing (memory `run-ci-feature-gated-tests-after-rebase`):
    `cargo nextest run -p guardian-server`, the `integration` and `e2e` feature runs, `authz`,
    `scripts/test-postgres.sh`, `cargo clippy --all-targets --all-features -- -D warnings`,
    `cargo fmt --check`, plus `crates/client`, `crates/contracts`, `crates/qualification-driver`,
    `crates/server/bench/loadgen`, `benchmarks/prod-server` compile (they construct `Auth`/proto).

## 4. TypeScript base client (`packages/guardian-client`)

15. `src/types.ts` `AuthConfig` and `src/server-types.ts` `ServerAuthConfig`: add
    `{ Miden: { cosigners: { commitment: string; scheme: SignatureScheme }[]; guardian_scheme:
    SignatureScheme } }`. `ServerProposalMetadata.signer_schemes?: string[]` and the camelCase
    mirror in `ProposalMetadata` + `conversion.ts` both directions. `StateObject.authScheme`
    doc: `"falcon" | "ecdsa" | "mixed"`. Tests in `conversion.test.ts` / `http.test.ts`
    (configure with the new variant serialises the exact JSON; metadata round-trips
    `signer_schemes`). The error-code drift guard needs no change (no new codes). Run every
    `packages/*` suite (memory `run-all-ts-packages-on-contract-changes`).

## 5. Docs (PR A)

16. `spec/api.md`: AuthConfig section (third variant + the "clients send `Miden` only for
    non-uniform sets" rule), `auth_scheme` values, gRPC `AuthConfig::{…, miden}`, proposal
    metadata `signer_schemes`, lookup note that Miden-variant cosigners are matched.
17. `docs/CONFIGURATION.md` / `docs/TROUBLESHOOTING.md` row for `signature_scheme_not_allowed`:
    every scheme in a new account's set (each cosigner and the guardian) must be allowed.
18. `docs/MIDEN_COMPATIBILITY.md` (or the server↔SDK compatibility table used by the production
    guide): mixed-scheme accounts need a server with this feature; older servers reject the
    `Miden` variant. `docs/DASHBOARD.md`/OpenAPI description of `auth_method` labels: add `miden`.
19. CHANGELOG/release notes if the repo keeps them for the next release PR (none at the root
    today; the release PR template carries notes).

## 5b. Review round (#565)

20. Merge `main` (#536 push gate, #561): route the e2e promotion test through
    `push_delta_proposal` with the ECDSA signer's approval, then push; extend it with a second
    transaction on the promoted mixed account approved by the Falcon signer alone (refused
    without a proposal, acknowledged with one, ECDSA ack).
21. `ProposalSignature::scheme()` (shared); `services/proposal_signature.rs::ensure_cosigner_approval`
    called from `sign_delta_proposal` and `push_delta_proposal`; `multisig_admission.rs`
    `verified_approver_count` over `extract_chain_cosigners()` (D16).
22. `NetworkClient::validate_approver_schemes` (default `Ok`, Miden impl, mocks), called in
    `configure_account` after the GUARDIAN binding (D15).
23. Allowlist on reconfiguration (D9), `packages/guardian-operator-client` `signerSchemes` on
    `DashboardDeltaProposalMetadata`; HTTP scheme names stay lowercase (D17).
24. Verify skills: model review and `--accept-drift` for `verify-auth-replay`,
    `verify-guardian-switch` and `verify-multisig-push-gate` (code maps updated for the scheme
    checks), plus `--deep` / `--mutants` for the push gate.

## 6. Follow-up PRs (not in PR A)

- **PR B, Rust SDK (`crates/miden-multisig-client`), closes #549**: `MultisigAccount::cosigners()
  -> Vec<(Word, SignatureScheme)>` and `guardian_scheme()` readers (same slots as the server
  inspector); `register_on_guardian` sends the legacy variant when uniform and `AuthType::Miden`
  otherwise (D2); `build_multisig_config_advice(threshold, &[(Word, SignatureScheme)])` with
  callers (`transaction/configuration/config.rs:94`, add/remove/change-threshold builders) reading
  existing signers' schemes from the account and giving a new signer the key manager's scheme (or
  an explicit `SignerSpec`-like argument); `ProposalMetadataPayload.signer_schemes`;
  `build_update_guardian_script` and the switch-guardian `/pubkey` query use the account's
  guardian scheme, not the key manager's; `docs/MULTISIG_SDK.md` Rust sections. Rebuild of an
  update-signers proposal by a cosigner must use `signer_schemes` from the proposal metadata when
  present, else the proposer's single scheme (today's behaviour).
- **PR C, TS SDK (after #540 merges)**: drop the `SignerSchemeMismatchError` refusals in
  `registerOnGuardian`, add/remove/change-threshold and switch-guardian; `registerOnGuardian`
  selects the variant per D2 using `this.signers` and `this.guardianScheme`; update-signers
  metadata carries `targetSigners: SignerSpec[]` → payload `signer_schemes`; rebuilding a proposal
  uses them; switch-guardian fetches `/pubkey?scheme=<guardianScheme>`; docs + README.
- File a server issue (title: "Guardian server: per-signer signature schemes (mixed Falcon +
  ECDSA) — follow-up to #539") so PR A has an issue to close, since #540 closes #539.

## 7. Live validation (mandatory before handover)

Run a local server with the filesystem backend and ephemeral ack keys (`docs/LOCAL_DEV.md`;
`GUARDIAN_ACK_SECRET_PROVIDER=none`), then an **untracked** Rust harness (e.g. under
`crates/client/examples/` or a scratch bin, never committed) that:

1. `GET /pubkey?scheme=ecdsa` ⇒ guardian commitment; builds the mixed account with
   `MultisigGuardianConfig` (Falcon signer A, ECDSA signer B, guardian scheme ecdsa).
2. `POST /configure` (HTTP, curl or the TS client) with the `Miden` JSON signed by A, and the same
   over gRPC with `crates/client` (`AuthType::Miden`) for a second account ⇒ both 200.
3. `GET /state` signed by B raw and EIP-712, and by A ⇒ 200 each; B with a Falcon-shaped
   signature ⇒ 401 `authentication_failed`; `auth_scheme` is `"mixed"`.
4. `GET /state/lookup` for B's commitment ⇒ the account id.
5. Repeat 2–3 with `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa` ⇒ `signature_scheme_not_allowed`,
   `meta.scheme: "falcon"`.
6. Register a uniform all-Falcon account with the legacy variant and prove `GET /state` and a
   proposal push/sign still work (regression).
7. Postgres: `scripts/test-postgres.sh` (live, ignored tests) plus the migration applied on a
   fresh database and on one holding legacy rows.

Record the exact commands and outputs in the PR body's "Testing" section.

## 8. Review checklist for the implementer

- `rg 'auth\.scheme\(\)|with_updated_commitments' crates/` is empty.
- Legacy JSON fixtures serialise to the same bytes (test asserts it).
- Every `match auth` in `crates/server`, `benchmarks/`, `crates/qualification-driver`,
  `crates/server/bench` compiles (no `_ =>` wildcards added to hide the new variant).
- Both proto copies carry the new messages; `docs/openapi*.json` regenerated; `spec/api.md`
  updated; `packages/guardian-client` tests green.
- The e2e file proves scenarios 1–7; integration proves both transports.
- PR body: summary, API (JSON + proto), compatibility note (D2), testing (unit, matrix, live).
