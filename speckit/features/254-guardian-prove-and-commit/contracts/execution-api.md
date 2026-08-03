# Server API Contract: Guardian Prove and Commit

Normative wire contract for the server surface added by #254. Field names and shapes
below are normative; Rust/TS type spellings are illustrative.

Route style follows the existing surface: **flat paths with query parameters, no path
parameters** (compare `/delta/proposal/single`, `/delta/candidate/abandon` in
`crates/server/src/builder/handle.rs:234-245`). Authentication is the existing
per-account scheme (`x-pubkey`, `x-signature`, `x-timestamp`) — no new mechanism
(FR-002).

## Endpoints

| Surface | Operation | Auth | Purpose |
|---|---|---|---|
| `POST /delta/proposal/execution` | `ExecuteDeltaProposal` | per-account, cosigner | Request that Guardian prove and submit a threshold-met proposal |
| `GET /delta/proposal/execution` | `GetDeltaProposalExecution` | per-account, cosigner | Report the state of one execution |
| `GET /delta/execution/current` | `GetCurrentExecution` | per-account, cosigner | Report the account's in-flight execution, if any (FR-036) |

gRPC additions to `crates/server/proto/guardian.proto`:

```proto
rpc ExecuteDeltaProposal(ExecuteDeltaProposalRequest) returns (ExecuteDeltaProposalResponse);
rpc GetDeltaProposalExecution(GetDeltaProposalExecutionRequest) returns (GetDeltaProposalExecutionResponse);
rpc GetCurrentExecution(GetCurrentExecutionRequest) returns (GetCurrentExecutionResponse);
```

The third operation exists so an in-flight execution is discoverable without polling every
proposal in turn. It is a **new** read rather than a field added to the existing proposal
listing, deliberately: changing an existing response shape would propagate to both base
clients and both multisig SDKs (FR-036).

Both HTTP handlers MUST carry `#[utoipa::path]` with per-status responses and
`security(...)`, and their wire types MUST derive `ToSchema` / `IntoParams`. Regenerate
committed specs with `cargo run --features evm --bin gen-openapi -- docs` (AGENTS.md §4).

## Request

`POST /delta/proposal/execution` — JSON body:

| Field | Type | Required | Notes |
|---|---|---|---|
| `account_id` | string | yes | Hex account id |
| `proposal_id` | string | yes | The proposal's commitment |

`GET /delta/proposal/execution` — query parameters `account_id`, `proposal_id`, both
required.

**When the proposal exists but has never been executed**, the operation MUST return
`404 Not Found` with code `GUARDIAN_EXECUTION_NOT_FOUND` (gRPC: `NOT_FOUND`, same code
string). This is distinct from `GUARDIAN_PROPOSAL_NOT_FOUND`, which means the proposal itself is
absent, and the two MUST NOT be conflated — a caller polling too eagerly needs to know that its
request has not been accepted yet, not that its proposal vanished.

It is also deliberately different from `GET /delta/execution/current`, which answers
`200 {"execution": null}` for "nothing in flight". That endpoint asks a question about the
*account* and "none" is a valid answer; this one asks for a *specific named execution*, and
absence is a missing resource. Both mappings MUST be verified by parity tests (SC-039).

`GET /delta/execution/current` — query parameter `account_id`, required. An execution in a
terminal state is not "in flight" and MUST NOT be returned here; it remains readable via
`GET /delta/proposal/execution`.

The empty result is normative and MUST be identical in meaning on both transports:

- **HTTP**: `200 OK` with body `{"execution": null}`. **Not** `404` — the account exists and
  the query succeeded; "nothing in flight" is a successful answer, not a missing resource.
- **gRPC**: a success response with the optional `execution` field absent.

So the response body wraps the envelope in an `execution` field on this operation, rather
than returning a bare envelope. The two per-proposal operations return the bare envelope.

## Response

Both operations return the same execution envelope:

