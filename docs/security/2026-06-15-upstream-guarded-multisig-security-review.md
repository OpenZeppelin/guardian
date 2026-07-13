# Security Re-Audit: Upstream `AuthGuardedMultisig` Adoption (Miden 0.15)

**Date:** 2026-06-15
**Scope:** The migration replacing Guardian's forked multisig+guardian MASM with the
upstream `miden-standards` 0.15.3 `AuthGuardedMultisig` component, plus all Guardian-side
wiring changes (Rust SDK tx-builders, server custody paths, slot-name migration,
`guardian_enabled` removal, TS SDK).
**Method:** Direct review of the upstream contract MASM (custody enforcement), plus
focused adversarial review of the changed Guardian code across three dimensions.
**Verdict:** No Critical/High issues in production code. One test-coverage defect found and
**fixed**. Production is gated on closing the cross-SDK determinism gap (browser test) and
an external sign-off of the rotation-model change.

> **Status update (2026-07-13):** The §3 cross-SDK determinism BLOCKER is **resolved** —
> resolution option (b) materialized: `@miden-sdk/miden-sdk 0.15.2` embeds
> miden-standards 0.15.3, matching the Rust pin. The Playwright gate
> (`packages/miden-multisig-client/tests/browser/determinism.spec.ts`) now asserts full
> account **id + commitment byte-equality** between the TS and Rust builders and passes;
> it runs in CI (`ts-sdk` job) alongside the procedure-root parity vitest. The §4
> rotation-model sign-off remains an open item for release approval.

---

## 1. Custody core — upstream contract enforcement (verified against MASM)

The custody guarantees are enforced by the audited upstream component. Confirmed against
`miden-standards-0.15.3/asm/standards/auth/{multisig,guardian}.masm` and
`account_components/auth/guarded_multisig.masm`:

- **Threshold enforcement** (`multisig::auth_tx`): verifies approver signatures against the
  per-approver `[PUB_KEY, SCHEME_ID]` slots, computes the transaction threshold, and aborts
  unless `num_verified_signatures >= transaction_threshold`. **Sound.**
- **Per-procedure overrides** (`compute_transaction_threshold`): takes the max contribution
  over all called non-auth procedures; a called procedure with no override contributes the
  default threshold (never less). A wrong/stale override key returns `[0,0,0,0]` →
  falls back to default → **fail-safe (cannot silently downgrade)**. This is why the
  procedure-root correctness (and its guard test) matters: a mismatch makes a *higher*
  configured override ineffective, but never weakens below default.
- **Guardian rotation carve-out** (`guardian::verify_signature`): the guardian signature is
  skipped **only** when `update_guardian_public_key` was called **and** it is the *sole*
  non-auth procedure **and** there are no input/output notes. The multisig threshold
  (`auth_tx`) still applies. **Sound, and matches `docs/CONCEPTS.md`** cold-key recovery:
  a user quorum (≥ default threshold) rotates the guardian without the guardian's
  cooperation. Accepted implication (decision D2): any normal quorum can replace the
  guardian — the guardian protects against sub-threshold attackers, not a compromised quorum.
- **Replay protection** (`multisig::assert_new_tx`): records the tx-summary commitment in the
  `executed_transactions` map and aborts on re-execution. Runs after guardian verification in
  the guarded flow. **Sound.**

The Guardian-side adoption was validated end-to-end on mock-chain by the 8 integration tests
in `crates/contracts/tests/auth/multisig.rs` (everyday 2-of-2 + guardian co-sign, add/remove
signers, per-proc thresholds for Falcon + ECDSA, guardian rotation on threshold alone).

## 2. Findings

### FIXED — Test-coverage defect: `rejects_unreachable_existing_proc_override`
`crates/contracts/tests/auth/multisig.rs` — the test built a truncated config advice vector
(omitting the interleaved scheme-id word), so it aborted on a malformed-advice parse error
rather than exercising the intended invariant (an existing `send_asset` override of 2 becomes
unreachable when the signer set shrinks to 1). **Fixed** by emitting the well-formed
interleaved `[PUB_KEY, SCHEME_ID]` layout; the test now reaches and verifies the real
contract invariant. Suite remains 8/8 green.

