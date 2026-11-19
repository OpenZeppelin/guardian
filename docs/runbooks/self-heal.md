# Self-Heal Runbook

How to repair accounts after the `/configure` takeover. Guardian 0.18.3 and
earlier let any caller replace an account's stored state and signer set
through `/configure`. An attacker used this in production. The self-heal block
finds the accounts that were changed, puts back the true state where it can
prove it, and stops the rest.

> **Audience:** operators who deploy the Guardian server and can read its logs.

The settings are in
[`CONFIGURATION.md`](../CONFIGURATION.md#startup-self-heal).

## What the block does

The block runs once, at startup. It runs before the background jobs start and
before the HTTP and gRPC listeners bind. Only one replica runs it: the block
first takes the canonicalization lease, and it holds the lease until it is
done.

For each Miden account that is not released, the block:

1. Reads the stored state.
2. Reads the account's commitment from the chain. This is the only chain read
   the block makes for an account.
3. When the two commitments are equal, the account is healthy. The block does
   nothing more with it.
4. When they are not equal, or the chain has no state for the account, the
   block reads every delta row of the account (in every status), every
   archived superseded row, and every proposal. It then rebuilds the account
   from its creation delta and applies the stored rows.
5. When the rebuilt state has the on-chain commitment, the block heals the
   account. It writes the true state and the true signer set, and it deletes
   the rows and the signatures that the true signers did not make.
6. When the block cannot prove the true state, it does not recover the
   account. It deletes the account's pending proposals, pauses the account and
   writes an entry to the recovery file.

The block skips EVM accounts and released accounts.

### The trust rule

The block trusts only the on-chain commitment. It does not trust the stored
state, the stored signer set or the stored rows. A row is part of the true
history only when replaying it leads to the commitment that the chain holds.
When the block cannot reach that commitment, it does not guess. It stops the
account instead.

### When the true state binds another guardian

When the true state names a guardian key that is not one of this server's ack
keys (Falcon or ECDSA), the block heals the account and then releases it.

## Run order

1. **Stop every replica that runs 0.18.3 or earlier.** An old replica still
   accepts the unverified `/configure`. It also holds the canonicalization
   lease, and the block waits until it gets that lease.
2. **Run one replica only.** The canonicalization worker of the replica that
   ran the block keeps the lease after the block ends. Another replica that
   starts with `GUARDIAN_SELF_HEAL=apply` or `dry-run` waits for the lease and
   does not start. Scale to one task for the self-heal runs, and scale up
   again only after you set `GUARDIAN_SELF_HEAL=off`.
3. **Do a dry run.** Set `GUARDIAN_SELF_HEAL=dry-run` and start the server.
   The block plans every account and writes the recovery file. It writes
   nothing to storage. The server then starts as usual.
4. **Read the log and the recovery file.** Each planned account has one
   `self-heal plan` event with `account_id` and `outcome` (`heal`, `skip` or
   `not_recovered`). A healed account also has one `self-heal row` event for
   each row the plan changes or keeps, with `row`, `id`, `verdict` and
   `reason`. The log never holds payloads or signatures. The last event,
   `self-heal finished`, has the totals. Look at every account in the
   recovery file before you go on.
5. **Apply.** Set `GUARDIAN_SELF_HEAL=apply` and start the server. The block
   applies each plan and writes the recovery file again. If a chain read or
   a write fails, the server does not start. It stops with a panic and the
   message `Self-heal failed; the server does not start`. Correct the cause
   (for example, the RPC node) and start again. The block is safe to run
   again: a healed account now matches the chain and is healthy.
6. **Verify.** Start the server once more with `GUARDIAN_SELF_HEAL=dry-run`.
   Every healed account must now be healthy. Only the accounts that were not
   recovered must stay in the recovery file.
7. **Turn the block off.** Set `GUARDIAN_SELF_HEAL=off` and scale up.

Keep the recovery file. On unix only its owner can read it. It holds
commitments, nonces and signer commitments, but no payloads, signatures or
keys.

## The recovery file

The block writes the file in both modes, also when it recovers every account.
It writes a temporary file in the same directory and then renames it, so a
reader never sees a part of a file. The accounts are sorted by account id.

```json
{
  "schema": 1,
  "generated_at": "2026-11-19T10:00:00+00:00",
  "network": "MidenMainnet",
  "dry_run": true,
  "totals": {
    "accounts": 120,
    "healthy": 110,
    "skipped": 3,
    "healed": 5,
    "not_recovered": 2
  },
  "accounts": [
    {
      "account_id": "0x...",
      "reason": "replay_short",
      "detail": "optional text",
      "chain": { "status": "mismatch", "commitment": "0x..." },
      "stored": {
        "commitment": "0x...",
        "nonce": 7,
        "signers": ["0x..."],
        "created_at": "...",
        "updated_at": "..."
      },
      "creation_delta": {
        "status": "canonical",
        "nonce": 0,
        "new_commitment": "0x...",
        "signers": ["0x..."]
      },
      "replay": {
        "base": "creation_delta",
        "rows_applied": 3,
        "last_commitment": "0x...",
        "last_nonce": 3,
        "last_signers": ["0x..."]
      },
      "needed": { "state_with_commitment": "0x..." },
      "delta_rows": [
        {
          "nonce": 4,
          "status": "candidate",
          "prev_commitment": "0x...",
          "new_commitment": "0x...",
          "proposal_type": "update_signers",
          "applied": false
        }
      ],
      "proposals": [
        {
          "id": "0x...",
          "nonce": 4,
          "base": "0x...",
          "proposer": "0x...",
          "proposal_type": "update_signers",
          "verified_signers": ["0x..."]
        }
      ]
    }
  ]
}
```

The fields:

- `dry_run`: `true` when the file comes from a dry run. In a dry run,
  `healed` counts the accounts the block would heal.
- `totals.skipped`: EVM accounts, released accounts, accounts with no stored
  state, and accounts that the planner skips.
- `totals.not_recovered`: the accounts in `accounts`, plus any account whose
  stored state moved between the plan and the write (see the limits below).
- `chain.status`: `mismatch` (the chain holds another commitment), `absent`
  (the chain has no state for the account) or `unreadable` (the chain read
  failed).
- `stored.signers`: the signer commitments of the stored auth. These can be
  the attacker's keys.
- `creation_delta.status`: `none` when the account has no creation delta row,
  or the status of that row. It is `not_read` for `rpc_error`, because the
  block reads no row when the chain read fails.
- `replay`: how far the replay got. `last_signers` was the true signer set at
  `last_commitment`. It is not proven for the on-chain state.
- `needed.state_with_commitment`: the commitment of a full account state that
  would let the replay reach the chain.
- `delta_rows[].applied`: whether the replay applied the row.
- `proposals[].verified_signers`: the signers whose signatures verify over
  the proposal. Whether each one is a true signer is not known.

### Reason codes

| `reason` | Meaning | Paused |
|---|---|---|
| `no_base` | No row can build a base account: the account has no creation delta. | yes |
| `creation_delta_pending` | The chain has no state, the creation delta is not yet canonical, and the stored state is not the state that delta starts from. | yes |
| `replay_short` | The rows end before the on-chain commitment. | yes |
| `left_this_guardian` | The rows show that the account moved to another guardian. | no |
| `ambiguous_history` | Two rows that apply share one base and both lead on, or the rows do not form one chain to the on-chain commitment. | yes |
| `undecodable_row` | A row that the replay needs does not decode. | yes |
| `rpc_error` | The chain read failed. Only a dry run records it; in apply mode the server does not start. | no |

## An account that is not recovered

In apply mode the block does these things, in this order:

1. When the reason pauses the account, it deletes the account's pending
   proposals, so no one can execute a proposal that the attacker's keys
   approved. It keeps the proposals of an account that it does not pause.
2. When the plan says so, it releases the account.
3. When the reason pauses the account, it pauses it with the reason
   `self-heal: <reason>`.

The stored state and the stored signer set stay as they are. To restore the
account, a real signer registers it again with the account's on-chain state:
`registerOnGuardian` in the TypeScript SDK, or `/configure` from another
client. The pause stays after the registration. An operator with the
`accounts:pause` permission then checks the new signer set and unpauses the
account (`POST /dashboard/accounts/{id}/unpause`, see
[`DASHBOARD.md`](../DASHBOARD.md)).

## Known limits

- **A paused account still serves reads.** The keys in its stored auth can
  still read the stored state. When those are the attacker's keys, the
  attacker can read the account until a real signer registers it again.
- **An account that is not deployed is found only through its creation
  delta.** When the chain has no state for an account, the block can detect
  an overwrite only when a creation delta row exists. Without that row, the
  block skips the account.
- **A real delta that was in flight at the takeover can be lost.** When the
  block cannot place a real candidate on the proven path, the plan keeps it
  as a retained row. The canonicalization worker deletes a retained row
  after the retained TTL (`GUARDIAN_CANONICALIZATION_RETAINED_TTL_SECONDS`),
  unless its transaction lands on the chain first.
- **A moved state is not in the recovery file.** When the stored state of an
  account changes between the plan and the write, the block does not write.
  It logs a warning and counts the account in `not_recovered`, but the file
  has no entry for it. Run the block again.
- **A failed apply run leaves no file.** When a chain read or a write fails
  in apply mode, the block stops at once. The accounts it already healed or
  paused stay that way, and no recovery file is written. The next run writes
  the file.
- **A released account drops out of the file.** The block skips released
  accounts. An account it released in one run is not in the file of the next
  run. Keep the file of the apply run.
