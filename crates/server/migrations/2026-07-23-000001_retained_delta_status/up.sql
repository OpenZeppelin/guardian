-- Issue #345: retain-and-reconcile for retry-exhausted candidates.
--
-- Adds the `retained` lifecycle status to the `deltas` CHECK constraint:
-- a candidate whose verification exhausted its retry budget is no longer
-- deleted but kept under `status_kind = 'retained'`, and a bounded
-- background reconcile pass re-checks `stored base + delta` against the
-- on-chain commitment, promoting the row to `canonical` if they ever
-- match (or dropping it once its TTL expires).
--
-- The replacement constraint is added NOT VALID so this migration is a
-- metadata-only switch: a plain ADD CHECK would scan the entire deltas
-- table — dominated by ever-growing canonical history — while holding
-- ACCESS EXCLUSIVE. Existing rows are provably valid anyway (the old
-- constraint enforced a strict subset of the new value set up to this
-- instant); the follow-up migration VALIDATEs in its own transaction,
-- where the scan only takes SHARE UPDATE EXCLUSIVE and never blocks
-- concurrent reads or writes.
--
-- `delta_proposals` keeps its existing constraint: a proposal is moved
-- out of the queue before its delta can ever become retained.
ALTER TABLE deltas
    DROP CONSTRAINT deltas_status_kind_valid;
ALTER TABLE deltas
    ADD CONSTRAINT deltas_status_kind_valid CHECK (
        status_kind IN ('candidate', 'canonical', 'retained', 'discarded')
    ) NOT VALID;
