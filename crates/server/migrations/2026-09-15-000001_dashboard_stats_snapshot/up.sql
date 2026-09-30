-- Shared publication store for the /dashboard/stats aggregate (issue #371).
-- One replica (the `dashboard_stats` lease holder) publishes; every replica
-- reads the same version. Only the current snapshot row is retained.
CREATE TABLE dashboard_stats_snapshots (
    version      BIGINT      PRIMARY KEY,
    fence_token  BIGINT      NOT NULL,
    holder_id    TEXT        NOT NULL,
    as_of        TIMESTAMPTZ NOT NULL,
    published_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    payload      JSONB       NOT NULL
);

-- Singleton refresh-control row: operator-triggered refresh requests, the
-- in-progress marker, and the operator-request cooldown clock.
CREATE TABLE dashboard_stats_control (
    id                       BOOLEAN     PRIMARY KEY DEFAULT TRUE CHECK (id),
    last_published_at        TIMESTAMPTZ,
    refresh_requested_at     TIMESTAMPTZ,
    refresh_requested_by     TEXT,
    refresh_started_at       TIMESTAMPTZ,
    refresh_started_by       TEXT,
    last_operator_request_at TIMESTAMPTZ
);

INSERT INTO dashboard_stats_control (id) VALUES (TRUE);