| Field | Type | Required | Notes |
|---|---|---|---|
| `account_id` | string | yes | |
| `proposal_id` | string | yes | Doubles as the execution handle (FR-003) |
| `state` | enum string | yes | See vocabulary below |
| `error` | object | no | Present iff `state` is a failed state |
| `error.code` | string | yes when present | Stable code from the vocabulary below |
| `error.message` | string | yes when present | Human-readable |
| `delta_nonce` | integer | no | Present from `submitted` onward; links to the candidate delta |
| `newly_accepted` | boolean | yes | `true` when this call created the execution; `false` when it idempotently returned an already-active one. Carries the HTTP 202-vs-200 distinction over gRPC, which has no 202. On the two read operations it is always `false` |
| `proposal_exists` | boolean | yes | Whether the proposal record still exists. A **fact**, not retry advice — see the truth table below (FR-042) |
| `ignored_signatures` | integer | no | Count of stored signature entries excluded as invalid, duplicate, or non-cosigner (FR-006); for diagnosis only |
| `updated_at` | RFC 3339 string | yes | |

**Handle encoding**: `(account_id, proposal_id)` is the handle. No opaque token is
minted, so polling requires no additional state and is idempotent. This is why the
request response and the state response share one shape.

`POST` returns **`202 Accepted`** on acceptance (FR-003), or `200 OK` when it idempotently
returns an already-active execution for the same proposal. The same distinction is carried in
the `newly_accepted` field, which is the authoritative form: gRPC has no 202, so consumers
MUST read the field rather than infer from transport status.

## Execution state vocabulary

Serialized as `snake_case` strings. **Exactly five values** (FR-024) — a state exists only
where it changes what the caller does. Exhaustive; consumers MUST handle every value
(`never`-based exhaustiveness in TS, full `match` in Rust).

| `state` | Terminal | Delta | Caller should | Meaning (FR-026) |
|---|---|---|---|---|
| `pending` | no | none | wait | Accepted; not started, or finishing up |
| `proving` | no | none | wait | Delegated to the prover — the multi-minute phase |
| `submitted` | no | **always exists** | wait, watch the delta, **do not retry** | Boundary crossed (FR-047): candidate and submission evidence are durable; the network send may be pending, attempted, or of unknown outcome (FR-030) |
| `landed` | **yes** | canonical | done | Candidate canonicalized. Success |
| `failed` | **yes** | none of its own, or a discarded one | retry **only if** `proposal_exists` | Did not take effect; see `error.code`. Post-boundary failures may have had their proposal deleted with their candidate (FR-042) |

Permitted transitions, and only these:

```
pending    → proving | failed
proving    → submitted | failed
submitted  → landed | failed        (see ownership below)
```

**From `submitted`, exactly one of three parties writes the outcome** (FR-053):

| Outcome | Written by | Trigger |
|---|---|---|
| `landed` | the extended `promote_candidate` | the candidate canonicalized |
| `failed` | the **execution owner** | a **definite** application-level rejection from the node |
| `failed` | **reconciliation** | an **unknown** submission outcome, resolved as superseded or expired |

Reconciliation MUST NOT write `landed`. Observing the account at the expected commitment tells
it to wait for promotion; writing the outcome itself would race the party that owns it.

`landed` and `failed` are terminal (FR-026). The reservation is released on reaching a
terminal state; while `submitted`, it is **retained** and retry is refused, because the
submission outcome may not yet be established.

### `proposal_exists` and when retry is permitted

`proposal_exists` states a fact about storage; it is **not** permission to retry. Retry is
permitted only when `state == "failed"` **and** `proposal_exists == true`.

| `state` | `proposal_exists` | Retry permitted |
|---|---|---|
| `pending`, `proving` | `true` | no — an execution is already in flight |
| `submitted` | `true` | **no** — expressly forbidden (FR-030) |
| `landed` | `false` | no — it succeeded; the proposal is deleted on promotion |
| `failed`, pre-boundary | `true` | **yes** |
| `failed`, post-boundary, candidate discarded | `false` | no — create a **new** proposal |

