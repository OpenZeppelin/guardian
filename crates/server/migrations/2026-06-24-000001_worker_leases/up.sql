-- Single-owner coordination for background workers under horizontal scaling
-- (issue #242, subsumes #190). At most one replica holds a named lease at a
-- time; the holder renews on a heartbeat and a stale lease can be reclaimed by
-- another replica once it expires. `fence_token` increments only on a change of
-- holder (steal), so a superseded holder can be detected at its write boundary.

CREATE TABLE worker_leases (
    lease_name  TEXT        PRIMARY KEY,
    holder_id   TEXT        NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL,
    renewed_at  TIMESTAMPTZ NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    fence_token BIGINT      NOT NULL DEFAULT 0
);
