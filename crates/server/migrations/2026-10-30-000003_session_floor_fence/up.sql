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
SET LOCAL lock_timeout = '5s';

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