An earlier revision named this field `proposal_retryable`, which was wrong in two directions:
it claimed retryable for `submitted`, where retry is forbidden, and implicitly for `landed`,
whose proposal no longer exists.

**Terminal outcomes are persisted, not derived** (FR-041). Canonicalization deletes an
unrecoverable candidate *and its proposal*, so there would be nothing left to derive from.
The write that determines the outcome — candidate promotion or deletion — MUST atomically
persist the execution's terminal state and release its reservation. Pre-terminal states
remain derived and MUST NOT be persisted, so the two representations cannot drift while both
exist.

### Internal states are not on the wire

Guardian maintains a finer-grained internal record (FR-025) — notably whether the no-retry
boundary was crossed, which FR-031's restart rule requires. Internal states MUST NOT appear
on any wire surface; each maps onto exactly one reported value. Two consequences:

- An unknown submission outcome (timeout, dropped connection, crash between send and
  response) reports `submitted`, not a distinct state. The caller's contract is already
  correct for it, and the safety property lives in the retained reservation, not in
  informing the caller. Reporting it separately would have required a state that is
  simultaneously a failure and a potential success.
- A candidate discarded after the no-retry boundary reports `failed` with a distinguishing error
  code, avoiding a second `discarded` that collides with `DeltaStatus::Discarded`.

Operators who need internal granularity get it from logs and metrics.

## Error codes

**Synchronous refusals** (FR-022) — returned by `POST`, no reservation or execution
record created:

| Code | HTTP | Cause |
|---|---|---|
| `GUARDIAN_PROVING_UNAVAILABLE` | 503 | No prover configured, capability disabled (FR-021), or the server runs in optimistic delta-commit mode (FR-043) |
| `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST` | 409 | Proposal was created by a self-executed client, so it carries no stored transaction request (FR-010). Names the cause, so the caller learns to create the proposal with a Guardian-executable client |
| `GUARDIAN_PROPOSAL_NOT_READY` | 409 | Below the effective threshold (FR-005) |
| `GUARDIAN_EXECUTION_CONFLICT` | 409 | Active reservation for a different proposal (FR-008). `meta.blocking_proposal_id` MUST name the blocker (FR-036) |
| `GUARDIAN_CONFLICT_PENDING_DELTA` | 409 | Existing; account holds a pending candidate |
| `GUARDIAN_ACCOUNT_PAUSED` | 409 | Existing |
| `GUARDIAN_ACCOUNT_RELEASED` | 409 | Existing |
| `GUARDIAN_PROPOSAL_NOT_FOUND` | 404 | Existing; the proposal itself is absent |
| `GUARDIAN_EXECUTION_NOT_FOUND` | 404 | The proposal exists but has never been executed (status reads only, never `POST`) |
| `GUARDIAN_AUTHENTICATION_FAILED` | 401 | Existing; includes non-cosigner callers |

There is deliberately **no signature-invalid error**. Invalid, duplicate, and non-cosigner
signature entries are ignored rather than fatal (FR-006), so the only signature-related
outcome is an insufficient *valid* set — reported synchronously as
`GUARDIAN_PROPOSAL_NOT_READY`. The ignored count is surfaced as `ignored_signatures` for
diagnosis. Any error code implying a fatal invalid signature would be unreachable.

**Asynchronous failure causes**, surfaced in `error.code` with `state: "failed"`:

