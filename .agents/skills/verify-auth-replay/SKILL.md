---
name: verify-auth-replay
description: Model-check Guardian's signed-request replay protection (issue #367 per-signer floors, session floors, the clock-skew window) with TLA+/TLC. Use when a diff touches update_last_auth_timestamp_cas, purge_session_floors, consume_auth_timestamp, validate_request_timestamp, resolve_account_with, verify_session_request, configure_account's ordering of writes and timestamp consumption, stale_floor_cutoff_ms or replay_floor_key, the session sweep worker, MAX_TIMESTAMP_SKEW or session TTL bounds, or when check.sh reports DRIFT.
---

# Verify auth replay protection

`model/AuthReplay.tla` is a TLA+ model of one account's replay
protection. Every signed request carries a client timestamp. Each wallet
signer has its own replay floor, and so does each session key. A request
is accepted only if its timestamp is strictly above that floor and above
the account's legacy floor (the one migration kept from before #367).
TLC explores every interleaving of these:

- clients signing requests anywhere inside the skew window;
- an attacker resending any request ever signed, at any time;
- two concurrent request handlers (any replica);
- session grant and revoke;
- the session-floor purge;
- the clock advancing.

One model step is one storage statement or one in-memory check between
two `.await`s. The Postgres CAS and purge are each a single statement.
The filesystem twins run under the `auth_state` write lock, so both
backends are one atomic step and the model has no backend switch.

## What is checked

| Property | In plain words | Harm if broken |
|---|---|---|
| `AcceptedAtMostOnce` | A signed request passes the replay check at most once, however often and wherever it is resent. | A captured request is replayed: a push, a proposal signature, or a configure runs twice. |
| `WritesAtMostOnce` | A signed request overwrites stored data at most once, even a resend that the replay check later rejects. Creating an absent account row in the configure claim is not an overwrite: it cannot land over data the account has moved to. | A rejected replay still overwrites state (see F1). |
| `FloorsOnlyRise` | A replay floor never moves back while it exists. The purge may delete a stale session floor outright. | A lowered floor re-opens timestamps that were already used. |

Groups in `check.sh`: `accept` checks `AcceptedAtMostOnce` and
`FloorsOnlyRise`, `writes` checks `WritesAtMostOnce`, and `all` checks
all three in one run.

Cells (parameters live in `check.sh` `cell_params`):

| Cell | Scenario |
|---|---|
| `configure-fresh` | the account is not configured yet; wallets s1 and s2 send configure and push |
| `configure-existing` | the account exists; wallets s1 and s2 send configure and push |
| `migrated` | the account exists with a legacy floor of 1, and a captured pre-migration push by s2 at timestamp 1 |
| `session-bounded` | one session request, with handlers finishing within max TTL plus skew of their skew check |
| `session-unbounded` | the same, with no bound on how long a handler may stall (the code today) |
| `session-mixed` (`--deep`) | a wallet and a session together |

## Does my change touch this?

Yes if the diff changes any function in `mirrored.txt`. The drift check
in `check.sh` hashes each one and lists any that changed. The model also
depends on these orderings, which must stay true in the code:

- An account route validates the timestamp
  (`validate_request_timestamp`), resolves a session if one is used
  (`verify_session_request` → `find`), consumes the timestamp
  (`consume_auth_timestamp`), and only then writes.
- `configure_account` consumes the timestamp before `submit_state` and
  `metadata.set`. A new account is first claimed with `metadata.create`
  (insert only if absent); a request that loses the claim continues as a
  reconfiguration of the account it lost to, replay check first. Before
  the fix for F1, a new account was written first and consumed after.
- The CAS compares against `max(signer floor, legacy floor)` with a
  strict `>`.
- The purge deletes only session floors below
  `now - (max_ttl + 2 * skew)` (`stale_floor_cutoff_ms`).

## Run it

From the repo root:

```bash
.agents/skills/verify-auth-replay/check.sh          # quick tier, about 1 min
.agents/skills/verify-auth-replay/check.sh --deep   # adds session-mixed, about 6 more min
```

A FAIL verdict names the property TLC reports violated
(`FAIL:AcceptedAtMostOnce`; any temporal property is `FAIL:temporal`),
and an expected failure in `expected.txt` must name it too, so a
different violation in a cell that is expected to fail still shows as a
`DIFF`. `--cell` exits 0 on PASS, 1 on FAIL and 2 on ERROR.
`--accept-drift` refuses to record a mirrored function it cannot find
(renamed or removed): fix `mirrored.txt` first. Set `TLC_WORKERS` to cap
TLC's worker threads (default: all cores).

It needs `java` and `python3`. TLC 1.7.4 is downloaded to
`~/.cache/guardian-tla/` and SHA-256 checked.

Each cell prints `ok` or `DIFF` against `expected.txt`. Exit 0 means
every cell matched and nothing drifted, 1 means a cell changed verdict,
2 is a setup error, and 3 means the cells matched but mirrored Rust
changed. The baseline is not all-PASS: `session-unbounded accept` fails
on finding F2 below. F1 is fixed; `configure-fresh writes FAIL` means it
is back.

For a full counterexample:

```bash
.agents/skills/verify-auth-replay/check.sh --cell configure-fresh writes
```

## Reading a result

Translate the printed actions with the map below.

- **Real bug:** every step is something the code can do, in that order.
  Report it in code terms. Don't edit `expected.txt` or the property to
  make it pass.
- **Modeling error:** a step the code cannot take. Fix the action to
  match the code, cite the Rust, and rerun.

