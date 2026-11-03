# Account state as a persisted Merkle forest (design note, draft)

Status: draft for discussion, 2026-11-02. Not scheduled. Not urgent today:
devnet and testnet reset regularly, so stored accounts are young. A prototype
(kept outside this repository, not shipped code) confirms the model is exact
and removes the growth; see "Prototype" below.

## Problem

Guardian stores each Miden account as one blob, `{"data": base64(Account::to_bytes())}`
in `states.state_json`, and turns it back into a full `Account` every time it
needs one. That load re-hashes every storage-map entry to rebuild the map's
sparse Merkle tree, because Miden serializes a map as its entries only.

Measured in release on synthetic multisig accounts
(`benchmarks/prod-server/results/2026-11-01-reconstruction-cost-local.md`):

| past transactions | state | load | apply delta | commitment | serialize |
|---:|---:|---:|---:|---:|---:|
| 0 | 3 KB | 0.67 ms | 46 µs | 6 µs | 2 µs |
| 1,000 | 89 KB | 10.6 ms | 46 µs | 6 µs | 20 µs |
| 10,000 | 858 KB | 101 ms | 49 µs | 6 µs | 0.2 ms |
| 50,000 | 4.3 MB | 495 ms | 57 µs | 6 µs | 1 ms |

Loading is 93 to 99.8% of every reconstruction, it grows by about 10 µs per
past transaction (the multisig's executed-transactions map gains an entry per
transaction and is never pruned), and a push pays it twice: once in
`push_delta` and again in canonicalization.

## How the official Miden client does it

`miden-client` 0.17.3 with `miden-client-sqlite-store` keeps account storage as
a persisted forest of sparse Merkle trees, one lineage per storage map and per
vault (`migrations/0001_init.sql`, `src/forest.rs`):

- `forest_trees`: the root (and entry count) per tree version.
- `forest_entries`: the key/value leaves.
- `forest_subtrees`: inner nodes, packed as 8-level subtree blobs.

It plugs that storage into miden-crypto's `LargeSmtForest` through the
`Backend` / `BackendReader` traits. To execute a transaction it builds a
`PartialAccount` holding only map roots (`PartialStorageMap::new(root)`) and
opens a proof per touched key: one leaf plus the subtree blobs on its path,
checked against the stored root. A change reads and rewrites only the touched
paths. Only a brand-new account (nonce 0) is built from all its entries.
Nothing on the normal path scales with map size.

## Proposal

Store every Miden account's storage maps and vault the same way, in Postgres,
and treat the forest as the source of truth for commitments. Keep everything
else about an account (id, nonce, code, value slots, seed) in a small header
row.

### Schema (as prototyped)

- `account_state_header(account_id, head, commitment)`: id, nonce, code,
  seed, vault assets, value-slot values and map roots.
- `account_map_entry(account_id, slot, key, value, position)`: raw map keys
  and values, with the leaf position as a sortable column.
- `account_map_node(account_id, slot, node_id, node)`: inner nodes of the top
  16 levels only. Below that, a subtree holds about `entries / 65,536`
  entries and is re-hashed from them on use. Storing all 64 levels costs
  about 53 rows per entry for no speed gain (measured).
- Production should use small integer ids for account and slot: repeating
  them per row is most of the storage overhead today (about 15 times the
  blob, roughly 3 times with integer ids).

Candidates keep what they keep today (delta payload, `new_commitment`); their
post-state lives only as the forest update that promotion applies.

### Data flow

| Step | Today | With the forest |
|---|---|---|
| Register (`configure`, first state) | store blob | build the trees once from the submitted state (the nonce-0 path; full rebuild, once per account) |
| `push_delta` verify and apply | load full account, apply, hash | read header and roots; apply the delta's patch to the touched paths (proofs verified against stored roots); new commitment from the new roots and header |
| Queued candidates (depth above 1) | replay queued payloads on a full account | apply the queued patches to the touched paths in memory, on top of the canonical forest; nothing is persisted until promotion |
| Canonicalization | load full account again, apply again | compare the stored `new_commitment` with the chain; promote by applying the same path-local update |
| Multisig checks (cosigners, threshold, guardian key, `has_multisig_auth`) | read slots from the full account | read the value slots from the header and the small cosigner map's entries |
| `/state` and SDK sync | serve the blob | assemble the account bytes from header and entries (see below) |
| Dashboard vault and snapshot | load full account | read vault entries directly |
| Guardian execution | load full account | build a `PartialAccount` plus witnesses, exactly like the client's data store |

