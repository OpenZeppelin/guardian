# Qualification findings

Defects, capability gaps and cross-SDK divergences found by building and
running the qualification suite against Miden testnet. Each entry says what was
observed, how it was established, and what it costs a consumer.

This is a findings log, not a changelog: entries stay until the underlying
behaviour changes. Each entry says whether it is fixed; where the suite carries a
workaround instead, the workaround is named.

Evidence is from runs against testnet on 2026-09-16 with a locally built server,
`@miden-sdk/miden-sdk` 0.16.0 and `@openzeppelin/miden-multisig-client` 0.17.0.

| | Finding | Affects |
|---|---|---|
| F1 | Rust cannot change a threshold | Rust SDK (**fixed**) |
| F2 | `load()` returns an account whose reads come from a stale local store | TypeScript SDK (**fixed**) |
| F3 | Rust cannot collect signatures off-channel | Rust SDK (**fixed**) |
| F4 | Proposal creation signs in Rust, not in TypeScript | Both SDKs |
| F5 | Native Node entry cannot run the multisig client | Published packages |
| F6 | Node needs an HTTP/2 shim to reach Miden gRPC | Published packages |
| F7 | `initSync` missing from the `./lazy` types | Published packages |
| F8 | Acknowledgement identity is per scheme, default is silent | GUARDIAN |
| F9 | A change stays pending briefly after its proposal clears | GUARDIAN |
| F10 | Absence from pending is not evidence of completion | GUARDIAN, both drivers |
| F11 | Devnet does not serve gRPC-web | Environment |
| F12 | A grown allowlist reads truncated through a macOS bind mount | Environment, GUARDIAN (**fixed**) |
| F13 | An ECDSA account cannot migrate GUARDIAN through the Rust SDK | Rust SDK (**fixed**) |

## Cross-SDK divergences

### F1. The Rust SDK cannot change a threshold (fixed)

`TransactionType::update_signers(threshold, commitments)` was a public
constructor whose transaction builder rejected the whole variant:

```rust
TransactionType::UpdateSigners { .. } => Err(MultisigError::InvalidConfig(
    "Use AddCosigner or RemoveCosigner for signer updates".to_string(),
)),
```

So a Rust consumer had no way to change a multisig's threshold. The on-chain
contract supports it (`update_signers_and_threshold`, exercised by
`crates/contracts/tests/auth/multisig.rs`) and the TypeScript SDK drives it
through `createChangeThresholdProposal`. The constructor was reachable and
documented, so the failure appeared only at proposal time.

The redirect in that message was a dead end. `build_add_cosigner` and
`build_remove_cosigner` both pin the threshold (`// Keep same threshold`), so
neither could serve a threshold change. The guard was sound for a membership
change and closed the only route to a capability with no alternative.

**Nothing else was missing.** Rust already parsed `change_threshold` metadata,
executed the variant, exported and imported it, and applied its delta; the
server and Rust's own proposal-type allowlist both accept the wire type. Only
creation was blocked, which is why a Rust client could always co-sign a
threshold change proposed by a TypeScript one.

**Fixed.** The builder now has a `build_update_signers` arm, modelled on
`build_add_cosigner` with the signer set left alone. Two details it does not
share with its neighbours:

- A membership change is refused rather than served. Deriving the new set from
  the current one is what makes add and remove safe, and a caller-supplied set
  would give that up for nothing, since this variant exists to move the
  threshold. The requested set is compared to the current one as a set, then the
  request is built from the account's own ordering, so listing the same signers
  in another order cannot silently repack the storage indices.
- `metadata.proposal_type` is set explicitly, unlike add and remove which let
  `TransactionType::proposal_type()` supply it. All three wire types parse back
  into `UpdateSigners`, so the variant cannot name itself; `proposal_type()`
  returns `None` for it on purpose and export refuses to guess
  (`from_proposal_rejects_ambiguous_update_signers_without_proposal_type`).
  Without the explicit value the proposal would be signable but not exportable.

Validation is a free function, `validate_threshold_change`, so its rules are
tested without a client or a network: same set in any order accepted,
membership change refused, threshold outside `1..=signers` refused, no-op
refused.

**In the suite**: `live-change-threshold-2of3-ecdsa` no longer skips the Rust
leg. Verified against testnet through the stack, Rust leg `Passed`.

### F2. `load()` returns an account whose reads come from a stale local store (fixed)

