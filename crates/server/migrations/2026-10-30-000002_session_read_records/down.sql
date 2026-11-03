DROP FUNCTION guardian_drop_fenced_session_read_partitions();
DROP FUNCTION guardian_advance_session_read_fence(BIGINT);
DROP FUNCTION guardian_record_session_read(BYTEA, BIGINT, BIGINT);
DROP FUNCTION guardian_ensure_session_read_partitions(BIGINT, BIGINT);
DROP FUNCTION guardian_db_now_ms();
DROP FUNCTION guardian_session_read_partition_ms();
DROP TABLE session_read_seen;
DROP TABLE replay_fence;