| Code | Cause |
|---|---|
| `GUARDIAN_EXECUTION_BINDING_MISMATCH` | Reproduced summary ≠ signed commitment (FR-007) |
| `GUARDIAN_EXECUTION_STATE_MISMATCH` | Account advanced past the proposal's base |
| `GUARDIAN_EXECUTION_REQUEST_CODEC` | Envelope/checksum invalid or undeserializable (FR-014) |
| `GUARDIAN_EXECUTION_PROTOCOL_MISMATCH` | Envelope declares an incompatible Miden line (FR-015) |
| `GUARDIAN_EXECUTION_FOREIGN_INPUTS_UNSUPPORTED` | The transaction requires foreign-account (FPI) inputs, which v1 refuses before any execution (FR-050) |
| `GUARDIAN_EXECUTION_PROVING_FAILED` | Prover error or timeout (FR-019, FR-020) |
| `GUARDIAN_EXECUTION_NO_FINITE_EXPIRATION` | The proven transaction's expiration block falls outside the reconciliation horizon; refused **before** submission (FR-046) |
| `GUARDIAN_EXECUTION_ACCOUNT_INADMISSIBLE` | The pre-submission re-check found the account moved, paused, released, or no longer guarded by this Guardian (FR-048) |
| `GUARDIAN_EXECUTION_SUBMISSION_REJECTED` | Node definitively rejected the proven transaction |
| `GUARDIAN_EXECUTION_CANDIDATE_DISCARDED` | Submitted, but the candidate reached `discarded`, or the account was observed superseded (FR-040) |
| `GUARDIAN_EXECUTION_EXPIRED` | Submitted, but the chain passed the recorded expiration block with the account still at base — the transaction can never land (FR-040) |
| `GUARDIAN_EXECUTION_LEASE_EXPIRED` | Lease expired without renewal **before the no-retry boundary** (FR-028). After the boundary, expiry transfers ownership to reconciliation instead of failing, even if the network send never began |
| `GUARDIAN_EXECUTION_ABANDONED` | Interrupted before the no-retry boundary and resolved failed on restart (FR-031) |

Every code MUST be added to the TS client's error-code vocabulary in the same PR — this
is exactly the gap that produced #353 (`candidate_landed` missing from the TS
vocabulary).

## HTTP ↔ gRPC parity

Both transports MUST be observably equivalent (Constitution II). Every refusal maps to a
fixed pair, and the structured code in `error.code` / the gRPC error detail is the same
string on both sides — the transport status is a hint, the code is the contract.

| Code | HTTP | gRPC |
|---|---|---|
| `GUARDIAN_PROVING_UNAVAILABLE` | 503 | `UNAVAILABLE` |
| `GUARDIAN_PROPOSAL_MISSING_TRANSACTION_REQUEST` | 409 | `FAILED_PRECONDITION` |
| `GUARDIAN_PROPOSAL_NOT_READY` | 409 | `FAILED_PRECONDITION` |
| `GUARDIAN_EXECUTION_CONFLICT` | 409 | `ABORTED` |
| `GUARDIAN_CONFLICT_PENDING_DELTA` | 409 | `ABORTED` |
| `GUARDIAN_ACCOUNT_PAUSED` | 409 | `FAILED_PRECONDITION` |
| `GUARDIAN_ACCOUNT_RELEASED` | 409 | `FAILED_PRECONDITION` |
| `GUARDIAN_PROPOSAL_NOT_FOUND` | 404 | `NOT_FOUND` |
| `GUARDIAN_EXECUTION_NOT_FOUND` | 404 | `NOT_FOUND` |
| `GUARDIAN_AUTHENTICATION_FAILED` | 401 | `UNAUTHENTICATED` |

Additional parity requirements:

- The five state values serialize to the identical `snake_case` strings on both transports.
- `meta.blocking_proposal_id` on a conflict, and `proposal_exists` /
  `ignored_signatures` on the envelope, MUST be present on both transports with the same
  semantics.
- The new-versus-already-active distinction MUST be carried by the `newly_accepted` field on
  both transports. HTTP additionally reflects it as 202 versus 200; gRPC has no 202, so the
  field is the contract and the status is the hint.
- `GET /delta/execution/current` with nothing in flight MUST be a **success** with an absent
  execution on both transports — never `NOT_FOUND` / `404`.
- Parity MUST be verified by tests per case (SC-023), not by inspection.

## Proposal payload addition