### Serving the full account

SDKs import the account Guardian serves, so `/state` must keep returning the
exact Miden serialization. A map serializes as its sorted entry list
(`StorageMap::write_into` writes its `BTreeMap` of entries), which Miden's own
`Serializable` impls can write without building a tree. So Guardian can
assemble the bytes from header and entries with Miden's serializers, never
hashing. The account layout around the maps (slot headers, vault, code) must
match `Account::write_into` exactly: a byte-equality test against
`Account::to_bytes` over fixture accounts guards it on every Miden upgrade.
Assembled bytes can also be cached per commitment, so repeated reads of an
unchanged account cost one lookup.

## Prototype

All checks compare against Guardian's current full-account path:

- Path-local tree updates reach the same root and stored nodes as Miden's
  `Smt`; altered or missing stored data is refused.
- Commitment and bytes assembled from parts equal `Account::to_commitment`
  and `Account::to_bytes`, also after a multisig transaction, a value-slot
  change and a vault withdrawal applied path by path.
- Through Postgres, 20 transactions per account stay equal to the full path.

| past txs | today per tx | forest per tx | serve full state |
|---:|---:|---:|---:|
| 0 | 1.5 ms | 3.6 ms | 0.7 ms |
| 10,000 | 121 ms | 11.7 ms | 7.7 ms |
| 50,000 | 651 ms | 10.0 ms | 35.8 ms |

The forest cost is flat (about 0.3 ms of CPU, the rest database round trips).
Young accounts get slightly slower; `/state` needs a per-commitment cache of
the assembled bytes to stay as cheap as today for large accounts.

## Correctness

- **Every read is authenticated.** A proof read from storage is verified
  against the stored root before use; a corrupted or partial row fails the
  check instead of yielding a wrong commitment.
- **The commitment stays the arbiter.** The new commitment is computed from
  roots and header as Miden defines it, compared with the client-claimed
  commitment at push and with the chain at canonicalization, as today.
- **One transaction per update.** Entries, subtrees, roots and header change in
  one database transaction under the existing per-account lock, so readers
  never see a half-applied update.
- **Equivalence check during rollout.** For a while, compute both paths (blob
  and forest) and compare commitments and served bytes; disagreement is an
  alarm and falls back to the blob.

## Migration and rollout

1. Build trees for existing accounts in a background backfill (one full rebuild
   each), while the blob stays authoritative.
2. Shadow mode: every push also updates the forest, and commitments are
   compared.
3. Switch reads to the forest; the blob becomes a cache of the assembled bytes.
4. Drop the blob once a release has run clean.

## Costs and risks

- A real storage model change: a new schema, a `Backend` implementation for
  Postgres (the client's 1,600-line `forest.rs` is the template), and every
  reader above moved off the full `Account`.
- Coupling to miden-crypto's forest API, which is newer than `Smt` and may
  move between releases. The client depending on it is the main reason to
  expect it to last.
- Byte-exact account assembly ties `/state` to Miden's serialization layout;
  the equality test catches drift, but each Miden upgrade must keep it green.
- More rows per account (one per entry and per touched subtree) instead of one
  blob.

## Alternatives considered

- **Store the post-state at push (A).** Removes the canonicalization load,
  costs a full-state write per push. Small change; does not help pushes.
- **Keep loaded accounts in memory (B).** Removes loads on hits only; weaker
  with more tasks.
- **Snapshots plus delta replay.** Does not help: the latest snapshot is
  already stored, and loading any snapshot is the cost.
- **Persist nodes and use `Smt::from_raw_parts`.** Skips the rehash for the
  tree, but miden-protocol has no way to put that tree into a `StorageMap`, so
  it cannot produce a full `Account`.
- **Prune the executed-transactions map in the multisig contract.** Fixes the
  growth at its source; a contract change outside Guardian. Worth raising
  upstream regardless.

## Open questions

1. Does miden-crypto's forest API stay stable across the next Miden release,
   or should Guardian wrap it behind its own trait from day one?
2. Is a byte-exact assembled account acceptable for `/state`, or should the
   API move to header plus entries and let SDKs build the account (a breaking
   change that moves the cost to clients)?
3. Do the release sweep, reconciliation and abandon paths need anything a
   partial account cannot give?
4. What size distribution do production accounts reach? This sets when the
   work pays off.
