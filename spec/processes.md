# Processes

## Services overview

- **configure_account**: creates a Miden account by validating the provided network configuration and auth policy, then storing account metadata and the initial state with its commitment and account nonce. Every entry in `auth.cosigner_commitments` must be a canonical commitment (`0x` plus 64 lowercase hex digits) and the list must be non-empty and duplicate-free. For MultisigGuardian accounts the list must exactly match the signer map extracted from `initial_state`, including the map's canonical (index) order — the stored list is the authorization source of truth for every later request, so any mismatch is rejected as `InvalidInput`. EVM accounts are not configured through this service.
- **push_delta**: verifies a Miden delta against the tail of the account's candidate queue (the canonical state when nothing is queued, always so at the default depth of one), computes the new state's commitment and account nonce, attaches an acknowledgement, and either enqueues it as a candidate (canonicalization enabled) or immediately applies it and marks it canonical (optimistic mode). When the tail account is a multisig, the acknowledgement is refused with `insufficient_signatures` unless a matching pending proposal carries enough cryptographically verified cosigner signatures for the account procedures the delta invokes. The required count mirrors the on-chain derivation: every invoked procedure contributes its per-procedure threshold (otherwise the account default) and the maximum applies. Which procedures were invoked is derived from the transaction summary itself — input notes imply the receive procedure, output notes other than the fee note imply the send procedure, patches to the auth component's configuration slots imply its update procedures, any other storage patch is held to the account default, and a summary that shows no invoked procedure at all (a bare nonce bump) is held to the account default, the on-chain fallback — so the proposal's claimed `proposal_type` can raise the requirement but never lower it. Signatures from keys outside the account's approver set do not count, and a storage fault while loading that proposal refuses the push. The gate runs before the delta is verified or applied. Single-key accounts are unchanged. EVM accounts do not support `push_delta` in v1.
- **get_state**: authenticates and returns the latest persisted account state.
- **get_canonical_nonce**: authenticates and returns the account nonce and commitment stored with the latest persisted account state, without loading the state blob, so a client can skip `get_state` when that nonce is below its local nonce, or equal to it at the same commitment (issue #191). A state stored before nonces were kept is decoded once, and its nonce is backfilled onto the row only while the row still holds that state.
- **get_delta**: authenticates and returns a specific delta by nonce.
- **get_delta_since**: authenticates, fetches deltas after a given nonce (excluding discarded), merges their payloads via the network client, and returns a single merged delta snapshot.
- **push_delta_proposal**: creates a pending Miden proposal by validating `tx_summary` against the tail of the account's candidate queue (the canonical state when nothing is queued) and deriving IDs through the Miden network client. It is refused with `409 conflict_pending_delta` when its delta could never be admitted: while the queue is full, when a queue exists and its nonce is not the newest queued candidate's plus one, or when the newest queued candidate changes the account's signer set or guardian key.
- **sign_delta_proposal**: appends one signer signature to a pending Miden proposal.
- **evm_session**: issues an EIP-712 wallet challenge, recovers the EOA with `ecrecover`, consumes the nonce once, and creates a cookie-backed session.
- **evm_accounts**: registers EVM smart accounts under `/evm/accounts` by validating the cookie session signer, server-owned chain config, ERC-7579 validator installation, and signer snapshot before storing account metadata without state or acknowledgement data.
- **evm_proposals**: creates, lists, approves, fetches executable data for, and cancels EVM proposals with opaque payloads, UserOperation hashes, signer snapshots, TTL, and lazy EntryPoint nonce cleanup.

### Diagrams

#### configure_account
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant N as Network
  participant ST as Storage
  participant M as Metadata
  C->>S: POST /configure {account_id, auth, network_config?, initial_state} + credentials
  S->>S: verify timestamp (within 300s skew window)
  S->>S: validate network_config for account_id
  S->>N: validate_credential(initial_state, credential)
  S->>N: should_update_auth(initial_state)\n(extract signer map)
  S->>S: reject unless auth.cosigner_commitments == extracted signer map\n(exact set and order)
  S->>S: auth.verify(account_id, timestamp, request_payload_digest, credential)
  S->>N: get_state_head(account_id, initial_state)\n(commitment, nonce)
  alt existing account
    S->>M: update last_auth_timestamp (verified signer, CAS)
  end
  S->>ST: submit_state(state_json, commitment, nonce)
  S->>M: set(account_id, auth, network_config, timestamps)
  alt first-time account
    Note over S,M: metadata must exist first because replay state references it by FK
    S->>M: seed last_auth_timestamp (verified signer, CAS)
  end
  S-->>C: 200 {account_id, ack_pubkey, ack_commitment}
```

#### push_delta
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  participant N as Network
  C->>S: POST /delta {delta, credentials}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp (per signer)
  S->>M: update last_auth_timestamp (per signer, CAS)
  alt EVM account
    S-->>C: error unsupported_for_network
  else Miden account
    S->>ST: pull_state(account_id)
    S->>ST: pull_candidate_deltas(account_id)
    opt the queue no longer chains from the state read, or it is empty\nand prev_commitment is not the state read
      S->>ST: pull_state_commitment(account_id)\n(a promotion may have raced the two reads)
      S->>ST: on a change: pull_state + pull_candidate_deltas again
    end
    alt the queue is full, still does not chain from the canonical state,\nprev_commitment competes with a queued candidate's base,\nor nonce does not exceed the newest queued candidate's
      S-->>C: 409 ConflictPendingDelta
    else prev_commitment is neither the canonical commitment\nnor a queued candidate's post-state
      S-->>C: 400 CommitmentMismatch (expected = canonical)
    else prev_commitment is the queue tail
      S->>S: replay queued payloads onto the canonical state\n(tail state; no-op when the queue is empty)
      S->>N: account_auth_binding(canonical_state), account_auth_binding(tail_state)
      alt the tail binds another signer set or guardian key
        S-->>C: 409 ConflictPendingDelta
      end
      S->>ST: pull_delta_proposal(account_id, summary commitment)
      alt the tail account is a multisig and the matching proposal is missing,\nits verified approver signatures are below the effective threshold\nof the procedures the delta invokes,\nor the proposal row cannot be loaded
        S-->>C: 400 InsufficientSignatures, or 500 on a storage fault
      end
      S->>N: verify_delta(tail_commitment, tail_state, payload)
      S->>N: apply_delta(tail_state, payload)\n(new_state_json, new_commitment, new_nonce)
      alt a candidate is queued and new_nonce is not the delta's nonce
        S-->>C: 409 ConflictPendingDelta
      end
      S->>S: ack_delta(delta.new_commitment) -> ack_sig
      alt canonicalization enabled
        S->>ST: submit_candidate(candidate)\n(the same gate again, under the account lock)
      else optimistic mode
        S->>ST: submit_state(new_state)
        S->>ST: submit_delta(canonical)
      end
      S-->>C: 200 {delta}
    end
  end
```

#### get_state
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  C->>S: GET /state?account_id=... {credentials}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp (per signer)
  S->>M: update last_auth_timestamp (per signer, CAS)
  S->>ST: pull_state(account_id)
  S-->>C: 200 {state}
```

#### get_canonical_nonce
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  participant N as Network Client
  C->>S: GET /state/nonce?account_id=... {credentials}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>M: update last_auth_timestamp (per signer, CAS)
  S->>ST: pull_state_head(account_id)\n(commitment, stored nonce)
  alt nonce stored with the state
    S-->>C: 200 {account_id, nonce, commitment}
  else state stored before nonces were kept
    S->>ST: pull_state(account_id)
    S->>N: account_nonce(state_json)
    S->>ST: backfill_state_nonce(account_id, commitment, nonce)\n(only while the row still holds this commitment)
    S-->>C: 200 {account_id, nonce, commitment}
  end
```

#### get_delta
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  C->>S: GET /delta?account_id=...&nonce=... {credentials}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp (per signer)
  S->>M: update last_auth_timestamp (per signer, CAS)
  S->>ST: pull_delta(account_id, nonce)
  S-->>C: 200 {delta}
```

#### get_delta_since
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  participant N as Network
  C->>S: GET /delta/since?account_id=...&nonce=... {credentials}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp (per signer)
  S->>M: update last_auth_timestamp (per signer, CAS)
  S->>ST: pull_deltas_after(account_id, nonce)
  S->>S: filter -> only canonical
  S->>N: merge_deltas(delta_payloads) -> merged_payload
  S->>S: build merged_delta (nonce=last, prev=first.prev, new=last.new, status=canonical)
  S-->>C: 200 {merged_delta}
```

#### push_delta_proposal
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  participant N as Network
  C->>S: POST /delta/proposal {account_id, nonce, delta_payload}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp (per signer)
  S->>M: update last_auth_timestamp (per signer, CAS)
  S->>ST: pull_state(account_id)
  S->>ST: pull_candidate_deltas(account_id)
  opt the queue no longer chains from the state read, or it is empty
    S->>ST: pull_state_commitment(account_id)\n(a promotion may have raced the two reads)
    S->>ST: on a change: pull_state + pull_candidate_deltas again
  end
  alt the queue is full, does not chain from the canonical state,\nor a queue exists and nonce is not the newest queued candidate's plus one
    S-->>C: 409 ConflictPendingDelta
  else
    S->>ST: pull_pending_proposals(account_id)
    alt viable proposals (pinned to the tail, nonce above the tail's) reach the limit
      S-->>C: 409 PendingProposalsLimit
    else
      S->>S: replay queued payloads onto the canonical state\n(tail state; no-op when the queue is empty)
      S->>N: account_auth_binding(canonical_state), account_auth_binding(tail_state)
      alt the tail binds another signer set or guardian key
        S-->>C: 409 ConflictPendingDelta
      end
      S->>N: verify_delta(tail_commitment, tail_state, tx_summary)
      S->>N: delta_proposal_id(account_id, nonce, tx_summary)
      S->>ST: submit_delta_proposal(id, pending_delta)\n(prev_commitment = tail commitment)
      S-->>C: 200 {delta, commitment:id}
    end
  end
```

#### sign_delta_proposal
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  C->>S: PUT /delta/proposal {account_id, commitment, signature}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp (per signer)
  S->>M: update last_auth_timestamp (per signer, CAS)
  S->>ST: pull_delta_proposal(account_id, commitment)
  S->>S: ensure status.pending & signer not recorded
  S->>S: derive signer commitment from x-pubkey
  S->>ST: update_delta_proposal(commitment, append signature)
  S-->>C: 200 {delta_with_signatures}
```

#### evm_accounts_and_proposals
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant A as SmartAccount
  participant V as Validator
  participant E as EntryPoint
  participant ST as Storage
  participant M as Metadata
  C->>S: GET /evm/auth/challenge?address=...
  S-->>C: EIP-712 challenge
  C->>S: POST /evm/auth/verify {address, nonce, signature}
  S->>S: ecrecover challenge signer & consume nonce
  S-->>C: Set-Cookie guardian_evm_session
  C->>S: POST /evm/accounts {chain, account, validator}
  S->>A: isModuleInstalled(1, validator, 0x)
  S->>V: getSignerCount/getSigners/threshold
  S->>S: verify session EOA is a validator signer
  S->>M: store account metadata
  C->>S: POST /evm/proposals {account_id, user_op_hash, payload, nonce, signature}
  S->>M: load EVM account metadata
  S->>A: isModuleInstalled(1, validator, 0x)
  S->>V: getSignerCount/getSigners/threshold
  S->>S: verify proposer and initial signature against signer snapshot
  S->>ST: store active EVM proposal
  C->>S: POST /evm/proposals/{id}/approve {account_id, signature}
  S->>ST: load EVM proposal
  S->>E: getNonce(account, nonce_key)
  S->>S: delete if expired or finalized
  S->>S: verify signer is in stored snapshot and signature is unique
  S->>ST: append signature
  C->>S: GET /evm/proposals/{id}/executable?account_id=...
  S-->>C: {hash, payload, signatures, signers} once threshold is met
```

#### get_delta_proposals
```mermaid
sequenceDiagram
  autonumber
  participant C as Client
  participant S as Server
  participant M as Metadata
  participant ST as Storage
  C->>S: GET /delta/proposal?account_id=... {credentials}
  S->>M: get(account_id) & verify(credentials, timestamp, request_payload_digest)
  S->>S: check timestamp > last_auth_timestamp (per signer)
  S->>M: update last_auth_timestamp (per signer, CAS)
  S->>ST: pull_all_delta_proposals(account_id)
  S->>S: filter(status.pending) & sort_by_nonce
  S-->>C: 200 {proposals}
```

## Canonicalization

### Modes
- Candidate mode (enabled): `push_delta` stores deltas as `candidate`; a background worker promotes or discards them after verification.
- Optimistic mode (disabled): `push_delta` marks deltas as `canonical` immediately and updates state.

### Configuration
- Shipped server builder configuration: submission_grace_period_seconds = 600
  (10m), check_interval_seconds = 10, fast_promotion_enabled = true,
  fast_promotion_interval_seconds = 3,
  fast_promotion_window_seconds = 30, max_retries = 48,
  divergence_confirmations = 2, max_concurrent_accounts = 10,
  retained_ttl_seconds = 86400 (24h; 0 disables retention and restores
  the historical delete-on-give-up behavior),
  reconcile_interval_seconds = 60, reconcile_page_size = 100.
- These values are configured in code, not through server env vars. The
  exceptions are `GUARDIAN_CANONICALIZATION_FAST_PROMOTION_ENABLED=false`,
  which disables the promotion-only pass,
  `GUARDIAN_CANONICALIZATION_MAX_CONCURRENT_ACCOUNTS`, which overrides account
  concurrency at startup, `GUARDIAN_CANONICALIZATION_RETAINED_TTL_SECONDS`,
  which overrides the retained TTL (`0` is the runtime kill switch for
  retention), and `GUARDIAN_CANONICALIZATION_RECONCILE_INTERVAL_SECONDS`,
  which overrides the reconcile pass cadence.

### Worker Behavior
- A full pass runs every `check_interval_seconds` and owns all retry,
  divergence, and discard decisions.
- Between full passes, a promotion-only pass runs every
  `fast_promotion_interval_seconds` for candidates younger than
  `fast_promotion_window_seconds`. It scans candidates directly in storage in
  bounded, oldest-first pages, carrying a cursor across passes so bursts are
  visited fairly. The pass stops admitting new work when its next cadence tick
  or the next full-pass tick is due; already-started candidate work finishes.
  It first compares each stored `new_commitment` with the chain and reconstructs
  state only after that cheap probe matches (a probe that reports the
  post-state of a candidate queued after this one qualifies too — the chain
  landed through it). Promotion still requires the candidate to chain from
  the stored state and the reconstructed commitment to equal the claimed
  commitment before the normal auth refresh and fenced write. Missing,
  incorrect, or not-yet-landed claims, and orphaned candidates, are left
  unchanged for the next full pass.
- The fast pass never increments `retry_count` or `divergence_count`, applies
  `submission_grace_period_seconds`, or discards a candidate. Those behaviors
  belong exclusively to full passes. Both pass types use
  `max_concurrent_accounts`; candidates within one account remain sequential.
- Candidate queue (issue #17): an account holds up to
  `max_pending_candidates_per_account` candidates (env
  `GUARDIAN_MAX_PENDING_CANDIDATES_PER_ACCOUNT`, at most 16; the default,
  `1`, is the historical one-in-flight behavior and queueing deeper is an
  operator opt-in) as a strictly ordered chain. `push_delta` admits a
  delta only on the queue *tail* — the newest queued candidate's
  post-state, or the canonical state when nothing is queued — with a nonce
  above the tail's; the tail state is replayed from the canonical state on
  demand and never persisted. The admission rules, in order: a full queue
  refuses every delta, whatever its base (so depth 1 behaves exactly as
  before the queue existed); a queue that no longer chains from the
  canonical state (a predecessor left it without promoting and the worker
  has not swept the orphans yet) refuses everything; a delta competing for
  a base another queued candidate already claimed, or whose nonce does not
  exceed the tail's, is refused; so is anything behind a tail that changes
  who may act on the account (the signer set or guardian key its state
  binds differs from the canonical state's; both bindings are read in one
  task on the reconstruction pool and compared on the replayed tail rather
  than on a proposal label, since a direct push changes signers without
  one): requests stay authorized against the canonical signer set until
  that candidate promotes, and a successor this server acknowledges behind
  a queued guardian switch could never land; and last, a delta behind a
  queued candidate whose label is not the nonce it leaves the account at
  (checked on the request path only, once the delta is applied and before
  the storage gate runs, so one that fails never reaches the lock: a
  timestamp label would sort past every real nonce and refuse each
  correctly labelled successor until it promoted; with nothing queued the
  label is not checked, so a client that labels with a timestamp still
  works at the head), all with `409
  conflict_pending_delta`; a delta building on a state the server does not
  know gets `400 commitment_mismatch` against the canonical commitment.
  Both storage backends re-evaluate the chain-position rules under the
  account lock, so two racing submissions cannot both extend the tail and
  nothing is admitted behind an orphan; the binding and label rules are
  judged in the request path only, which is safe because a successor can only name a
  tail that exists once its candidate is admitted, and a candidate
  admitted in between moves the tail, so the lock-side position check
  refuses the successor as competing. The state and the queue are read separately, so a promotion
  can land between the two reads; admission re-reads the stored
  commitment when that would change its verdict (a queue that no longer
  chains, or an empty one that a delta on another base or a proposal is
  judged against) and re-validates against the fresh state. Proposals are
  pinned to the tail as well (their `prev_commitment` is the tail
  commitment) and are refused up front when their delta could never be
  admitted: while the queue is full, when a queue exists and their nonce
  is not the tail's plus one, or behind a tail that changes the signer set
  or guardian key. Both SDKs label a proposal with the account's next
  nonce (the TypeScript SDK since the release after 0.18.0; earlier
  releases used a timestamp, which the rule refuses), so a proposal built
  on the tail carries the tail's nonce plus one, and one built on the
  canonical state (all `/state` serves) carries the tail's nonce or less:
  the cosigner on another device proposing while this device's candidate
  is queued. The queue therefore serves the device that pushed the newest
  candidate; every other cosigner is refused until it drains, as at depth
  one. The server cannot tell which state a summary was built on, so the
  nonce rule and, at execution, the SDKs keep a proposal from executing
  anywhere but on the tail: both SDKs refuse to execute a proposal pinned
  to a state the client does not hold (TypeScript 0.18.0 and earlier
  pushed the pinned base regardless; a switch proposal is checked while
  the pre-switch GUARDIAN serves it), and both push the state they
  executed on, which the delta gate refuses unless it is the tail. Only viable proposals (pinned to the tail with a nonce above the
  tail's) count toward the pending-proposal limit, which is checked before
  the tail replay. Promotion of the oldest candidate
  moves the canonical state *along* the chain, so the tail commitment —
  and every proposal pinned to it — stays valid while the queue drains.
  A queued payload that no longer replays (an upgrade changed delta
  application while it was queued) refuses admissions with `409
  conflict_pending_delta` until the queue drains. The pending-candidate
  flag is released only once no candidate remains queued.
- For each account with a pending candidate:
  - Pull candidate deltas (`pull_candidate_deltas`, a store-side status
    filter — canonical and discarded history rows never leave the store);
    process in nonce order, as a chain from the stored state: a candidate
    whose base is the stored state is verified; one whose base is the
    post-state of the candidate queued just before it, while that
    predecessor is still queued (deferred or retried), is waiting on it,
    so the account's pass stops there; one whose base is neither — or
    whose predecessor left the queue on this very pass (parked,
    discarded, or abandoned) — is an *orphan* and is parked as
    `retained` with reason `orphaned` (discarded when retention is off)
    without any chain observation, together with every candidate queued
    after it, until one chains from the stored state again. A client
    abandon intent on an orphan resolves as `client_abandoned` instead.
  - Apply delta locally to compute expected state and commitment.
  - Fetch the on-chain commitment and classify:
    - Matches the post-state of a candidate queued after this one: the
      chain landed *through* this candidate (its successors were admitted
      chained from its post-state), so canonicalize it with the recomputed
      state exactly as below — provided the recomputed commitment is still
      the post-state its successors chained from; the successors verify on
      their own turn as the stored base advances along the chain.
    - Matches the expected new commitment: canonicalize —
      persist new state (atomic with delta status update when possible),
      optionally update auth from chain via `should_update_auth`, set delta
      status to `canonical`, and delete the matching Miden delta proposal
      identified via `delta_proposal_id(account_id, nonce, delta_payload)`.
      The persisted commitment is the recomputed one the verification
      proved on-chain; a client-supplied `new_commitment` that differs (or
      is absent) is logged and counted but never blocks promotion.
      Promotion is additionally gated on the stored state still sitting at
      the candidate's `prev_commitment` — if a concurrent write moved it,
      the promotion rolls back (`stale_base`) and the next tick
      re-verifies against the new base.
    - Matches the candidate's `prev_commitment` (its transaction has not
      landed yet), or the comparison itself failed (RPC error): defer within
      `submission_grace_period_seconds`, then consume retry budget on each
      full-pass tick. After `max_retries` the candidate is parked as
      `retained` with reason `retry_exhausted` (issue #345) — not deleted —
      the account's pending-candidate flag is cleared, and the matching
      proposal is deleted (the delta row carries everything reconciliation
      needs; a proposal left `pending` would be stranded forever the moment
      a resubmission supersedes the retained row).
    - Matches the candidate's `prev_commitment` AND the candidate carries a
      client abandon intent (`abandon_requested_at`, recorded by
      `POST /delta/candidate/abandon`): count the observation toward the
      abandon quarantine instead — this takes precedence over the grace
      deferral. After `abandon_quarantine_checks` consecutive at-base
      observations (default 2) AND `abandon_quarantine_seconds` since the
      request (default 15, so a late-landing transaction can surface),
      delete the matching proposal, transition the delta to
      `discarded` with reason `client_abandoned` (preserved as history),
      and clear the pending-candidate flag. A divergent observation resets
      the abandon-confirmation streak; a landed transaction always wins
      and canonicalizes normally.
    - Matches neither — the account appears to have advanced past the
      candidate's base state: after `divergence_confirmations` consecutive
      such observations (default 2, to tolerate a single stale RPC read),
      bypass the grace period and park the candidate as `retained` with
      reason `diverged` (issue #345), clearing the account's
      pending-candidate flag so new proposals stop returning
      `409 conflict_pending_delta`. Retention (rather than deletion)
      matters because a diverged verdict is an observation, not proof —
      a lagging RPC node can produce one for a transaction that landed.
      With `retained_ttl_seconds = 0` the historical behavior applies:
      delete the delta and its matching proposal.
- Recoverable deltas — `retained` rows, plus
  `discarded { client_abandoned }` rows no older than
  `retained_ttl_seconds` (the abandon quarantine cannot fully rule out a
  late-landing transaction; one that lands after the abandon finalizes
  leaves stored state behind chain, and the preserved row holds
  everything needed to recover) — are swept by a dedicated reconcile
  pass, never by the full pass. It runs every
  `reconcile_interval_seconds` (default 60), visits at most
  `reconcile_page_size` accounts per pass under a rotation cursor
  (a backlog larger than one page drains breadth-first across passes),
  and stops admitting work at the next full-pass tick, so
  reconciliation can never delay ordinary candidate processing. Per
  visited account, in order:
  - Skip the account entirely while it has an in-flight candidate
    (reconciliation never runs under a pending candidate — promoting
    would move the stored base out from under a signed proposal).
  - Drop any retained delta older than `retained_ttl_seconds` before
    any network work. Expired client-abandoned rows are merely dropped
    from the scan — they are preserved history, never deleted.
  - Back off aged rows: for its first 15 minutes a recoverable row is
    reconsidered on every reconcile tick; after that the spacing doubles
    per 15 minutes of age, capped at 10 minutes. The schedule is derived
    purely from the row's age (no persisted cursor), so it survives
    restarts and lease failover and every replica computes the same
    answer.
  - Retry proposal cleanup for retained rows whose matching proposal
    could not be deleted at retain time.
  - Probe the chain once against the stored state commitment. A match
    (or an absent on-chain account) means nothing recoverable can have
    landed — the pass stops there, with no state reconstruction at all.
  - Only when the chain moved past the stored base: select the
    recoverable row whose submission-computed `new_commitment` equals
    the observed on-chain commitment (rows without a stored hint fall
    back to reconstruct-and-compare), reconstruct that path from the
    stored base — reconstruction remains mandatory, the hint alone never
    promotes — and require the recomputed commitment to equal the
    observed one before the same fenced promotion the candidate pass
    uses (auto-recovering an account whose stored state fell behind the
    chain). Recoverable rows may also form a chain (issue #17: a parked
    predecessor followed by its orphaned successors); when the stored
    hints link the stored base to the on-chain commitment through more
    than one row, that path is reconstructed hop by hop — every hop must
    reproduce its row's hint and the last the on-chain commitment — and
    promoted in order, base-first. Anything else waits for a later tick
    — the TTL is the only bound.
  - A new candidate submission at a retained or client-abandoned delta's
    nonce supersedes (deletes) that row inside the submission
    transaction — without the abandoned-row supersede, the resubmission
    the abandon endpoint exists to enable would be refused forever at
    the nonce's unique constraint. Deltas are unique per
    `(account_id, nonce)` and admission requires chaining from the
    account's queue tail (the canonical head when nothing is queued), so
    same-nonce supersede is the only
    replacement path; a retained row orphaned by an out-of-band base
    move (e.g. `configure`) can never promote — the base gate rules it
    out — and ages out through the TTL.

- Release on guardian switch, push path (issue #305): when a delta
  commits (optimistic mode) or canonicalizes (candidate mode) and the
  resulting state's guardian public key commitment differs from this
  server's ack key, the account is released (`released_at` set,
  `accounts.release` audit row with `detected_by: delta`). Switches that
  never reach the push path are covered by the release sweep, a separate
  background task described below.

EVM proposals are not processed by Miden canonicalization. They are stored in the EVM proposal store and deleted lazily when expired or when the configured EntryPoint nonce indicates finality.

#### Canonicalization worker (diagram)
```mermaid
sequenceDiagram
  autonumber
  participant T as Timer
  participant W as Worker
  participant M as Metadata
  participant ST as Storage
  participant N as Network
  T->>W: tick(full interval or fast promotion interval)
  alt full pass
    W->>M: list_with_pending_candidates()
    W->>ST: pull_candidate_deltas(account_id)\n(per account, nonce order)
  else promotion-only pass
    W->>ST: pull_recent_candidate_deltas(cutoff, cursor, page size)\n(oldest first, paginated until deadline)
  end
  loop selected candidates
    alt promotion-only pass
      W->>N: verify_commitment(account_id, stored new_commitment)
      alt claim matches on-chain
        W->>ST: pull_state(account_id)
        W->>N: apply_delta(prev_state, delta)\n(new_state, recomputed_commitment, nonce)
        alt recomputed commitment equals stored claim
          W->>N: should_update_auth(new_state)
          W->>ST: promote_candidate(new_state, canonical delta, new_auth?)\n(lease-fenced write)
        else reconstruction differs
          W->>W: leave candidate for full pass
        end
      else missing, wrong, or not landed
        W->>W: leave candidate for full pass
      end
    else full pass
      W->>ST: pull_state(account_id)
      W->>N: apply_delta(prev_state, delta)\n(new_state, expected_commitment, nonce)
      W->>N: verify_commitment(account_id, expected_commitment)
      alt on-chain matches expected commitment
        W->>N: should_update_auth(new_state)\n(maybe new cosigner keys)
        W->>ST: promote_candidate(new_state, canonical delta, new_auth?)\n(one lease-fenced write: state + delta status + auth + flag)
        ST-->>W: applied | stale_base | not_candidate | stale_lease\n(rejections leave no partial write)
      else on-chain still at prev_commitment (not landed)
        W->>W: defer (grace period), then consume retry budget
      else diverged (matches neither)
        W->>ST: delete_delta + delete matching proposal\nclear pending-candidate flag (after confirmation)
      end
    end
  end
```

### State Machine
- candidate -> canonical | retained | discarded; retained -> canonical
  (reconciled) | superseded (deleted by a new submission at its nonce) |
  dropped (TTL expiry); discarded{client_abandoned} -> canonical
  (late-landing reconcile, within the TTL) | superseded (new submission
  at its nonce). Discarded deltas MUST NOT be returned by default APIs.

### Failure Handling
- Transient failures SHOULD be retried with backoff. Malformed candidates SHOULD be quarantined with logs/metrics.

### Concurrency
- Processing SHOULD be per-account sequential; multi-account processing MAY be parallel with bounded concurrency.
- The server processes accounts with bounded concurrency
  (`max_concurrent_accounts`, default 10); candidates within one account
  remain strictly sequential in nonce order, and every custody write is
  individually lease-fenced, so correctness does not depend on the bound.

## Release sweep

A background task (issue #434), independent of the canonicalization
worker, that recognises guardian switches whose `SwitchGuardian` delta
never reached this server — the offline switch path, a failed
best-effort push, a client predating the push, a switch executed while
this server was unreachable — and releases the account exactly as the
push-path hook does. It also writes a push-path release whose own write
failed. Release detection is not latency-sensitive (an undetected switch
costs stale reads and dead pending proposals, never funds or custody),
so the sweep is deliberately slow and rate-bounded.

### Configuration
- Shipped defaults: `enabled = true`, `rotation_seconds = 21600` (6 h),
  `max_rate_per_second = 5`, `page_size = 100`, `recheck_seconds = 60`,
  `confirmations = 2`. Every value has a `GUARDIAN_RELEASE_SWEEP_*` env
  override (see `docs/CONFIGURATION.md` for the bounds);
  `GUARDIAN_RELEASE_SWEEP_ENABLED=false` is the runtime kill switch.
- One replica holds the `release_sweep` lease (its own single-owner
  lease, renewed every 10 s with a 30 s TTL). A lost lease stops the
  loop; a panic in it stops the renewal too, so the lease expires and
  another replica takes over.

### Behavior
- **One paced loop.** The holder visits one account per turn: the next
  account of the rotation, or a due confirmation re-check. When both are
  due they take turns, and two visits are never closer than
  `1 / max_rate_per_second` whatever their kind, so neither can starve
  the other and together they never exceed the rate.
- **Rotation.** The walk covers every unreleased Miden account with no
  candidate in flight (the store filters both; the push path owns busy
  accounts) in `account_id` order, paced so it spreads over
  `rotation_seconds`: spacing = rotation / fleet size, recounted on every
  page refill, never below the rate floor. The cursor advances per
  visited account, so a slow node or a failed listing never skips
  accounts. A completed walk idles until `rotation_seconds` after it
  started; a fleet too large for one rotation at the rate bound simply
  takes longer. A rotation in which some accounts could not be checked
  (a failed visit, or a chain read deferred to a later visit) ends as
  `partial` rather than `completed`. Each visit reads the account's
  metadata row by id, never from the (possibly hours old) page.
- **Replica-local state.** The cursor, the confirmation streaks, the
  cached candidate post-states and history-search positions live in the
  lease holder's memory. A new holder (failover or restart) starts a
  fresh rotation and fresh streaks, which only delays a release.
- **Per visit**, cheapest first:
  1. Probe the chain once against the stored state commitment (the
     commitment alone is read from storage, without decrypting the
     state). An absent on-chain account has nothing to release.
  2. **The stored state itself**, whenever the account is on chain
     (parsed once per stored commitment). Its guardian key is this
     server's (as `/configure` validated) unless a promoted delta moved
     it and that delta's push-path release was never written, or this
     server's own ack key changed since. When it is not this server's,
     the newest canonical delta decides: if it produced the stored state
     and its ack signature verifies against this server's current key
     (the signature itself is checked, which every storage backend
     keeps), it was that switch, and the account is released with the
     evidence the push path would have written (`detected_by: delta`),
     however far the chain has moved since. Otherwise nothing is
     released: when the chain holds the stored state the account is
     reported as `own_key_mismatch`; when it moved on, the detectors
     below decide.
  3. **Chain moved past the stored base: candidate match.** Every
     pending proposal and every recoverable delta (`retained`, or
     `discarded { client_abandoned }` within `retained_ttl_seconds`)
     that chains from the stored base is applied to the stored state
     (the same `apply_delta` canonicalization uses; computed once per
     candidate and base, failures included). A proposal counts whatever
     its label (the post-state's guardian key decides, not the
     client-written type) and whatever base it was recorded against: the
     candidate queue records a proposal against its tail. One built on
     the stored state behind a queued candidate is refused unless its
     nonce is the tail's plus one, but a summary does not name its base,
     so one labelled exactly so is recorded against the tail all the
     same.
     A switch delta queued behind another candidate and parked with
     it does not chain from the stored base and is not matched yet
     (issue #504). The
     resulting commitment is looked for on chain: first at the head
     (free: the probe already read it), then — for post-states that move
     the guardian key away — in the account's transaction history
     (`SyncTransactions`, from genesis the first time, then resuming
     after the last searched block). Transaction headers are public for
     every account and never pruned, and only the **final** state
     commitment is compared (a new account's first transaction records
     an empty initial commitment). An exact commitment proves that
     candidate executed — a lagging node cannot invent it — so the
     account is released at once with `detected_by: proposal_match` (the
     proposal id) or `recoverable_delta` (the delta nonce) and, when
     found in the history, the block of the switch transaction. Once the
     release is persisted an executed proposal is finalized (deleted)
     like a proposal whose delta canonicalized; a failed write leaves it
     for the next visit. A recoverable delta row is left to the
     reconcile pass and its TTL. This needs no published storage, so it
     covers **private** accounts, including accounts that transacted
     again under the new guardian. If the history cannot be read the
     visit falls through to the storage read, and an account whose
     storage is private is deferred (`probe_failed`) rather than reported
     opaque.
  4. **Storage read**: the guardian public key map from the account's
     **published** on-chain storage (`GetAccount` with storage-map
     details), with the observed state's nonce and the block the node
     answered at. Private accounts publish no storage; for them the sweep
     records that it cannot tell (`storage_opaque`) rather than guessing.
     A storage read at the stored commitment contradicts the probe (one
     of the two reads lagged) and is no observation. A read of a state
     whose nonce is below the stored state's is not the chain moving on
     (the stored state has not landed yet, for example just re-onboarded
     after a switch back, or the node lags) and is never evidence
     (`chain_behind_stored`). A foreign key must be observed
     `confirmations` times, each at a strictly later block: the rotation
     visit is the first, and the account is re-checked every
     `recheck_seconds` until the key is confirmed or the streak closes.
     It is then released with `detected_by: chain_sweep` and the
     `on_chain_commitment` / `stored_commitment` pair.
  - **What counts as a switch.** A guardian key found on chain (or in an
    executed candidate's post-state) is foreign only when it is neither
    this server's current key nor the stored base's. In normal operation
    the two are the same key. A server whose own ack key changed (a new
    ack secret, or the ephemeral keys a non-prod server generates on
    every boot) sees its accounts still bound to the previous key:
    `own_key_mismatch`, a warning, never a release. A key equal to this
    server's is `still_bound` (the stored state lags the chain, issue
    #345 territory); no key at all is `no_binding`.
  - Every release write is conditional on the stored state still being
    the one the evidence was proved against, atomically with that check
    (Postgres: one transaction under the metadata row lock; filesystem:
    under the metadata store's lock). `/configure` clears a release the
    same way after storing a re-onboarded state, for every existing
    account, so a re-onboarding racing a release never ends up released,
    while a release of a later state (a switch delta that replaced the
    re-onboarded one) stands. The stored state itself is left as is;
    reads keep serving the last state this server verified.
  - The sweep ignores `paused_at` like the canonicalization passes:
    pause gates client mutations, the sweep records chain truth.
- The one transaction a guarded multisig executes without this server's
  signature is the guardian key rotation, so "chain moved past the
  stored base" is either that rotation or an acknowledged delta whose
  promotion never caught up; only the detectors above tell them apart,
  and the sweep never infers a switch from a commitment mismatch alone.
