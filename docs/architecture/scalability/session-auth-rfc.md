# RFC: operation-bound session authentication and concurrent reads

Status: Superseded in part by feature 317 (2026-11-02). The operation-bound
envelope shipped as proposed (`session-v2`). Concurrent reads did not ship as
the window-only `read_replay_policy: "window"` below: session reads use
`unique` (each signed read admitted once, through a record of accepted reads,
with a 60 s window), for sessions stored by earlier releases too.
See `speckit/features/317-secure-scalability/` and
`benchmarks/prod-server/results/2026-11-02-local-317-sustained.md`.

Date: 2026-10-29. Code baseline: `ba910918`.

Related documents: [scalability gap analysis](./gap-analysis.md),
[original session specification](../../../speckit/features/011-miden-account-sessions/spec.md),
[session replay design model](./tla/README.md).

## Proposal

Introduce version 2 of Miden per-account session request authentication.
Each signature binds an explicit operation and transport in addition to the
account, timestamp, and payload already covered today. Session reads check
a short timestamp window and current authorization, without consuming a
replay floor. Session writes retain their per-account, per-session floor.

The proposed read window is **60 seconds in either direction**. Adopting
window-only reads for private accounts requires explicit acceptance of the
captured-request confidentiality risk described below. Operation binding
can ship with strict replay checks first if that decision is deferred.

This RFC does not promise a particular throughput increase. It removes the
replay-state write from each session read and permits parallel reads from
one session. A target-shaped benchmark must measure the result.

## Current behavior and constraints

The server verifies P-256 session signatures over the same
`AuthRequestMessage` word used by wallet requests. That word binds the
account ID, timestamp in milliseconds, and request payload digest. It does
not identify the operation. Several reads have identical payloads.

Every accepted per-account request then advances a strictly increasing
timestamp floor. Session floors use `session-<hash>` keys separate from
wallet floors. Parallel requests can fail when timestamps coincide or
arrival order differs from signing order. Both base clients already retry
explicit replay rejections with fresh signatures, up to two retries.

Session verification also reads the live grant, checks its Guardian key
and network, and checks that the granting signer still cosigns the account.
That work remains necessary after removing read replay floors. A successful
session `get_state` currently needs three database reads and one replay
write. Wallet requests do not have the session lookup.

Both multisig SDKs already use `get_canonical_nonce` to avoid fetching
unchanged state. The server returns the stored nonce and commitment without
loading the state blob, except for legacy rows needing nonce backfill.
The benchmark must include this existing polling flow.

Relevant implementation:

- [Request message](../../../crates/shared/src/auth_request_message.rs)
- [Account and session authorization](../../../crates/server/src/services/mod.rs)
- [Session lifecycle](../../../crates/server/src/session.rs)
- [Rust request signing and retry](../../../crates/client/src/client.rs)
- [HTTP request signing and retry](../../../packages/guardian-client/src/http.ts)
- [Browser session key](../../../packages/miden-multisig-client/src/session/key.ts)

## Scope

This proposal changes session-signed per-account requests over HTTP and
gRPC, their SDK signing interfaces, and the read replay policy. It requires
matching filesystem and Postgres semantics.

Wallet raw and EIP-712 request formats, wallet-signed grants, transaction
approvals, ACK signatures, session scope, and grant lifetime remain as
defined today. The operator dashboard and EVM cookie sessions are separate
auth domains and are outside this RFC.

Session creation, logout, and wallet revoke-all retain their dedicated
signing messages. In particular, logout remains idempotent and does not
consume an account replay floor. This RFC does not change the known grant
registration timing behavior of revoke-all.

Response encryption, streaming, distributed nonce storage, and metadata
caching are separate proposals. Public accounts receive the same session
read policy as private accounts. There is no account-type exemption and no
change to wallet-signed read replay protection.

## Security properties and accepted limits

The design must provide these properties:

- A request accepted for one operation cannot be moved to another operation
  or transport, even when their payload bytes match.
- An otherwise valid session read inside its time window is not rejected
  because another request used the same or a later timestamp.
- Every read checks current session validity and account authorization.
  A read does not gain write authority by selecting an auth version.
- A session cannot advance a wallet's replay floor. Reads cannot advance
  session write floors either.
- Reusing one signed write cannot authorize a second execution. This is
  distinct from retrying a business operation with a newly signed request.
