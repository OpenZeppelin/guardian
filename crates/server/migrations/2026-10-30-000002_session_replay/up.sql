-- Session replay protection for session-v2 requests (issue #317), in one
-- migration: the record of accepted session reads with its fence and
-- partitions, the fenced session write floors, the one-call admission of a
-- recorded session read, and the read-only serving call of the session
-- canonical nonce poll.

-- Record of accepted session reads (issue #317). Under the `unique` read
-- replay policy each signed session read is admitted at most once, fleet-wide,
-- instead of advancing a per-key timestamp floor, so a client's parallel or
-- reordered reads are all accepted while a copy of any one of them is refused.
--
-- Two independent checks refuse a delayed request:
--   freshness: the timestamp is re-checked against the database clock after
--              the shared fence lock is held, whether or not a purge ran;
--   fence:     a purge raises the watermark under the exclusive lock before
--              dropping anything, and admission refuses `ts <= watermark`, so
--              a purged request can never be admitted again.
-- Advisory lock keys: 7310317001 read fence, 7310317003 partition maintenance.
SET LOCAL lock_timeout = '5s';

CREATE TABLE replay_fence (
    fence        TEXT   PRIMARY KEY,
    watermark_ms BIGINT NOT NULL
);

INSERT INTO replay_fence (fence, watermark_ms) VALUES ('session_read', 0);

-- `record_key` is SHA-256(session public key || signed envelope). The
-- timestamp is inside the envelope, so a copy of a request always targets the
-- same partition and conflicts on the primary key.
CREATE TABLE session_read_seen (
    record_key    BYTEA  NOT NULL,
    request_ts_ms BIGINT NOT NULL,
    PRIMARY KEY (record_key, request_ts_ms)
) PARTITION BY RANGE (request_ts_ms);

CREATE FUNCTION guardian_session_read_partition_ms() RETURNS BIGINT
LANGUAGE sql IMMUTABLE AS $$ SELECT 300000::BIGINT $$;

CREATE FUNCTION guardian_db_now_ms() RETURNS BIGINT
LANGUAGE sql VOLATILE AS $$
    SELECT (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::BIGINT
$$;

-- Creates every partition overlapping [now - behind_ms, now + ahead_ms) that
-- does not exist yet. Safe to call from every replica; takes no lock when
-- nothing is missing, and gives up after 2 s rather than holding up reads on
-- a busy parent table.
CREATE FUNCTION guardian_ensure_session_read_partitions(behind_ms BIGINT, ahead_ms BIGINT)
RETURNS INTEGER
LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    width   BIGINT := guardian_session_read_partition_ms();
    now_ms  BIGINT := guardian_db_now_ms();
    first   BIGINT := ((now_ms - behind_ms) / width) * width;
    start   BIGINT;
    created INTEGER := 0;
BEGIN
    start := first;
    WHILE start < now_ms + ahead_ms LOOP
        EXIT WHEN to_regclass(format('session_read_seen_%s', start)) IS NULL;
        start := start + width;
    END LOOP;
    IF start >= now_ms + ahead_ms THEN
        RETURN 0;
    END IF;

    PERFORM set_config('lock_timeout', '2s', true);
    PERFORM pg_advisory_xact_lock(7310317003);
    start := first;
    WHILE start < now_ms + ahead_ms LOOP
        IF to_regclass(format('session_read_seen_%s', start)) IS NULL THEN
            EXECUTE format(
                'CREATE TABLE IF NOT EXISTS session_read_seen_%s PARTITION OF session_read_seen '
                'FOR VALUES FROM (%s) TO (%s)',
                start, start, start + width
            );
            created := created + 1;
        END IF;
        start := start + width;
    END LOOP;
    RETURN created;
END;
$$;

-- Admits one signed session read: `recorded` the first time, `duplicate` for
-- a copy, `outside_window` when the timestamp is outside the window by the
-- database clock, `stale` when it is at or below the fence. A missing
-- partition or fence row is an error, so the read fails closed.
CREATE FUNCTION guardian_record_session_read(p_record_key BYTEA, p_ts BIGINT, p_skew_ms BIGINT)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    v_watermark BIGINT;
    v_inserted  INTEGER;
BEGIN
    PERFORM pg_advisory_xact_lock_shared(7310317001);
    IF abs(guardian_db_now_ms() - p_ts) > p_skew_ms THEN
        RETURN 'outside_window';
    END IF;
    SELECT f.watermark_ms INTO STRICT v_watermark FROM replay_fence f WHERE f.fence = 'session_read';
    IF p_ts <= v_watermark THEN
        RETURN 'stale';
    END IF;
    INSERT INTO session_read_seen (record_key, request_ts_ms)
    VALUES (p_record_key, p_ts)
    ON CONFLICT DO NOTHING;
    GET DIAGNOSTICS v_inserted = ROW_COUNT;
    RETURN CASE WHEN v_inserted = 1 THEN 'recorded' ELSE 'duplicate' END;
END;
$$;

-- Raises the read watermark to just below the last partition boundary at or
-- below `now - retain_ms`, and returns it. Runs as its own transaction before
-- partitions below it are dropped: once it commits every admission refuses
-- them, so dropping does not need the lock. Returns without locking when the
-- watermark would not move, which is all but once per partition width.
CREATE FUNCTION guardian_advance_session_read_fence(retain_ms BIGINT)
RETURNS BIGINT
LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    width     BIGINT := guardian_session_read_partition_ms();
    target    BIGINT := ((guardian_db_now_ms() - retain_ms) / width) * width - 1;
    watermark BIGINT;
BEGIN
    SELECT f.watermark_ms INTO watermark FROM replay_fence f WHERE f.fence = 'session_read';
    IF target <= watermark THEN
        RETURN watermark;
    END IF;
    PERFORM set_config('lock_timeout', '2s', true);
    PERFORM pg_advisory_xact_lock(7310317001);
    UPDATE replay_fence
       SET watermark_ms = GREATEST(watermark_ms, target)
     WHERE fence = 'session_read'
    RETURNING watermark_ms INTO watermark;
    RETURN watermark;
END;
$$;

-- Drops every partition wholly at or below the committed read watermark.
-- Returns how many were dropped.
CREATE FUNCTION guardian_drop_fenced_session_read_partitions()
RETURNS INTEGER
LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    width     BIGINT := guardian_session_read_partition_ms();
    watermark BIGINT;
    dropped   INTEGER := 0;
    part      RECORD;
BEGIN
    PERFORM set_config('lock_timeout', '2s', true);
    PERFORM pg_advisory_xact_lock(7310317003);
    SELECT f.watermark_ms INTO watermark FROM replay_fence f WHERE f.fence = 'session_read';
    FOR part IN
        SELECT c.oid::regclass AS partition
          FROM pg_inherits i
          JOIN pg_class c ON c.oid = i.inhrelid
         WHERE i.inhparent = 'session_read_seen'::regclass
           AND substring(c.relname FROM 'session_read_seen_([0-9]+)$')::BIGINT + width <= watermark + 1
    LOOP
        EXECUTE format('DROP TABLE IF EXISTS %s', part.partition);
        dropped := dropped + 1;
    END LOOP;
    RETURN dropped;
END;
$$;

-- ------------------------------------------------------------------------

-- Fenced session write floors (issue #317). A session floor is deleted once no
-- session can use it any more, and a handler can stall between its timestamp
-- check and the floor update for longer than that. Two checks keep such a
-- request from being admitted twice:
--   freshness: the timestamp is re-checked against the database clock after
--              the shared fence lock is held;
--   fence:     the purge raises a watermark under the exclusive lock before
--              deleting floors, and the update refuses `ts <= watermark`.
-- Advisory lock key: 7310317002 session floor fence. Wallet floors are never
-- purged and keep the plain compare-and-set.
INSERT INTO replay_fence (fence, watermark_ms) VALUES ('session_floor', 0);

-- `advanced`, `replay` (not above the floor), `outside_window` (outside the
-- window by the database clock), or `stale` (at or below the fence). A missing
-- fence row is an error, so the write fails closed. A migrated account-level
-- floor is consulted as in the plain compare-and-set.
CREATE FUNCTION guardian_advance_session_floor(
    p_account_id TEXT,
    p_floor_key  TEXT,
    p_ts         BIGINT,
    p_skew_ms    BIGINT,
    p_legacy_key TEXT
)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    v_watermark BIGINT;
    v_updated   INTEGER;
BEGIN
    PERFORM pg_advisory_xact_lock_shared(7310317002);
    IF abs(guardian_db_now_ms() - p_ts) > p_skew_ms THEN
        RETURN 'outside_window';
    END IF;
    SELECT f.watermark_ms INTO STRICT v_watermark FROM replay_fence f WHERE f.fence = 'session_floor';
    IF p_ts <= v_watermark THEN
        RETURN 'stale';
    END IF;

    INSERT INTO account_auth_state (account_id, signer_commitment, last_auth_timestamp)
    SELECT p_account_id, p_floor_key, p_ts
    WHERE NOT EXISTS (
        SELECT 1
          FROM account_auth_state legacy
         WHERE legacy.account_id = p_account_id
           AND legacy.signer_commitment = p_legacy_key
           AND legacy.last_auth_timestamp >= p_ts
    )
    ON CONFLICT (account_id, signer_commitment) DO UPDATE
       SET last_auth_timestamp = EXCLUDED.last_auth_timestamp
     WHERE account_auth_state.last_auth_timestamp < EXCLUDED.last_auth_timestamp;
    GET DIAGNOSTICS v_updated = ROW_COUNT;
    RETURN CASE WHEN v_updated = 1 THEN 'advanced' ELSE 'replay' END;
END;
$$;

-- Old session floors are found by this index, not a scan of every signer's
-- floor.
CREATE INDEX account_auth_state_session_floor_ts
    ON account_auth_state (last_auth_timestamp)
    WHERE signer_commitment LIKE 'session-%';

-- Raises the floor watermark to just below `now - max_age_ms` by the
-- database clock, so no replica's own clock can move it, and returns it. Runs
-- as its own transaction before the floors below it are deleted: once it
-- commits every update refuses them, so the delete does not need the lock,
-- which is held only for this one-row update. Returns without locking when
-- the watermark would not move.
CREATE FUNCTION guardian_raise_session_floor_fence(p_max_age_ms BIGINT)
RETURNS BIGINT
LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    target    BIGINT := guardian_db_now_ms() - p_max_age_ms - 1;
    watermark BIGINT;
BEGIN
    SELECT f.watermark_ms INTO watermark FROM replay_fence f WHERE f.fence = 'session_floor';
    IF target <= watermark THEN
        RETURN watermark;
    END IF;
    PERFORM set_config('lock_timeout', '2s', true);
    PERFORM pg_advisory_xact_lock(7310317002);
    UPDATE replay_fence
       SET watermark_ms = GREATEST(watermark_ms, target)
     WHERE fence = 'session_floor'
    RETURNING watermark_ms INTO watermark;
    RETURN watermark;
END;
$$;

-- Deletes the session floors at or below the committed watermark. The prefix
-- is spelled as in the index predicate so the index serves the delete; it is
-- `session::SESSION_FLOOR_PREFIX`. Returns how many were deleted.
CREATE FUNCTION guardian_delete_fenced_session_floors()
RETURNS BIGINT
LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    watermark BIGINT;
    deleted   BIGINT;
BEGIN
    PERFORM set_config('lock_timeout', '2s', true);
    SELECT f.watermark_ms INTO watermark FROM replay_fence f WHERE f.fence = 'session_floor';
    DELETE FROM account_auth_state
     WHERE signer_commitment LIKE 'session-%'
       AND last_auth_timestamp <= watermark;
    GET DIAGNOSTICS deleted = ROW_COUNT;
    RETURN deleted;
END;
$$;

-- ------------------------------------------------------------------------

-- One round trip per session read under the `unique` read replay policy
-- (issue #317): the account's metadata version, the live session and the
-- read record together. Nothing is recorded for an unknown account or an
-- unknown, revoked or expired session. The grant's Guardian key, network and
-- cosigner checks stay with the caller, which also keeps the account's
-- metadata cached at the returned version.
--
-- Outcomes: account_not_found, session_unknown, session_revoked,
-- session_expired, and the record outcomes recorded, duplicate,
-- outside_window, stale.
-- With p_with_head, a recorded read also returns the canonical state's
-- commitment and nonce (head_found false when the account has no state), so
-- a nonce poll needs no second call. The head is read only for a recorded
-- read and the caller discards it if a later check refuses the request.
-- A session is live as `PgSessionStore::get` defines it: not revoked and
-- not expired; a revoked session reads as revoked even if expired. Expiry is
-- compared with the current clock, not the transaction start. The record and
-- the state head read may wait (fence lock, partition maintenance, a lock on
-- `states`), so a recorded read reads the head and then re-reads the account's
-- metadata version and the session. The version returned is the one read
-- after every wait, so the caller checks the grant against the signer set
-- current at that point, not the one before the wait. If the session was
-- revoked or expired meanwhile, the record is removed in the same
-- transaction and the read is refused, so a refused read leaves nothing
-- recorded.
CREATE FUNCTION guardian_admit_session_read(
    p_account_id   TEXT,
    p_realm        TEXT,
    p_token_digest BYTEA,
    p_record_key   BYTEA,
    p_ts           BIGINT,
    p_skew_ms      BIGINT,
    p_with_head    BOOLEAN
)
RETURNS TABLE (
    outcome         TEXT,
    auth_version    BIGINT,
    subject         JSONB,
    head_found      BOOLEAN,
    head_commitment TEXT,
    head_nonce      BIGINT
)
LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    v_auth_version BIGINT;
    v_subject      JSONB;
    v_expires_at   TIMESTAMPTZ;
    v_revoked_at   TIMESTAMPTZ;
    v_outcome      TEXT;
    v_refusal      TEXT;
    v_head_found   BOOLEAN := FALSE;
    v_commitment   TEXT;
    v_nonce        BIGINT;
BEGIN
    SELECT m.auth_version INTO v_auth_version
      FROM account_metadata m
     WHERE m.account_id = p_account_id;
    IF NOT FOUND THEN
        RETURN QUERY SELECT 'account_not_found'::TEXT, NULL::BIGINT, NULL::JSONB,
            FALSE, NULL::TEXT, NULL::BIGINT;
        RETURN;
    END IF;

    SELECT s.subject, s.expires_at, s.revoked_at
      INTO v_subject, v_expires_at, v_revoked_at
      FROM auth_sessions s
     WHERE s.realm = p_realm AND s.token_digest = p_token_digest;
    IF NOT FOUND THEN
        RETURN QUERY SELECT 'session_unknown'::TEXT, v_auth_version, NULL::JSONB,
            FALSE, NULL::TEXT, NULL::BIGINT;
        RETURN;
    END IF;
    IF v_revoked_at IS NOT NULL THEN
        RETURN QUERY SELECT 'session_revoked'::TEXT, v_auth_version, NULL::JSONB,
            FALSE, NULL::TEXT, NULL::BIGINT;
        RETURN;
    END IF;
    IF v_expires_at <= clock_timestamp() THEN
        RETURN QUERY SELECT 'session_expired'::TEXT, v_auth_version, NULL::JSONB,
            FALSE, NULL::TEXT, NULL::BIGINT;
        RETURN;
    END IF;

    v_outcome := guardian_record_session_read(p_record_key, p_ts, p_skew_ms);
    IF v_outcome = 'recorded' THEN
        IF p_with_head THEN
            SELECT st.commitment, st.nonce INTO v_commitment, v_nonce
              FROM states st
             WHERE st.account_id = p_account_id;
            v_head_found := FOUND;
        END IF;
        SELECT m.auth_version INTO v_auth_version
          FROM account_metadata m
         WHERE m.account_id = p_account_id;
        IF NOT FOUND THEN
            v_refusal := 'account_not_found';
        ELSE
            SELECT s.expires_at, s.revoked_at
              INTO v_expires_at, v_revoked_at
              FROM auth_sessions s
             WHERE s.realm = p_realm AND s.token_digest = p_token_digest;
            v_refusal := CASE
                WHEN NOT FOUND THEN 'session_unknown'
                WHEN v_revoked_at IS NOT NULL THEN 'session_revoked'
                WHEN v_expires_at <= clock_timestamp() THEN 'session_expired'
            END;
        END IF;
        IF v_refusal IS NOT NULL THEN
            DELETE FROM session_read_seen
             WHERE record_key = p_record_key AND request_ts_ms = p_ts;
            RETURN QUERY SELECT v_refusal, v_auth_version, NULL::JSONB,
                FALSE, NULL::TEXT, NULL::BIGINT;
            RETURN;
        END IF;
    END IF;
    RETURN QUERY SELECT v_outcome, v_auth_version, v_subject,
        v_head_found, v_commitment, v_nonce;
END;
$$;

-- ------------------------------------------------------------------------

-- One round trip for a session read accepted every time it is presented
-- inside its window (the canonical nonce poll): the account's metadata
-- version, the live session and, with p_with_head, the canonical state head.
-- Nothing is recorded, so it takes no fence lock and writes nothing.
--
-- Outcomes: account_not_found, session_unknown, session_revoked,
-- session_expired, served, outside_window. The window is checked against the
-- database clock, p_past_ms behind it and p_future_ms ahead. A session is live
-- as `guardian_admit_session_read` defines it; expiry and the window are
-- checked against one reading of the current clock, not the transaction start.
-- The state head is read first, then the account and the session, then the
-- clock: a wait on any of these reads (a lock on `states`, `account_metadata`
-- or `auth_sessions`) ends before the session row is read and the clock is
-- read, so it never falls between the checks and the answer.
CREATE FUNCTION guardian_serve_session_read(
    p_account_id   TEXT,
    p_realm        TEXT,
    p_token_digest BYTEA,
    p_ts           BIGINT,
    p_past_ms      BIGINT,
    p_future_ms    BIGINT,
    p_with_head    BOOLEAN
)
RETURNS TABLE (
    outcome         TEXT,
    auth_version    BIGINT,
    subject         JSONB,
    head_found      BOOLEAN,
    head_commitment TEXT,
    head_nonce      BIGINT
)
LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    v_auth_version BIGINT;
    v_subject      JSONB;
    v_expires_at   TIMESTAMPTZ;
    v_revoked_at   TIMESTAMPTZ;
    v_now_ms       BIGINT;
    v_commitment   TEXT;
    v_nonce        BIGINT;
    v_head_found   BOOLEAN := FALSE;
BEGIN
    IF p_with_head THEN
        SELECT st.commitment, st.nonce INTO v_commitment, v_nonce
          FROM states st
         WHERE st.account_id = p_account_id;
        v_head_found := FOUND;
    END IF;

    SELECT m.auth_version INTO v_auth_version
      FROM account_metadata m
     WHERE m.account_id = p_account_id;
    IF NOT FOUND THEN
        RETURN QUERY SELECT 'account_not_found'::TEXT, NULL::BIGINT, NULL::JSONB,
            FALSE, NULL::TEXT, NULL::BIGINT;
        RETURN;
    END IF;

    SELECT s.subject, s.expires_at, s.revoked_at
      INTO v_subject, v_expires_at, v_revoked_at
      FROM auth_sessions s
     WHERE s.realm = p_realm AND s.token_digest = p_token_digest;
    IF NOT FOUND THEN
        RETURN QUERY SELECT 'session_unknown'::TEXT, v_auth_version, NULL::JSONB,
            FALSE, NULL::TEXT, NULL::BIGINT;
        RETURN;
    END IF;
    IF v_revoked_at IS NOT NULL THEN
        RETURN QUERY SELECT 'session_revoked'::TEXT, v_auth_version, NULL::JSONB,
            FALSE, NULL::TEXT, NULL::BIGINT;
        RETURN;
    END IF;
    v_now_ms := guardian_db_now_ms();
    IF v_expires_at <= to_timestamp(v_now_ms / 1000.0) THEN
        RETURN QUERY SELECT 'session_expired'::TEXT, v_auth_version, NULL::JSONB,
            FALSE, NULL::TEXT, NULL::BIGINT;
        RETURN;
    END IF;

    IF v_now_ms - p_ts > p_past_ms OR p_ts - v_now_ms > p_future_ms THEN
        RETURN QUERY SELECT 'outside_window'::TEXT, v_auth_version, v_subject,
            FALSE, NULL::TEXT, NULL::BIGINT;
        RETURN;
    END IF;
    RETURN QUERY SELECT 'served'::TEXT, v_auth_version, v_subject,
        v_head_found, v_commitment, v_nonce;
END;
$$;