### MEDIUM (hardening) — presence-based guardian detection skips replay write if key zeroed
`crates/server/src/network/miden/account_inspector.rs::has_guardian_auth` now returns
`extract_guardian_public_key().is_some()` (was selector-based). The replay-protection write in
`network/miden/mod.rs` is gated on this. If the guardian pub_key map entry were ever zeroed,
replay protection would be silently skipped. **Not externally exploitable**: zeroing the slot
requires a contract-accepted delta that also passes Guardian's delta-chain validation, and the
result would be a commitment mismatch (account lock), not a replay. **Recommendation:** add a
defensive invariant in `apply_delta` rejecting any `has_guardian_auth` true→false transition.

### LOW (operational) — conflated error in `validate_guardian_commitment`
A missing slot and a zeroed key both surface "Missing required slot" in
`network/miden/mod.rs`. Both correctly block configuration (no bypass); the message is just
misleading for incident response. Optional: distinguish the two cases.

### LOW (latent) — `extract_pubkeys` truncates on first index gap; `extract_slot_1_pubkeys` is a stale-named alias
Pre-existing patterns in `account_inspector.rs`; sound while the upstream component assigns
approver indices contiguously, but worth confirming against future remove-signer semantics.

## 3. Residual risks gating production (KNOWN / TRACKED)

### BLOCKER (now PROVEN) — Cross-SDK account identity diverges: web SDK vs Rust standards version skew
A Playwright browser gate was built (`packages/miden-multisig-client/tests/browser/`,
`npm run test:browser`, uses system Chrome) that builds the account in a real browser with the
same fixed inputs as the Rust parity test and decomposes the result. Findings:

1. **Storage layout parity — ACHIEVED.** TS `AccountBuilder.build()` was adding an extra
   `miden::standards::metadata::storage_schema::commitment` slot that the Rust account
   (7 slots) does not have. Switching the TS builder to `buildWithoutSchemaCommitment()`
   makes the TS **storage commitment byte-match Rust** (`0xa5b24ee9…`) and the slot set
   identical. This was a real bug — **fixed**. (`src/account/builder.ts`.)
2. **Account id/commitment — STILL DIVERGE, root cause identified.** Of the six procedure
   roots, five (the threshold-override targets: update_signers, update_procedure_threshold,
   update_guardian, send_asset, receive_asset) are present in the TS account, but
   `auth_tx_guarded_multisig` is **not** — the TS-compiled auth-flow procedure has a different
   MAST. The auth-flow library internals (`multisig::auth_tx`, `guardian::verify_signature`)
   are resolved from the **web SDK assembler** (`@miden-sdk/miden-sdk` 0.15.0), which bundles a
   **different miden-standards patch than the Rust pin (0.15.3)**. The result: a TS-created
   account's code commitment and id are **not byte-identical to the Rust/server account**.

**This is a dependency version skew, not a Guardian wiring bug** — it cannot be fixed in
Guardian code. Until the web SDK (`@miden-sdk/miden-sdk`) and the Rust crates
(`miden-protocol`/`miden-standards`) are aligned to the same standards patch, browser-created
accounts will not match server/Rust-created accounts. **Resolution options:** (a) pin Rust
down to the standards patch the web SDK 0.15.0 bundles, regenerate all reference values +
`procedures.ts` + re-audit; (b) obtain a web SDK build against 0.15.3; (c) restrict account
creation to a single SDK until aligned. The Playwright gate asserts the achievable parity
(storage + override-target procedures) and tracks full id/commitment parity as a documented
`test.fixme` to be unskipped once versions align.

### Recommended additional guards
- A TS unit test asserting the **final slot array order** (today `storage.test.ts` mocks
  the SDK and checks map entry counts, not order).
- Validate user-supplied custom procedure-threshold names against the compiled account's
  procedures (today an unknown name silently stores an inert map entry).

### Tx-script runtime correctness (TS) — unproven in node
`updateGuardian.ts` stack-arg felt order and `updateSigners.ts` advice must be validated on
devnet/browser; node cannot execute them. Mirrors the validated Rust builders, but
unexecuted in TS.

### Data cutover
The 0.15 cutover migration is irreversible and wipes pre-0.15 account data
(`states`/`deltas`/`delta_proposals`/`account_metadata`); `admin_actions` audit table is
preserved. Documented in `docs/PRODUCTION.md`. No audit-trail regression.

## 4. Sign-off recommendation

The adopted component is the already-audited upstream `miden-standards` contract, and
Guardian's wiring is validated on mock-chain. However, two items warrant external sign-off
before production:
1. The **rotation-model change** (fork required current-guardian co-signature; upstream/
   CONCEPTS allow rotation on the user quorum alone) — a deliberate custody-semantics change.
2. The **cross-SDK determinism gate** — close the browser parity test, then confirm.