- Failure to read authoritative authorization state fails closed.

An attacker holding a copied signed read can submit it repeatedly within
its window and receive plaintext responses while the session remains
authorized. Responses can include state or proposals created after the
original read. This is a real confidentiality change for private accounts.
TLS remains required, and signed credentials must not be logged.

With a symmetric 60-second window, a request timestamped 60 seconds ahead
can remain usable for approximately 120 seconds from signing. The window
does not protect against compromise of the session private key or scripts
that can invoke it. Those require revocation and the existing delegation
limits. Window-only reads also permit repeated resource consumption;
existing rate limits remain necessary.

Current floors already allow a captured request to win a race against the
legitimate sender. They prevent reuse after consumption, not all leakage.
The decision here is whether bounded reuse after consumption is acceptable.

## Version 2 signed message

Per-account requests use `x-auth-format: session-v2` in HTTP headers or
gRPC metadata. The existing public-key, signature, and timestamp encodings
remain unchanged. No client-provided operation header is trusted.

The proposed message bytes are the following fixed-length concatenation:

```text
ASCII("GUARDIAN_SESSION_REQUEST")
|| 0x00
|| u16be(2)
|| u8(transport)
|| u16be(operation)
|| AuthRequestMessage(account_id, timestamp_ms, request_payload).to_word().as_bytes()
```

`transport` is `1` for HTTP JSON and `2` for gRPC protobuf. The final word
is exactly 32 bytes in the existing shared word serialization. The domain
prefix has one trailing zero byte. Integers have the widths and byte order
shown above. No other fields, padding, or text conversions are included.

P-256 ECDSA signs these bytes with SHA-256, using raw 64-byte `r || s`
signatures and the existing compressed 33-byte public-key encoding.
WebCrypto receives the message bytes with `hash: "SHA-256"`; it must not
receive a precomputed SHA-256 digest and hash it again. The Rust signer
must perform the equivalent operation. New shared constructors own this
encoding; transport handlers and feature code must not assemble it ad hoc.

The inner word preserves existing HTTP canonical-JSON and protobuf payload
rules. Equivalent HTTP and gRPC requests intentionally have different
signatures. Cross-language fixtures must test each transport's encoding,
including payloads that serialize identically across different operations.

Guardian identity and network continue to be enforced through the live
grant on every request. This envelope introduces no independent audience
field and grants no authority when the session lookup fails.

### Operation registry

The HTTP handler supplies the operation from the matched method and route.
The gRPC handler supplies it from the invoked RPC. IDs are permanent and
must not be reused. Unknown IDs and unregistered session routes fail
closed. A new operation requires an explicit policy entry and tests.

| ID | Operation | HTTP method and path | RPC on `guardian.Guardian` | Replay policy |
|---|---|---|---|---|
| 1 | `GetState` | `GET /state` | `GetState` | Read window |
| 2 | `GetCanonicalNonce` | `GET /state/nonce` | `GetCanonicalNonce` | Read window |
| 3 | `GetDelta` | `GET /delta` | `GetDelta` | Read window |
| 4 | `GetDeltaSince` | `GET /delta/since` | `GetDeltaSince` | Read window |
| 5 | `GetDeltaHistory` | `GET /delta/history` | `GetDeltaHistory` | Read window |
| 6 | `GetDeltaProposals` | `GET /delta/proposal` | `GetDeltaProposals` | Read window |
| 7 | `GetDeltaProposal` | `GET /delta/proposal/single` | `GetDeltaProposal` | Read window |
| 8 | `PushDelta` | `POST /delta` | `PushDelta` | Strict write floor |
| 9 | `PushDeltaProposal` | `POST /delta/proposal` | `PushDeltaProposal` | Strict write floor |
| 10 | `SignDeltaProposal` | `PUT /delta/proposal` | `SignDeltaProposal` | Strict write floor |

Binding the operation ID avoids signing proxy hostnames, URL prefixes, or
query-string ordering. Each registered method and path maps to exactly one
operation. Query and body values remain covered by the payload digest.
An alias may share an ID only when it is the same operation with identical
authorization and payload semantics.

Wallet-only routes remain wallet-only. Their session rejection handling
must understand the new format without dispatching it through wallet
verification or accepting the legacy account digest as a v2 signature.
No new operation ID implicitly enables a route for sessions.

