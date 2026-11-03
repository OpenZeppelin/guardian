DROP FUNCTION guardian_delete_fenced_session_floors();
DROP FUNCTION guardian_raise_session_floor_fence(BIGINT);
DROP INDEX account_auth_state_session_floor_ts;
DROP FUNCTION guardian_advance_session_floor(TEXT, TEXT, BIGINT, BIGINT, TEXT);
DELETE FROM replay_fence WHERE fence = 'session_floor';