`live-remove-signer-2of3-falcon` failed on TypeScript and passed on Rust, five
times, against the same GUARDIAN in the same runs. The scenario reported:

```
GUARDIAN serves 0x27e3b76a…,0x6dc948c9…,0x82320d22…
but             0x6dc948c9…,0x82320d22…  was expected after 180s
```

**That message was wrong about who was stale.** It was measuring the client and
reporting the result as GUARDIAN's. Logging both sources on every poll for the
full 180s window:

```
guardian-snapshot = 0xc1516905…,0xd27c08b4…                 (2 signers, correct)
store-read        = 0x8fde6b3e…,0xc1516905…,0xd27c08b4…     (3 signers, stale)
expected          = 0xc1516905…,0xd27c08b4…
```

GUARDIAN returned the correct post-removal set on the **first** poll and every
poll after. The removal propagated immediately.

**Mechanism.** `MultisigClient.load()` fetches the account from GUARDIAN,
deserializes it, derives the config from it, and then:

```ts
const existingAccount = await this.midenClient.accounts.get(AccountId.fromHex(accountId));
if (!existingAccount) {
  await this.midenClient.accounts.insert({ account, overwrite: true });
}
```

`packages/miden-multisig-client/src/client.ts:229`

It writes GUARDIAN's account to the store only when the store has no record. A
caller that already holds the account keeps its own copy, and
`getSignerPublicKeyCommitments()` reads the store through `getStoreAccount()`,
so the state GUARDIAN just returned is discarded for every read. The returned
`Multisig` is internally inconsistent: its **config** comes from GUARDIAN, its
**account reads** come from a store that may be arbitrarily old.

**Why Rust is unaffected.** `MultisigClient::pull_account` does the same fetch
and then overwrites unconditionally, keeping the fetched account in memory as
the one subsequent reads see:

```rust
self.add_or_update_account(&account, true).await?;
self.account = Some(MultisigAccount::new(account));
```

`crates/miden-multisig-client/src/client/account.rs:174`

So this is a real divergence between the SDKs, not a harness artifact.

**GUARDIAN is correct, on independent evidence.** For the same account, its
canonicalization log shows both deltas applied and verified:

```
Canonicalizing delta (commitment matches on-chain) nonce=…
Deleting matching proposal as delta is now canonical
```

and its auth path refuses the removed key (`public key commitment not
authorized`). A GUARDIAN holding the pre-removal set could not produce either.

**Refuted along the way.** The TypeScript SDK defaults a proposal nonce to
`Date.now()` (`multisig.ts:214`) while Rust uses `account.nonce() + 1`. That is
a genuine divergence, and it is **not** the cause here: pinning the Rust
convention reproduced the failure unchanged.

**Cost**: a TypeScript consumer calling `load()` for an account it already holds
locally gets stale membership, silently, with no error and no staleness signal.
On a private account GUARDIAN's copy is the only full state a third party can
read, so `load()` is exactly the call that is supposed to correct a stale client,
and it is the one that does not.

**In the suite**: the assertion now reads the account `load()` returned from
GUARDIAN rather than the store, which is what it always claimed to check, and
`live-remove-signer-2of3-falcon` passes on both SDKs. The store value is still
printed in the failure text when the two disagree, so this defect stays visible
without being asserted on.