## Read authorization and consistency

Validate credential encodings and the read timestamp window, then verify
the operation-bound signature before querying the session store. Resolve
the live session and current account metadata, validate the grant's
Guardian and network, and confirm current cosigner membership. Only then
read and return the requested data. No read consults or advances its
session replay floor.

When window-only reads are enabled, the read window is a protocol constant,
proposed as `abs(server_now_ms - timestamp_ms) <= 60_000`. The binding-only
stage described below retains strict checks and the existing window.
Expiry uses server time,
not the signed request timestamp. Recheck freshness and expiry after
potentially long storage waits before admitting the response. Unbounded
handlers must not turn a short window into an indefinite authorization.

Revocation and signer-removal semantics are defined at the authoritative
authorization read. A request whose authorization check occurs after the
relevant commit must fail. A request already authorized before the commit
may finish. This RFC does not promise cancellation of in-flight responses.
Use primary database reads for authorization. A lagging read replica or
TTL-only cache cannot satisfy this rule.

The initial implementation keeps the existing session and metadata
lookups. A later compact lookup may join the live session, account auth
version, and state head. It must preserve realm isolation, expiry,
revocation, cosigner membership, and error classification. A metadata
version alone cannot establish that a session is still live.

Account pause continues to block the existing mutation paths. Reads remain
available, as specified in [dashboard documentation](../../DASHBOARD.md#account-pausing).
Do not introduce read denial through a new cached pause flag.

Read classification means no user-requested business mutation. Legacy
nonce backfill remains an internal conditional repair and must retain its
existing concurrency guard. Such repairs are measured separately from
replay writes in the benchmark.

## Write replay policy and client concurrency

Writes keep the current 300-second timestamp skew allowance and strict CAS
on the existing account/session replay key. Do not reset floors during the
version cutover or create separate floors per operation. Reads never
influence the write timestamp generator in v2.

SDKs serialize writes per account and session within a client ownership
scope, holding that ordering across explicit replay retries. They generate
strictly increasing millisecond timestamps from an injected clock and
per-key state. Reads use the clock directly and may share timestamps.
Client clones sharing a session must share the write sequencer. Both SDKs
must expose the same behavior without serializing unrelated accounts.

Independent processes or browser tabs sharing a persisted key are not
coordinated by a local sequencer. They can still encounter replay errors.
This RFC does not promise zero write replay errors across those owners.
Use separate grants for independent writers when that guarantee is needed.
Retain the existing bounded retries for explicit `authentication_replay`
responses. Do not silently retry an ambiguous write timeout with a fresh
signature: the caller must reconcile whether the operation committed.

Preserve session-expiry, revocation, and authorization error distinctions.
An unsupported request-auth version is a separate, terminal compatibility
error and must not automatically clear the grant or switch to wallet
signing. Existing explicit session lifecycle behavior remains separate
from version negotiation.

### Stalled handlers and floor cleanup

The [existing auth replay model](../../../.agents/skills/verify-auth-replay/SKILL.md)
records F2: a duplicate handler can pass time and session checks, stall
until its floor is purged, then consume the timestamp again. Retaining
today's floor logic alone does not prove the write property above.

Before enabling the new read policy, the implementation must close F2 or
document and obtain review of a concrete bounded-execution assumption.
The preferred implementation direction is freshness enforcement at the
storage authorization boundary, with bounded connection acquisition and
handler lifetime as additional controls. A check before a pool wait is
insufficient. A timeout that leaves detached mutation work running is
also insufficient.

The implementation design must specify the exact atomic boundary relative
to CAS, purge, and subsequent effects for both backends, then pass the
updated step-level model. This RFC does not prescribe an unverified SQL
predicate as a complete fix. F2 is a release gate, not an assumed property
of the proposed envelope.

## Errors and compatibility

Keep `authentication_replay` for strict-floor losses. Window-only reads
must not produce it. Out-of-window requests keep the existing terminal
authentication failure behavior; a metric distinguishes timestamp failure
from invalid signatures without exposing credentials.

Add the stable error code `unsupported_auth_version` for recognized but
unsupported session request formats. Proposed mapping: HTTP 400 and gRPC
`InvalidArgument`, with non-retryable structured metadata. Match the
repository's wire-code prefix convention and update both client enums.
Malformed credentials remain authentication or input failures under the
existing taxonomy. Session-invalid errors keep their current meanings.

Advertise `request_auth_version: 2`, `read_replay_policy: "window"`, and
`read_timestamp_skew_ms: 60000` in the session creation response on both
transports and in HTTP `/status.sessions`. A binding-only deployment
advertises `read_replay_policy: "strict"` and a read skew of `300000`.
These fields describe server policy; callers cannot choose a weaker policy
per request. Validate required values at the SDK boundary. Missing version
or protobuf zero means unsupported, not v1 fallback. Future versions need
their own codec and explicit support; a version number is not negotiation.

Resumed clients use the v2 codec and handle an incompatible server
explicitly. Cached discovery must not override server verification or
justify retrying another auth format. The read window must be known before
benchmarking; a changed policy requires fresh discovery or session setup.

This is a coordinated breaking session-request change, with the release
number chosen under the repository's Miden dependency policy. It does not
itself require a Miden dependency upgrade. Do not label it an unconditional
`0.19.x` additive change.

The cutover server rejects `x-auth-format: session` on per-account routes.
It retains the existing logout format solely on the dedicated logout
route. Existing live grants and stored keys can sign v2 requests after an
SDK upgrade because the grant bytes and key usages do not change. No grant
migration, new wallet prompt, or replay-floor reset is required solely for
this envelope change.

Deploy a homogeneous v2 server fleet before directing v2 traffic to it.
Use a drained or blue/green cutover; mixed v1/v2 replicas behind one endpoint
would cause intermittent auth failures. Coordinate SDK releases and browser
application upgrades. An incompatible rollback fails explicitly and does
not downgrade signatures. This RFC adds no dual-format compatibility layer.

## Alternatives

| Alternative | Assessment |
|---|---|
| Serialize all reads in SDKs | Preserves current policy but limits concurrency and does not coordinate independent callers. Useful only if read replay relaxation is rejected. |
| Shared request-ID or nonce store | Can accept out-of-order unique requests while preserving duplicate rejection, but needs atomic distributed storage and explicit duplicate-read semantics. Keep as the security-preserving alternative if window-only reads are rejected. |
| Per-replica replay cache | Cannot guarantee fleet-wide duplicate rejection and would reintroduce read failures. Do not make authorization depend on it. |
| Encrypt responses | Can protect against a captured-request attacker but requires a separate key design and response protocol. Not part of this RFC. |
| Authenticated streams | May reduce polling but require connection lifecycle, revocation, and reconnect semantics. Evaluate separately after polling measurements. |

Browser session keys are non-extractable ECDSA signing keys. They cannot
be reused directly as ECDH keys through WebCrypto, whose derivation APIs
check both algorithm and key usages. A future encryption proposal should
authenticate a separate encryption key and define response binding,
rotation, and downgrade protection. See the
[WebCrypto derivation rules](https://www.w3.org/TR/webcrypto-1/#SubtleCrypto-method-deriveBits).
The design model's idealized encryption switch is not evidence that the
current browser key can implement that protocol.

## Implementation and validation plan

Implement from the server contract upward. The following is future work,
not a claim that this documentation change modifies these components.

| Layer | Required work |
|---|---|
| Shared Rust | Add the typed operation registry and session request envelope under `crates/shared/src/`; extend P-256 helpers without changing grant/logout signing. Add shared fixtures under `crates/shared/tests/fixtures/`. |
| Server | Update `metadata/auth/credentials.rs`, `api/http.rs`, `api/grpc.rs`, `services/mod.rs`, session creation/status responses, and replay-store boundaries. Pass typed operation context explicitly. Keep business rules in services and backend parity. |
| Contract | Update `crates/server/proto/guardian.proto`, HTTP schemas, error taxonomy, and all relevant `utoipa` annotations. Regenerate OpenAPI with `cargo run --features evm --bin gen-openapi -- docs`. |
| Rust base client | Update `crates/client/src/client.rs`, auth context, session signing, typed errors, response parsing, write sequencing, and retry tests. |
| TS base client | Update `packages/guardian-client/src/types.ts`, `auth-request.ts`, `http.ts`, server types, conversion/error parsing, and tests. Add a required operation argument to `SessionRequestSigner.signRequest`. |
| Multisig SDKs | Update session adapters in both SDKs, especially `packages/miden-multisig-client/src/session/`. Preserve Falcon and ECDSA wallet grant flows and existing nonce-precheck sync behavior. |
| Examples | Exercise `examples/demo`, `examples/web`, and `examples/smoke-web` with sessions, wallet-only operations, revocation, and concurrent reads. |
| Documentation | Update `spec/api.md`, the original session spec, `docs/MULTISIG_SDK.md`, `docs/MIDEN_COMPATIBILITY.md`, and READMEs for shared, both base clients, and both multisig SDKs. Include the SDK signature-interface break and rollout requirements. |

Required correctness checks:

- Shared HTTP and gRPC vectors pin exact message bytes, inner words,
  signatures, and verification outcomes in Rust and TypeScript. Changing
  version, operation, transport, account, timestamp, or payload invalidates
  the signature. Include same-payload read pairs and synthetic colliding
  write operations in tests.
- Concurrent reads with equal and reversed timestamps succeed. Reads
  before and after writes never consume or depend on write floors.
  Replayed writes fail before business effects, including across replicas.
- Revocation, expiry, signer removal, Guardian-key rotation, and network
  mismatch still deny authorization. Test requests authorized before and
  after a revocation commit, storage failures, and pool-wait clock changes.
- Paused accounts remain readable and reject the same mutations as today.
  Wallet-only operations and logout retain their intended signing domains.
- Clock tests cover both window boundaries, future timestamps, clock
  rollback, write sequencing, independent owners, and retry exhaustion.
- Version tests cover old SDK/new server, new SDK/old server, persisted
  session keys, unsupported versions, and no automatic downgrade.
- Extend the design model with write false-rejection and stated revocation
  semantics. Update the code-mirroring auth replay model for separate read
  and write policies, handler steps, and purge behavior. Run its deep tier;
  do not suppress F2 by weakening the write property or hiding an await.

Run targeted shared/server/base-client tests first, then affected multisig
tests and browser builds. Exercise both Falcon and ECDSA grant signers,
Rust and browser sessions, and filesystem/Postgres backends. Follow the
repository's ignored-test convention for live Postgres tests. Complete the
manual example checks and OpenAPI drift check before implementation ships.

### Performance experiment

Establish the baseline before changing replay behavior. Compare v1 strict,
v2 strict, and v2 window-only under the same infrastructure and workload.
This separates envelope overhead from savings due to removing floor writes.

Use 100,000 accounts, 20,000 reader identities at explicitly stated polling
rates, and 100 writers with an independently specified write rate. Include
unchanged-state nonce polling, changed-state fetches, representative state
sizes, hot-account contention, and overlapping read/write sessions. Include
multi-replica traffic and a sustained run, not just a finite burst.

Measure route p50/p95/p99, offered and completed request rates, first-attempt
errors, errors hidden by SDK retries, DB reads/writes, pool wait, CPU, and
accepted-to-canonical latency. Record client clock offset and session
expiry separately. Do not label expired or unauthorized traffic as a
legitimate replay failure. Use bounded metric labels, never account IDs,
session public keys, signatures, or payloads.

Acceptance for the replay change is zero ordering-related read rejections
for valid in-window traffic, and zero replay-floor writes for v2 reads.
Measure write replay failures separately. The broader p95 <= 1 second read
and p95 <= 30 seconds canonicalization targets remain system-level goals;
passing this RFC's tests does not establish either capacity target.

## Decisions required before implementation

1. Accept or reject bounded captured-read reuse for private accounts. The
   recommendation is a 60-second symmetric window, subject to security
   review and measured client clock behavior. If rejected, retain strict
   checks while evaluating a shared nonce design.
2. Approve the v2 envelope, operation IDs, and typed SDK API change through
   cross-language test vectors and security review.
3. Specify and model the write authorization boundary that closes F2, or
   approve an explicit enforceable execution bound. Do not infer one from
   typical request latency.
4. Choose the coordinated release and application cutover. Operation
   binding may ship first with strict checks; enabling window-only reads
   remains a separate reviewed policy change, even with unchanged wire bytes.

This draft recommends operation binding immediately as design work, then
window-only reads once the confidentiality decision and write replay gate
are resolved. Response encryption is not a prerequisite for drafting or
benchmarking the operation-bound protocol.
