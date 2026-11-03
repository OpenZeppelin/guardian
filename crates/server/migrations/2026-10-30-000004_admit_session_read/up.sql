-- One round trip per session read under the `unique` read replay policy
-- (issue #317): the account's metadata version, the live session and the
-- read record together. Nothing is recorded for an unknown account or an
-- unknown, revoked or expired session. The grant's Guardian key, network and
-- cosigner checks stay with the caller, which also keeps the account's
-- metadata cached at the returned version.
--
-- Outcomes: account_not_found, session_unknown, session_revoked,
-- session_expired, and the record outcomes recorded, duplicate,
-- outside_window, stale.
-- With p_with_head, a recorded read also returns the canonical state's
-- commitment and nonce (head_found false when the account has no state), so
-- a nonce poll needs no second call. The head is read only for a recorded
-- read and the caller discards it if a later check refuses the request.
-- A session is live as `PgSessionStore::get` defines it: not revoked and
-- `expires_at > now()`; a revoked session reads as revoked even if expired.
CREATE FUNCTION guardian_admit_session_read(
    p_account_id   TEXT,
    p_realm        TEXT,
    p_token_digest BYTEA,
    p_record_key   BYTEA,
    p_ts           BIGINT,
    p_skew_ms      BIGINT,
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
    v_outcome      TEXT;
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
    IF v_expires_at <= now() THEN
        RETURN QUERY SELECT 'session_expired'::TEXT, v_auth_version, NULL::JSONB,
            FALSE, NULL::TEXT, NULL::BIGINT;
        RETURN;
    END IF;

    v_outcome := guardian_record_session_read(p_record_key, p_ts, p_skew_ms);
    IF p_with_head AND v_outcome = 'recorded' THEN
        SELECT st.commitment, st.nonce INTO v_commitment, v_nonce
          FROM states st
         WHERE st.account_id = p_account_id;
        RETURN QUERY SELECT v_outcome, v_auth_version, v_subject,
            FOUND, v_commitment, v_nonce;
        RETURN;
    END IF;
    RETURN QUERY SELECT v_outcome, v_auth_version, v_subject,
        FALSE, NULL::TEXT, NULL::BIGINT;
END;
$$;
