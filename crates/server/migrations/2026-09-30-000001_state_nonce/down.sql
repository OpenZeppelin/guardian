-- Reverse of 2026-09-30-000001_state_nonce. The nonce is derived data (every
-- value is recomputable from `state_json`), so dropping it loses nothing.

DROP TRIGGER IF EXISTS states_clear_stale_nonce ON states;
DROP FUNCTION IF EXISTS states_clear_stale_nonce();
ALTER TABLE states DROP COLUMN IF EXISTS nonce;
