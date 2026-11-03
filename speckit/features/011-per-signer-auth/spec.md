# Feature 011: Per-signer signature schemes on the Guardian server

**Issue**: #563 (the server follow-up of #539, filed as its sub-issue). The client half is PR #540
(TS SDK); the Rust SDK advice rewrite is #549. **Branch**: `539-server-per-signer-schemes`.
**Status**: PR A (server, contracts, base clients, docs) merged (#565); PR B (Rust SDK, #549)
merged (#577); PR C (TS SDK, #576) implemented.

## Problem

`miden-standards` 0.17 stores a signature scheme per approver (`approver_scheme_ids` map,
`[i,0,0,0] => [scheme_id,0,0,0]`) and one for the guardian (`guardian::scheme`). A 2-of-2 with one
ECDSA wallet/hardware signer and one locally held Falcon key is a valid on-chain account. The
Guardian server cannot operate it:

- `Auth::{MidenFalconRpo, MidenEcdsa} { cosigner_commitments }` binds one scheme to the account.
  `Auth::verify` checks every request signature under that scheme, so an ECDSA cosigner of a
  `MidenFalconRpo` account can never authenticate (`crates/server/src/metadata/auth/mod.rs:170`).
- `Auth::scheme()` also selects the guardian's ack identity (`state.ack.commitment(&scheme)` in
  `configure_account.rs:73`, `push_delta.rs:112`, `release_on_switch.rs:254`,
  `release_sweep/sweep.rs:664`), the allowlist check, and the `auth_scheme` label on `GET /state`
  and the dashboard.
- `should_update_auth` (`network/miden/mod.rs:794`) rebuilds the cosigner set from the chain by
  commitment only (`with_updated_commitments`), so a legacy account that becomes mixed on chain
  keeps a wrong single scheme.
- Delta metadata lifts `signer_commitments` but no schemes (`delta_summary/mod.rs:188`).
- The server never checks the account's guardian *scheme* slot, so an account registered under
  ECDSA with a Falcon guardian slot (the brick scenario from the #540 review) is accepted.

Because of this, PR #540 makes the TS SDK refuse `registerOnGuardian`, signer-update proposals
and switch-guardian proposals with `SignerSchemeMismatchError` whenever an approver's or the
guardian's scheme differs from the acting signer's.

## Goals

- FR-1 An account can declare a scheme per cosigner and an independent guardian scheme.
- FR-2 Every signed request is verified under the registered scheme of the signer who made it, and
  only against commitments registered under that scheme.
- FR-3 The guardian ack identity (configure, push ack, release detection, `/pubkey` parity) is the
  declared guardian scheme. `/configure` validates both the guardian slot commitment and the
  guardian scheme slot, for every auth variant.
- FR-4 Canonicalization keeps commitments and schemes in step with the chain. A legacy
  single-scheme account that becomes mixed on chain is promoted to the per-signer representation;
  a uniform account keeps its legacy representation byte for byte.
- FR-5 `GET /state/lookup` / `GetAccountByKeyCommitment` find accounts in the new representation on
  both metadata backends (filesystem, Postgres).
- FR-6 Backward compatibility: legacy variants keep their wire and storage shape; existing clients
  and stored rows are untouched; an account registered with a legacy variant behaves exactly as
  today. The new variant is additive on HTTP JSON, gRPC and OpenAPI.
- FR-7 Delta metadata lifts `signer_schemes` (index-aligned with `signer_commitments`) for
  update-signers proposals so history/dashboard consumers see the target schemes.
- FR-8 Base clients can express the new variant (`crates/client` proto, `packages/guardian-client`
  types). The Rust SDK registers mixed accounts and builds per-signer update-signers advice
  (resolves #549). The TS SDK lifts the #540 refusal once this ships (separate PR after #540 merges).
- FR-9 `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES` applies to every scheme a new account uses (each
  cosigner's and the guardian's), and to every scheme a reconfiguration adds to the stored auth.
- FR-10 A proposal approval counts only under the scheme the account stores for its signer:
  `PUT`/`POST /delta/proposal` refuse any other, and the multisig push gate does not count it.
- FR-11 `/configure` requires a supported scheme for every approver of a multisig state.

## Non-goals

- EVM accounts (`Auth::EvmEcdsa`) are untouched.
- No migration of stored legacy rows; no change to the lookup response shape.
- No per-signer `create_account` API in the Rust SDK (creation stays single-scheme; can follow).
- No new error codes (keeps the `packages/guardian-client` drift guard untouched).

## Decisions

- **D1 New variant `Auth::Miden`, legacy variants kept.** JSON
  `{ "Miden": { "cosigners": [{ "commitment": "0x…", "scheme": "ecdsa" }, …], "guardian_scheme": "falcon" } }`.
  Self-describing objects (mirrors TS `SignerSpec`) rather than parallel arrays. Legacy variants are
  not rewritten on read or on save, so old servers, clients and rows keep working.
- **D2 Clients send the legacy variant for uniform sets.** A client sends `Miden` only when the
  cosigner schemes are not all equal to the guardian scheme. This keeps every existing flow on the
  old wire bytes and lets a new client talk to an old server for uniform accounts. The server
  accepts `Miden` for uniform sets too (no normalisation).
- **D3 Request verification dispatches on the credential, scoped to the signer's scheme.**
  For `Auth::Miden`: EIP-712 format ⇒ ECDSA path with the ECDSA-registered commitments only. Raw
  format ⇒ by `x-signature` byte length: 1524 bytes ⇒ Falcon path with Falcon-registered
  commitments; 65 bytes ⇒ ECDSA path with ECDSA-registered commitments; anything else ⇒
  `authentication_failed`. Same length rules as `metadata/auth/lookup.rs`. A key therefore
  authenticates only under the scheme it is registered with. Legacy variants keep their current
  code paths unchanged.
- **D4 `x-pubkey` for `Auth::Miden`** (`compute_signer_commitment`): a 64-hex value is a commitment
  (as today); 33 bytes ⇒ ECDSA public key; Falcon `PK_LEN` bytes ⇒ Falcon public key; else error.
- **D5 Guardian scheme is set at `/configure` and never changed by canonicalization.** The server
  validates at configure that the state's guardian slot holds `ack.commitment(guardian_scheme)`
  and that the guardian scheme slot equals `guardian_scheme` (legacy variants: their single scheme).
  Chain syncs only re-derive cosigners; they keep the stored guardian scheme. A change of
  guardian scheme reaches the server only through re-onboarding, which re-validates the binding.
- **D6 Promotion rule** (`Auth::with_chain_cosigners`): given the chain's `(commitment,
  Option<scheme>)` list, a legacy variant with scheme S stays legacy if every cosigner scheme is
  S; otherwise it becomes `Miden { cosigners, guardian_scheme: S }`. A `Miden` auth is rebuilt as
  `Miden` with its stored guardian scheme. A missing or unknown scheme entry (no 0.17 state has
  one; synthetic test states do) keeps the scheme the current auth registers that commitment
  under, else the guardian scheme. `/configure` refuses such a multisig state (D15), so the
  fallback serves canonicalization and synthetic test states. The rule is total so
  canonicalization never stalls on it, and it is safe: a wrong scheme can only lock that cosigner out, since no key of another scheme has
  its commitment. It runs at both sync points: `should_update_auth` (`/configure`) and
  `AppliedState::updated_auth` (candidate promotion, which reads `AppliedState.cosigner_schemes`
  from the same decode as the commitments).
- **D7 Labels.** `Auth::method_label()` for `Miden` is `"miden"` (new label, per the doc rule on
  `method_label`). `auth_scheme` strings (`GET /state`, dashboard summaries) come from a new
  `Auth::auth_scheme_label()`: legacy ⇒ its scheme; `Miden` ⇒ the shared name when all cosigners
  and the guardian use one scheme, else `"mixed"`. Contract docs list `"mixed"` as a third value.
- **D8 Lookup index.** A second Postgres GIN index on `(auth -> 'Miden' -> 'cosigners')
  jsonb_path_ops`, and the query gains an `@> jsonb_build_array(jsonb_build_object('commitment',
  $1))` disjunct. The existing index is left as is, so replicas still on the previous version keep
  using it during a rolling deploy. Filesystem lookup iterates the cosigners of every variant.
- **D9 Allowlist.** `reject_disallowed_scheme` runs for each distinct scheme in
  `Auth::schemes_in_use()` (cosigners ∪ guardian); the error names the first disallowed scheme.
  A reconfiguration skips the schemes the stored auth already uses and checks the rest, so an
  account cannot register under an allowed scheme and then reconfigure to another, while
  re-onboarding under its own schemes stays open whatever the policy is now (review of #565).
- **D10 No new error codes.** Malformed auth ⇒ `invalid_input`; verification failures ⇒
  `authentication_failed`; binding failures ⇒ `authorization_failed` (as today);
  allowlist ⇒ `signature_scheme_not_allowed`.
- **D11 Contracts crate** grows per-signer schemes and a guardian scheme
  (`with_signer_schemes`, `with_guardian_scheme`; defaults = `signature_scheme`) so server e2e tests
  and the Rust SDK can build mixed accounts through the existing builder.
- **D12 Delivery in three PRs**: A = server + contracts + base clients + docs (this feature);
  B = Rust SDK (closes #549); C = TS SDK (after #540 merges; lifts the refusal).
- **D13 Sessions** check that a session's grant names this server's ACK key of the scheme the
  account registers the session's signer under (was: of the account's single scheme).
- **D14 Candidate-queue binding** (`AuthBinding`) carries each signer's scheme and the guardian
  scheme, so nothing chains behind a candidate that only re-registers a key under another scheme.
- **D15 Approver schemes at `/configure`.** `NetworkClient::validate_approver_schemes` refuses a
  multisig state (threshold slot present) with an approver whose scheme entry is missing or
  unknown, as `invalid_input`; the chain verifies each approver under that entry. Only
  `/configure` calls it.
- **D16 Proposal approvals by scheme.** `ensure_cosigner_approval` (sign and create proposal)
  requires `ProposalSignature::scheme()` to equal the scheme the account registers the
  approval's signer with. The push gate counts approvals against `extract_chain_cosigners()` and
  skips one whose approver's stored scheme differs or is missing, since the chain aborts on an
  unsupported scheme id (a state without the scheme map at all, bare test slots, leaves it
  unchecked). A correctly registered key always passes, since its commitment is derived under its
  own scheme; the check matters for an account that registers a key under the wrong scheme, whose
  approvals the chain would refuse. On `POST` request auth already pins the proposer's key to its
  registered scheme, so the check there is defence in depth.
- **D17 Scheme names over HTTP stay lowercase.** gRPC parses scheme names in any case, HTTP JSON
  only in lowercase. Parsing HTTP names case-insensitively was tried and dropped: HTTP request
  auth signs the canonical JSON of the parsed body, so `"ECDSA"` re-serialized as `"ecdsa"` and
  the request failed with `authentication_failed` instead of the clearer parse error (checked
  live). gRPC signs the protobuf bytes as sent, so it can accept any case.

## Acceptance scenarios

1. Mixed registration: an account with Falcon signer A, ECDSA signer B, guardian = this server's
   ECDSA ack key with guardian scheme `ecdsa`, registered with
   `Miden { cosigners: [A:falcon, B:ecdsa], guardian_scheme: ecdsa }` signed by A (Falcon raw) ⇒
   200; stored auth equals the request; `ack_pubkey` is the ECDSA ack key; `GET /state` reports
   `auth_scheme: "mixed"`.
2. Each signer authenticates: `GET /state` with B (ECDSA raw and EIP-712) ⇒ 200; with A (Falcon
   raw) ⇒ 200. B's commitment presented under a Falcon-length signature, or A's under 65 bytes ⇒
   401 `authentication_failed`. Replay floors are per signer as before.
3. Legacy declaration of a mixed state: `MidenFalconRpo { [A, B] }` for the same state ⇒ 400
   `invalid_input` naming the scheme mismatch (promotion makes the extracted auth differ).
4. Wrong guardian scheme: `Miden { …, guardian_scheme: falcon }` for a state whose guardian slot is
   the ECDSA ack key ⇒ 403 `authorization_failed` (commitment mismatch). A legacy `MidenEcdsa`
   registration of a state whose guardian scheme slot is Falcon but whose guardian commitment is the
   ECDSA ack key ⇒ `authorization_failed` (scheme slot check). A legacy `MidenFalconRpo`
   declaration of a mixed state with an ECDSA guardian fails the guardian check first.
5. Allowlist: `GUARDIAN_ALLOWED_ACCOUNT_SCHEMES=ecdsa` rejects scenario 1 with
   `signature_scheme_not_allowed`, `meta.scheme = "falcon"`; an all-ECDSA `Miden` registration passes.
   An existing ECDSA account reconfigured to Falcon, or with a Falcon cosigner added, is rejected
   the same way; an existing mixed account re-configuring under its own schemes passes.
6. Uniform set via `Miden`: all-ECDSA cosigners, guardian ecdsa ⇒ accepted; `auth_scheme: "ecdsa"`;
   `method_label` `"miden"`.
7. Canonicalization promotion: a `MidenEcdsa` account whose canonical state now holds a Falcon
   approver ⇒ metadata becomes `Miden { …, guardian_scheme: ecdsa }`; the Falcon signer can then
   authenticate. A uniform account's metadata is unchanged (same JSON bytes) after canonicalization.
8. Lookup: B's commitment resolves the mixed account on filesystem and Postgres; legacy rows still
   resolve; EVM rows never match.
9. Proposals: B creates an `add_signer` proposal whose payload metadata carries
   `signer_schemes`; A signs; the delta's lifted metadata exposes `signer_schemes` aligned with
   `signer_commitments`. Existing proposals without `signer_schemes` lift as before (empty).
10. Compatibility: every existing unit/integration/e2e test passes unchanged; legacy JSON fixtures
    round-trip to identical bytes; OpenAPI regenerates with the new `oneOf` arm only.
11. Approval schemes: an ECDSA approval from a key the account registers as Falcon is refused with
    `invalid_proposal_signature` and not counted by the push gate; on a mixed account each
    approver's approval under its own scheme is accepted and counted.
12. Approver schemes: `/configure` of a multisig state whose approver has no supported scheme entry
    ⇒ 400 `invalid_input`, nothing stored.