**Fixed.** The SDK already contained the right rule and `load()` did not use it:
`syncState()` reconciles by nonce and commitment before overwriting, `load()`
did not. Copying Rust's unconditional overwrite would have been wrong, because
between pushing a delta and GUARDIAN canonicalizing it the local account is
legitimately ahead and independently verifiable against chain (#316, #312,
#319); clobbering it would build the next transaction on a stale nonce.

The rule now lives in `src/state/adopt.ts` and both paths use it. `load()`
reconciles, then derives its config from whichever account won, so the returned
`Multisig` no longer describes one state while reading another:

| Store | Outcome |
|---|---|
| empty | adopt GUARDIAN's account |
| same commitment as GUARDIAN's | keep local, and do not consult the rule, since equal nonce with differing commitments is read as divergence and throws |
| behind GUARDIAN | adopt GUARDIAN's account |
| ahead of GUARDIAN | keep local, and describe local |

Verified three ways. Three unit tests in `client.test.ts` cover the table and
all three fail against the previous behaviour. The live scenario passes on both
SDKs. And with the assertion pointed back at the store, the exact read that
failed five times, `live-remove-signer-2of3-falcon` passes against a real
testnet GUARDIAN.

### F3. Rust cannot collect signatures off-channel (fixed)

A cosigner handed a proposal as JSON could add a signature to it in TypeScript
but not in Rust. Two separate causes, and the first hid the second.

**Cause 1: the signing gate.** `sign_imported_proposal` refused anything but
`SwitchGuardian`, on `supports_offline_execution()`. That predicate answers "can
this execute without a GUARDIAN acknowledgement", which only a guardian switch
can. Signing is a local act over a commitment and does not depend on it. The
gate borrowed a predicate that is right about its own question to decide a
different one, the same shape as F1, and it sat directly in front of the
verification that does the real work, which is structurally identical to the
TypeScript one.

**Cause 2: the document could not be executed.** With signing allowed, the flow
reached execution and stopped at `proposal not ready: need 2 signatures, have 1`.
Off-channel signatures live in the document, the online path reads readiness from
GUARDIAN's copy which never saw them, and `execute_imported_proposal` fetched no
acknowledgement, so it could only ever execute the one type that needs none.

**Fixed.** The signing gate is gone, and `execute_imported_proposal` now takes
cosigner signatures from the document and the acknowledgement from GUARDIAN,
exactly as the online path does. Its own gate went with it, since the reason for
it was the missing acknowledgement. The switch-only note-import warning is now
keyed on the type rather than on "ack-less", so a future ack-less type does not
inherit it.

**Checked first, because the client-side check is not what enforces the quorum.**
GUARDIAN acknowledges a delta without counting cosigner signatures:
`push_delta` calls `ack_delta` unconditionally and consults a matching proposal
only to label a metric. So readiness checks in either client are pre-flight
convenience, and the contract is the enforcement. That was assumed rather than
demonstrated, so it is now pinned by
`test_multisig_below_threshold_is_rejected_on_chain`: a 1-of-2 transaction
carrying GUARDIAN's signature is rejected on chain, and the same transaction
succeeds once the missing cosigner signs, so the refusal can only be the count.
It also pins that GUARDIAN's signature does not substitute for a cosigner.

`live-below-threshold-2of3-falcon` was part of the same gap. It accepted any
error whose text contained `signature`, which the client's own
`proposal not ready: need 2 signatures, have 1` satisfies, so it proved the
client refused rather than that the quorum held. Both drivers now match the
refusal precisely and confirm the account nonce did not advance.

**Not air-gapped signing.** The earlier wording claimed this blocked an
air-gapped workflow. It does not, on either SDK: except for `SwitchGuardian` and
`Custom` the binding check reproduces the transaction, so the signer needs a
synced store and, for consume-notes, the node. What was blocked is **off-channel**
signature collection, where the proposal travels as JSON between cosigners
instead of through GUARDIAN, each with a working client.

**In the suite**: the Rust leg of `live-offline-export-import-2of3-falcon`
passes, having only ever skipped. Migration and the ordinary execute path were
re-run against testnet to confirm the shared execute branch still behaves.

### F4. Creating a proposal signs it in Rust but not in TypeScript

The Rust SDK attaches the proposer's signature when the proposal is created;
the TypeScript SDK does not. A 2-of-3 therefore needs one more signature
collected on the TypeScript path than on the Rust path for the same flow.

Not a defect in either, but it is unstated, and any threshold arithmetic written
against one SDK is wrong against the other.

**In the suite**: both drivers offer the proposal to every cosigner rather than
assuming who has signed, and the TypeScript handoff scenario signs locally
before handing over.

## Consuming the published packages from Node

These apply to any Node consumer of `@openzeppelin/miden-multisig-client`, not
only to this suite.

### F5. The SDK's native Node entry cannot run the multisig client

`@miden-sdk/miden-sdk` exports a native Node binding under the `node` condition
(`js/node-index.js`, backed by `@miden-sdk/node-*` optional dependencies). That
entry exports 148 symbols against the browser WASM build's 184, and the missing
ones include `FeltArray`, `NoteAndArgsArray` and `NoteArray`.

`@openzeppelin/miden-multisig-client` imports `FeltArray` at 27 call sites
(`lookupAuth.ts`, `utils/signature.ts`, `utils/digest.ts`), so a plain `import`
from Node resolves the native entry and fails with
`FeltArray is not a constructor`.

It looks like a generation miss rather than a deliberate omission: the SDK ships
a `gen:node-reexports` script.

**Workaround the suite carries**: alias `@miden-sdk/miden-sdk` to the browser
WASM build, `dist/st/index.js`.

### F6. Node cannot reach the Miden gRPC endpoints without an HTTP/2 shim

Node's built-in fetch is HTTP/1.1 only. The Miden RPC and prover endpoints sit
behind a load balancer whose gRPC target group accepts HTTP/2 only, and answers
HTTP/1.1 with **464 and no headers at all**. The SDK's gRPC-web client reports
that as `missing content-type header in gRPC response`.

The same request, byte for byte, over each protocol:

```
HTTP/1.1 → 464, Content-Length: 0, no content-type
HTTP/2   → 200, content-type: application/grpc-web+proto, grpc-status: 13
```

Browsers are unaffected because they negotiate HTTP/2 through ALPN. All four
endpoints (`rpc` and `tx-prover`, both networks) advertise `h2`.

**Cost**: without a shim the remote prover is unreachable and the SDK falls back
to in-WASM proving, which costs roughly twenty-five times the CPU. A 2-of-3
lifecycle took 124s wall / 91s CPU locally against 28.7s / 3.6s remote. The
fallback is silent: it looks like slowness, not breakage.

**Workaround the suite carries**:
`packages/miden-multisig-client/tests/qualification/h2Fetch.ts` routes gRPC-web
calls over `node:http2`.

### F7. `initSync` is exported at runtime but missing from the `./lazy` types

The `./lazy` entry exports `initSync` (it is in the built JS), but the type
declarations that subpath points at do not declare it, so a Node consumer
initialising the WASM module by hand has nothing to import.

**Workaround the suite carries**: a local ambient declaration,
`packages/miden-multisig-client/tests/miden-sdk-lazy.d.ts`.

## GUARDIAN behaviour

### F8. The acknowledgement identity is per scheme, and the default is silent

GUARDIAN holds one acknowledgement identity per signature scheme
(`state.ack.commitment(&scheme)`), and an account binds the one matching its own
scheme. `getPubkey()` without a scheme argument returns the default, so an ECDSA
account built from it binds the Falcon commitment and is refused at registration
with `403 ... not an authorized signer for this account`.

The HTTP status and message point at the caller's authorisation rather than at
the mismatch; only the server log names it
(`Slot 'guardian::pub_key' mismatch`).

**Cost**: every ECDSA account created without passing the scheme fails
registration, with an error that misdirects. **In the suite**: the scheme is
always passed.

### F9. A change stays "pending" briefly after its proposal disappears

Proposing a change immediately after the previous one clears the pending list is
rejected with `There's already a pending change for this account`. GUARDIAN's
authorisation list lags likewise: a signer admitted by an executed add-signer is
refused (`authorized_count` still reports the pre-change size) until GUARDIAN
catches up.

Both are brief, and both are races only a fast client hits. The Rust driver hit
them where the TypeScript driver, being about four times slower, did not.

**In the suite**: both drivers poll against bounded deadlines instead of racing.

### F10. Absence from the pending set is not evidence of completion

A proposal leaves GUARDIAN's pending set for two opposite reasons: because
canonicalization **applied** its delta, or because canonicalization **gave up**
on it, logged as `Deleting matching proposal as its delta left the candidate
path`. The two are indistinguishable from the client side, so a discarded delta
reads exactly like a successful execution.

This is GUARDIAN behaviour, and it has not changed. What has changed is that the
suite no longer relies on the unsound signal.

**Fixed in the suite.** Both drivers now assert FR-018 directly: chain
confirmation and commitment agreement through `verify_state_commitment` /
`verifyStateCommitment`, plus a canonical delta in `delta_history` /
`deltaHistory` carrying that commitment. Absence from pending is a precondition,
not the proof. A proposal that vanished without a canonical delta is reported as
a product failure rather than a pass.

Two corollaries, both found while fixing it:

- The pending listing is the TypeScript client's own cache, and an executed
  proposal stays in it marked `finalized`. Presence alone is not pending; status
  decides.
- A GUARDIAN migration repoints the client at the GUARDIAN it moved to, which
  has no history for an account it was just handed. Completion there is chain
  agreement plus the new GUARDIAN serving the account.

**Still owed**: a negative control that discards a delta deliberately and
confirms the driver fails. Until that runs, the new assertion is correct by
construction but unfalsified. F2 is the kind of defect this blind spot hid.

## Environment

### F11. Devnet does not serve gRPC-web

A correct gRPC-web POST to `rpc.devnet.miden.io` returns **415** where
`rpc.testnet.miden.io` returns **200**. The suite treats this as an outage
rather than a structural limitation, so devnet stays declared available in the
coverage matrix and recovers without a manifest edit.

Devnet's separate constraint is structural and persists: it serves historical
account state for roughly fifty blocks (~150s), so flows whose step budget
exceeds that window are excluded from its required set.

### F12. A grown allowlist file reads truncated through a Docker Desktop bind mount (fixed)

`det-operator-allowlist-reload` fails on macOS with a 500 and this server-side
error, at the same position every time:

```
Failed to parse .../operators.json: EOF while parsing a string at line 11 column 7
```

A fixed position is the tell: a write race would move around. Line 11 column 7
is byte **3728**, and the pre-change file is **3729** bytes. The server read the
old file's length out of the new, larger one.

Confirmed by inverting it. Padding the baseline so the scenario's write
*shrinks* the file instead of growing it makes the scenario pass: a stale larger
size still yields the whole smaller file. Grow it and the read truncates; shrink
it and it does not.

So the bind mount is serving stale size metadata immediately after a rename.
The write itself is atomic (staged sibling plus `renameSync`), and reading the
file from inside the container a moment later shows the full 3757 bytes.

**Expected to be macOS-only.** Docker Desktop reaches the host filesystem
through a virtiofs/FUSE layer that caches attributes; a Linux runner bind-mounts
natively and has no such layer. Not verified on Linux, so treat that as the
expectation rather than a measurement.

**Worth noting about GUARDIAN regardless of the host**: the allowlist is re-read
on *every* authenticated request and a partial read is answered with a 500 and
no retry. Any writer that is not atomic from the server's point of view takes
the operator dashboard down for the duration.

**Measured.** A reader looping inside a container while the host performed 400
staged-write-plus-rename swaps saw **26,299 of 260,853 reads** fail to parse,
every one with the same signature the scenario hits. The same loop with both
writer and reader inside the container saw **0 of 1,135,457**. The rename is
atomic on the host; the container's view of the shared directory is not
coherent during the swap.

**Fixed in GUARDIAN, not in the scenario.** The note above turned out to be the
actionable half: `AllowlistSource::load` now retries a failed load three times
over 100ms before surfacing it
(`crates/server/src/dashboard/allowlist.rs`). A source that stays unreadable
still fails closed, so a genuinely misconfigured allowlist behaves exactly as
before; only the torn-write window is absorbed. This also covers a transient
Secrets Manager error on the `AwsSecret` source, and it means an operator
editing the file non-atomically no longer takes the dashboard down for the
duration of the write.

**In the suite**: the scenario keeps the atomic write, which is correct on a
real filesystem. `det-operator-allowlist-reload` now passes on macOS.

### F13. An ECDSA account cannot migrate GUARDIAN through the Rust SDK

`live-guardian-migrate-offline-1of1-ecdsa` cannot even build its proposal:

```
refusing to use GUARDIAN endpoint http://127.0.0.1:54574:
endpoint pubkey commitment 0x1629b4b5db2327ef… does not match expected 0x509f64d7272b96e7…
```

The cause is one argument. `verify_endpoint_commitment` in
`crates/miden-multisig-client/src/guardian_endpoint.rs` fetches the target's
identity with `client.get_pubkey(None).await`, **no scheme**, so it always gets
GUARDIAN's default, which is Falcon.

GUARDIAN holds one acknowledgement identity per signature scheme (F8), and an
ECDSA account's guardian slot must hold the **ECDSA** commitment. The caller
therefore supplies the correct ECDSA commitment, the validator compares it
against the target's Falcon one, and they never match. There is no value a
caller can pass that satisfies both the validator and the account: the check is
unsatisfiable for any non-default scheme.

The same omission as F8, one layer up. F8 was a caller forgetting the scheme;
this is the SDK forgetting it on the caller's behalf, where no caller can
compensate.

**Cost**: GUARDIAN migration is impossible for ECDSA accounts on the Rust path,
and the error names a commitment mismatch rather than the missing scheme, so it
reads as a misconfigured endpoint.

**Found by**: the first live run through the stack, which is the only
configuration that supplies a second GUARDIAN to migrate to.

**Fixed.** `verify_endpoint_commitment` now takes the account's
`SignatureScheme` and queries `get_pubkey(Some(scheme.as_str()))`. The scheme is
a required parameter rather than an option, so the call cannot be made without
one; that is a stronger guarantee than a test asserting the argument is present.
Consumers on the published SDK still hit this until the next release.