`ProposalPayload` gains one optional field, populated only when the creating client is
configured Guardian-executable (FR-009). Absent for every other proposal, preserving the
pre-feature byte shape (FR-010, SC-009).

```json
{
  "tx_summary": { },
  "signatures": [],
  "metadata": { },
  "transaction_request": {
    "format_version": 1,
    "protocol_line": "0.15",
    "serializer_id": "0.15.2",
    "checksum": "0x…",
    "bytes": "<base64>"
  }
}
```

**Why `serializer_id` exists alongside `protocol_line`.** `MAJOR.MINOR` alone treats every
`0.16` prerelease as mutually compatible, and upstream alphas have changed serialization
between prereleases — Guardian's own 0.15→0.16 work is tracked against alpha builds. A
`protocol_line` match with a `serializer_id` mismatch MUST be refused as
`GUARDIAN_EXECUTION_PROTOCOL_MISMATCH` unless the server's configured allowlist admits the
declared value. Deserializing bytes written by a different serializer is the failure this
envelope exists to prevent, and the coarser field cannot detect it.

| Field | Required | Notes |
|---|---|---|
| `format_version` | yes | Envelope version; integer (FR-014) |
| `protocol_line` | yes | Miden protocol line the bytes were serialized against; refused if incompatible (FR-015) |
| `serializer_id` | yes | Exact serialization identity — the full `miden-protocol` version **including prerelease** (e.g. `0.16.0-alpha.4`), not just `MAJOR.MINOR` |
| `checksum` | yes | Integrity check over `bytes` before any deserialization attempt (FR-014) |
| `bytes` | yes | Base64 serialized `TransactionRequest`; subject to FR-016 size limits |

The proposal's identity is unaffected: the server derives it from `tx_summary` alone via
`delta_proposal_id(&account_id, nonce, tx_summary)`
(`crates/server/src/services/push_delta_proposal.rs:124-151`), so adding this field
changes no proposal ID (FR-012).

## Interaction with existing endpoints

- **`POST /delta` (push_delta)** MUST additionally refuse while an execution reservation
  is active for the account, with `GUARDIAN_EXECUTION_CONFLICT` (FR-027). This is a new
  refusal condition on an existing endpoint; it can only trigger when the feature is in
  use, so it is conditional rather than a compatibility break.
- **`POST /delta/proposal` (push_delta_proposal)** MUST enforce the FR-016 size limits and
  reject an oversized or malformed envelope at creation.
- No change to `GET /delta/proposal`, `GET /delta/proposal/single`,
  `PUT /delta/proposal`, or `/delta/candidate/abandon` semantics.

## Configuration

| Variable | Required | Effect |
|---|---|---|
| `GUARDIAN_TX_PROVER_URL` | to enable | `{protocol}://{host}:{port}` of the remote prover. Unset ⇒ capability unavailable, no fallback (FR-021) |
| `GUARDIAN_TX_PROVER_TIMEOUT_SECS` | no | Explicit prover timeout; MUST default well above the client library's 10 s (FR-020) |
| `GUARDIAN_PROVING_ENABLED` | no | Operator kill-switch, independent of prover reachability |
| `GUARDIAN_MAX_PROPOSAL_REQUEST_BYTES` | no | Per-request size cap (FR-016) |
| `GUARDIAN_MAX_ACCOUNT_REQUEST_BYTES` | no | Per-account aggregate cap (FR-016) |
| `GUARDIAN_EXECUTION_LEASE_SECS` | no | Reservation lease duration (FR-023, FR-028) |
| `GUARDIAN_EXECUTION_RECONCILE_INTERVAL_SECS` | no | How often reconciliation re-checks unresolved submissions (FR-040) |
| `GUARDIAN_EXECUTION_EXPIRATION_HORIZON_BLOCKS` | no | Maximum allowed distance from the reference block to expiration; exceeding it prevents crossing the no-retry boundary (FR-046) |

All MUST be documented in `docs/CONFIGURATION.md`.
