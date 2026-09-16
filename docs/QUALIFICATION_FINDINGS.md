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
| F1 | Rust cannot change a threshold | Rust SDK |
| F2 | GUARDIAN serves a stale signer set after a TypeScript remove-signer (enforcement is correct) | GUARDIAN, TypeScript path |
| F3 | Offline signing tied to offline execution | Rust SDK |
| F4 | Proposal creation signs in Rust, not in TypeScript | Both SDKs |
| F5 | Native Node entry cannot run the multisig client | Published packages |
| F6 | Node needs an HTTP/2 shim to reach Miden gRPC | Published packages |
| F7 | `initSync` missing from the `./lazy` types | Published packages |
| F8 | Acknowledgement identity is per scheme, default is silent | GUARDIAN |
| F9 | A change stays pending briefly after its proposal clears | GUARDIAN |
| F10 | Absence from pending is not evidence of completion | GUARDIAN, both drivers |
| F11 | Devnet does not serve gRPC-web | Environment |
| F12 | A grown allowlist reads truncated through a macOS bind mount | Environment, GUARDIAN |
| F13 | An ECDSA account cannot migrate GUARDIAN through the Rust SDK | Rust SDK — **fixed** |

## Cross-SDK divergences

### F1. The Rust SDK cannot change a threshold

`TransactionType::update_signers(threshold, commitments)` is a public
constructor, but the transaction builder rejects the whole variant:

```rust
TransactionType::UpdateSigners { .. } => Err(MultisigError::InvalidConfig(
    "Use AddCosigner or RemoveCosigner for signer updates".to_string(),
)),
```

`crates/miden-multisig-client/src/transaction/builder.rs`

So a Rust consumer has no way to change a multisig's threshold. The on-chain
contract supports it (`update_signers_and_threshold`, exercised by
`crates/contracts/tests/auth/multisig.rs`) and the TypeScript SDK drives it
through `createChangeThresholdProposal`, which the suite runs green. The
constructor is reachable and documented, so the failure appears only at
proposal time.

**Cost**: a threshold change requires the TypeScript SDK. **In the suite**: the
Rust leg of `live-change-threshold-2of3-ecdsa` reports a skip naming the gap.

### F2. GUARDIAN serves a stale signer set after a TypeScript remove-signer

After `live-remove-signer-2of3-falcon` executes, GUARDIAN is left one nonce
behind and keeps serving the pre-removal signer set indefinitely. Measured on a
failing run, account `0xa3f26288fd49d6015fa0d80b7a7ef5`:

| | nonce | state commitment |
|---|---|---|
| Client's local store | 2 | `0x72c0918be6396559…` |
| Chain | 2 | `0x72c0918be6396559…` |
| GUARDIAN | 1 | `0x55be6e1e617ef20b…` |

`verifyStateCommitment()` reports the local and on-chain commitments as equal,
so the removal executed and the client is not stale. GUARDIAN alone is behind,
and stays behind past a 180s deadline.

The proposal did leave GUARDIAN's pending set, which is what the driver treats
as completion (see F10). GUARDIAN logs nothing for the account at `warn` level:
no canonicalization failure, no discard, no error. So from the outside a
discarded delta and an applied one look identical.

Reproducible and SDK-specific: TypeScript failed four times, Rust passed three,
same scenario and network. Both drivers read the same thing, GUARDIAN's stored
account blob (`pull_account` in Rust, `load` in TypeScript), so this is not the
two clients reading different sources. Whatever differs is in what each SDK
pushes, or in how GUARDIAN handles it.

**Not an artifact of a reused server.** The first three failures were against a
long-lived local GUARDIAN on SQLite that had absorbed a full day of runs. The
fourth was a container built from the current tree, on Postgres, with an empty
database, started minutes earlier, through the qualification stack. It served
the same stale three-signer set 180s after the removal executed. The Rust leg
passed the same scenario against that same fresh GUARDIAN in the same session,
so the storage backend and accumulated state are both ruled out.

Note the accounts are **private**, so the chain holds only a commitment and
GUARDIAN's copy is the only full state a second party can read. A signer removed
from a private account therefore still appears, to anyone asking GUARDIAN, to
hold it.

**The removal is enforced; only the listing is stale.** This was the open
severity question, and it is now measured rather than inferred. After the
removal, the removed key attempts an authenticated call of its own
(`signer-removed-refused`, run before the listing assertion so it executes even
when that fails). GUARDIAN **refuses it**, on both SDKs.

So the removed cosigner cannot act. This is a correctness and observability
defect, not an eviction failure: a compromised signer *is* locked out, GUARDIAN
just keeps describing the account as though it were not.

**Cost**: after a removal on the TypeScript path, GUARDIAN serves a signer set
that includes the removed signer, with no error anywhere to indicate it. Anything
reading membership from GUARDIAN — an operator dashboard, a consumer checking who
can sign, an audit — sees a signer who has in fact been removed. On a private
account GUARDIAN's copy is the only full state a third party can read, so there
is no second source to correct it.

**In the suite**: the scenario is left failing rather than widening its window
further; it is already 180s.

**Next diagnostic**: re-run with the server at `RUST_LOG=info` and read the
canonicalization decisions for the account. That distinguishes "the delta was
never applied" from "it was applied and the served state was not updated", which
the `warn` level cannot.

### F3. Offline signing is tied to offline execution in Rust

`TransactionType::supports_offline_execution()` is true only for
`SwitchGuardian`, and `requires_guardian_ack()` is defined as its inverse. So
`sign_imported_proposal` refuses to collect signatures off-channel for any
proposal type that needs a GUARDIAN acknowledgement at execution, which is every
other type.

The TypeScript SDK signs any proposal type offline and contacts GUARDIAN only to
execute, which is what the Rust path would also do. The restriction blocks an
air-gapped cosigning workflow the protocol allows.

**In the suite**: the Rust leg of `live-offline-export-import-2of3-falcon`
reports a skip naming the gap.

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

### F12. A grown allowlist file reads truncated through a Docker Desktop bind mount

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

**In the suite**: the scenario keeps the atomic write, which is correct on a
real filesystem, and is left failing on macOS rather than contorted to suit one
host's caching.

### F13. An ECDSA account cannot migrate GUARDIAN through the Rust SDK

`live-guardian-migrate-offline-1of1-ecdsa` cannot even build its proposal:

```
refusing to use GUARDIAN endpoint http://127.0.0.1:54574:
endpoint pubkey commitment 0x1629b4b5db2327ef… does not match expected 0x509f64d7272b96e7…
```

The cause is one argument. `verify_endpoint_commitment` in
`crates/miden-multisig-client/src/guardian_endpoint.rs` fetches the target's
identity with `client.get_pubkey(None).await` — **no scheme**, so it always gets
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
