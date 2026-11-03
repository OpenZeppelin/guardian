-- Block watch (issue #585): the release sweep's lease holder consumes the
-- node's committed-block feed and records, for every guarded account whose
-- on-chain commitment moved to a state this server does not hold, an
-- observation for the release detectors to visit.
--
-- `block_watch_cursor` is the last block fully processed, one row per
-- network. It is written forward-only, under the holder's `release_sweep`
-- lease (holder_id / fence_token record which lease wrote it), so a
-- successor resumes exactly where the previous holder stopped. It records
-- the genesis commitment of the chain it counts blocks of, so a reset
-- chain (devnet) is recognised instead of resumed at a stale height.
CREATE TABLE block_watch_cursor (
    network            TEXT        PRIMARY KEY,
    last_block         BIGINT      NOT NULL,
    genesis_commitment TEXT,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    holder_id          TEXT        NOT NULL,
    fence_token        BIGINT      NOT NULL
);

-- The worklist between the feed and the detectors: one row per account per
-- block it moved in. Deleted once visited; retried with backoff otherwise.
-- No foreign key into the custody tables: an observation is a hint, and a
-- visit re-reads everything it acts on.
CREATE TABLE block_watch_observations (
    account_id          TEXT        NOT NULL,
    block_num           BIGINT      NOT NULL,
    on_chain_commitment TEXT        NOT NULL,
    observed_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts            INTEGER     NOT NULL DEFAULT 0,
    next_attempt_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, block_num)
);

CREATE INDEX block_watch_observations_due_idx
    ON block_watch_observations (next_attempt_at, block_num);

-- Walks of the release sweep rotation that block watch owes the fleet: it
-- skipped blocks (a first start, a gap too large to replay, a block the
-- node no longer serves, a reset chain) and asked for one walk to cover
-- them. One row per network; a walk is owed while `requested_seq` is past
-- `completed_seq`. Each request raises `requested_seq`; a walk records the
-- sequence it started under and, once complete, raises `completed_seq` to
-- it, so a request made during a walk stays owed. Kept here rather than in
-- the holder's memory so a successor, or a restart with the rotation
-- turned off, still runs the walk. Written under the `release_sweep` lease.
CREATE TABLE block_watch_walks (
    network       TEXT        PRIMARY KEY,
    requested_seq BIGINT      NOT NULL,
    completed_seq BIGINT      NOT NULL DEFAULT 0,
    requested_at  TIMESTAMPTZ NOT NULL,
    holder_id     TEXT        NOT NULL,
    fence_token   BIGINT      NOT NULL
);