A FAIL turning into PASS on `session-unbounded accept` means F2 was
fixed. Confirm the fix,
then update `expected.txt` and the findings below.

## When the change is intended

1. Map the changed Rust to its action and edit the action to match: same
   reads, same writes, same order.
2. A new storage call or `.await` between two existing ones becomes its
   own action.
3. Run `check.sh --deep`. A property that now fails is a finding about
   the change.
4. Once the model matches the code, run `check.sh --accept-drift`.
   Update `mirrored.txt` first if a mirrored function was added or
   renamed.

## Action map

Paths are relative to `crates/server/src/`.

| Model action | Code |
|---|---|
| `CasWins`, `CasStep` | `metadata/postgres.rs` and `metadata/filesystem.rs` `update_last_auth_timestamp_cas`, via `services/mod.rs` `consume_auth_timestamp` |
| `Purge`, `PurgeCutoff` | `builder/handle.rs` `start_session_sweep_worker` → `session.rs` `stale_floor_cutoff_ms` → `purge_session_floors` |
| `Grant`, `Revoke`, `SessionLive` | `session.rs` session store (`find` treats expired or revoked as not live) |
| `Sign` | a client signing an `AuthRequestMessage` with its own clock |
| `PMeta`, `PSkew`, `PFind`, `PCas` | `services/mod.rs` `resolve_account_with`: `metadata.get`, `validate_request_timestamp`, `verify_session_request`, `consume_auth_timestamp` |
| `PWrite` | the route's write after `resolve_account` (for example `push_delta`) |
| `CSkew`, `CExisting`, `CClaim`, `CCasFirst`, `CState`, `CMeta` | `services/configure_account.rs` `configure_account`: `validate_request_timestamp`, `metadata.get`, `metadata.create` (`metadata/postgres.rs` / `metadata/filesystem.rs` `create`), CAS, `submit_state`, `metadata.set` (existing account or lost claim) |
| `Tick` with `MaxStall` | wall-clock time; `MaxStall` bounds how long a handler sits between its skew check and its last step |

## Scope and assumptions

- One account; two concurrent handlers; at most 2 signed requests (1 in
  the session cells); clock bounded at 4 to 6 units with skew 1 and
  session TTL 2. A pass covers these bounds.
- All handlers read one clock. Replica clock drift is not modeled. The
  purge margin (`2 * skew`) tolerates drift up to roughly TTL plus skew.
- Signatures are assumed unforgeable. The model is about resending
  genuine requests, not forging them.
- Proposal signing, EVM sessions, `revoke_session`, and
  `revoke_all_sessions` are out of scope. The two revoke routes
  deliberately consume no floor and are idempotent within the skew
  window.
- Storage failures are not modeled. A first configuration that claims the
  account and then fails before storing its state leaves metadata with no
  state; `configure_account` treats a retry of that account as finishing
  the first configuration (new-account scheme policy, counted as a
  creation), and filesystem `create` rolls its in-memory insert back when
  the write fails. Neither changes the claim, consume, write order the
  model checks.
- Account deletion does not exist in the code. If it is added,
  `ON DELETE CASCADE` drops the account's floors and this model needs
  that action.

## Findings in the baseline

**F1 (fixed). A replayed first-time configure wrote before it was
rejected.** For a new account, `configure_account` used to run
`submit_state` and `metadata.set` before it consumed the timestamp,
because the replay-state row has a foreign key to `account_metadata`.
Counterexample before the fix:

1. The client's first `/configure` (handler A) reads `existing = None`.
2. A resend of the same request (handler B: an attacker's replay, or the
   client's own retry) also reads `existing = None` before A's
   `metadata.set`.
3. A writes the state and metadata and consumes the timestamp.
4. The client pushes a delta, and the stored state advances.
5. B runs `submit_state` with the request's `initial_state`, so the
   account state goes back to its initial state.
6. B's consume fails with `authentication_replay`, but the write has
   already happened.

The fix claims the account first: `metadata.create` inserts the row only
if none exists, so of two concurrent first configurations exactly one
creates it. The winner then consumes its timestamp before `submit_state`;
the loser re-reads the account and takes the reconfiguration path, which
consumes first, so a duplicate is refused before it writes. Covered by
`duplicate_first_configuration_is_refused_before_writes` and
`first_configuration_claims_then_consumes_before_storing_state` in
`services/configure_account.rs`, and by the `create_inserts_only_when_the_account_is_absent`
tests of both metadata stores. The old flow still fails
`WritesAtMostOnce` under the current property.

**F2. A handler stalled past max TTL plus skew outlives its session
floor** (`session-unbounded accept FAIL`). The skew check runs once,
early in `resolve_account_with`. The purge deletes a session floor once
it is older than `max_ttl + 2 * skew`. Counterexample:

1. A session request is accepted, which sets the session floor.
2. A resend of it passes the skew check and the session lookup, then
   stalls before its CAS.
3. Time passes beyond TTL plus skew, and the purge deletes the floor.
4. The resend's CAS sees no floor and is accepted a second time.

Nothing in the server bounds that stall: there is no request timeout, and
the Postgres pool waits for a connection without a deadline. The window
is at least TTL plus skew (about 10 minutes at the minimum TTL, about 8
hours at the default), so this is theoretical. `session-bounded` shows the
design is safe once a handler finishes within that bound; the boundary is
exact (a stall bound of 3 units passes and 4 fails). Re-checking the skew
at the CAS, or a request deadline shorter than TTL plus skew, would close
it.
