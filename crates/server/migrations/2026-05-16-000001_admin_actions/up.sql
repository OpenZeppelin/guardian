-- Feature 006-operator-authz: append-only forensic audit table.
-- Spec: speckit/features/006-operator-authz/data-model.md §New table
-- Plan-phase decision: enforcement is DB-trigger (research.md Decision 2)
-- so a future refactor of the application-side `Auditor` trait cannot
-- silently re-introduce UPDATE/DELETE on already-persisted rows. Any
-- legitimate retention work in a follow-up MUST drop this trigger in
-- its own migration so the audit-trail-of-the-audit-trail remains
-- visible.

CREATE TABLE admin_actions (
    id                BIGSERIAL PRIMARY KEY,
    occurred_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    operator_identity TEXT        NOT NULL,
    action_kind       TEXT        NOT NULL,
    target_account_id TEXT        NULL,
    payload           JSONB       NOT NULL DEFAULT '{}'::jsonb,
    outcome           TEXT        NOT NULL CHECK (outcome IN ('success', 'denied')),
    error_code        TEXT        NULL,
    -- Originating client IP; NULL when no request context (synthetic callers).
    client_ip         TEXT        NULL
);

-- Primary forensic lookup: "what did operator X do".
CREATE INDEX admin_actions_operator_idx
    ON admin_actions (operator_identity, occurred_at DESC);

-- Cross-operator recent activity scan, e.g. "all admin actions in the
-- last hour" during incident response.
CREATE INDEX admin_actions_recent_idx
    ON admin_actions (occurred_at DESC);

-- Append-only enforcement. UPDATE/DELETE through the running server
-- raise an exception (research.md Decision 2). Out-of-band DB
-- superuser access remains out of scope; the trigger fires on row
-- triggers, not statement triggers, so even multi-row UPDATEs are
-- blocked per-row before any rows are modified.
CREATE OR REPLACE FUNCTION admin_actions_append_only()
    RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'admin_actions is append-only';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER admin_actions_no_update
    BEFORE UPDATE OR DELETE ON admin_actions
    FOR EACH ROW EXECUTE FUNCTION admin_actions_append_only();
