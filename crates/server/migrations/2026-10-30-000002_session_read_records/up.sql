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
                'CREATE TABLE session_read_seen_%s PARTITION OF session_read_seen '
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
    SELECT f.watermark_ms INTO watermark FROM replay_fence f WHERE f.fence = 'session_read';
    FOR part IN
        SELECT c.relname
          FROM pg_inherits i
          JOIN pg_class c ON c.oid = i.inhrelid
          JOIN pg_class p ON p.oid = i.inhparent
         WHERE p.relname = 'session_read_seen'
           AND substring(c.relname FROM 'session_read_seen_([0-9]+)$')::BIGINT + width <= watermark + 1
    LOOP
        EXECUTE format('DROP TABLE %I', part.relname);
        dropped := dropped + 1;
    END LOOP;
    RETURN dropped;
END;
$$;
