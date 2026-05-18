-- Reverse of 2026-05-16-000001_admin_actions.
-- Rolling back drops the table outright since v1 ships no consumer
-- that reads from it; the audit trail itself is the only thing lost,
-- which is the expected consequence of rolling back the feature.

DROP TRIGGER IF EXISTS admin_actions_no_update ON admin_actions;
DROP FUNCTION IF EXISTS admin_actions_append_only();
DROP INDEX IF EXISTS admin_actions_recent_idx;
DROP INDEX IF EXISTS admin_actions_operator_idx;
DROP TABLE IF EXISTS admin_actions;
