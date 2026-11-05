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
    IF p_with_head THEN
        SELECT st.commitment, st.nonce INTO v_commitment, v_nonce
          FROM states st
         WHERE st.account_id = p_account_id;
        RETURN QUERY SELECT 'served'::TEXT, v_auth_version, v_subject,
            FOUND, v_commitment, v_nonce;
        RETURN;
    END IF;
    RETURN QUERY SELECT 'served'::TEXT, v_auth_version, v_subject,
        FALSE, NULL::TEXT, NULL::BIGINT;
END;
$$;
