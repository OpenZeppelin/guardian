-- Wallet replay floor compare-and-set (issue #317), as a function so the
-- server calls it as one typed, prepared-once statement. Returns whether the
-- floor moved: a new floor, or one raised above its stored value, unless the
-- account's migrated account-level floor already covers the timestamp.
CREATE OR REPLACE FUNCTION guardian_advance_wallet_floor(
    p_account_id TEXT,
    p_signer     TEXT,
    p_ts         BIGINT,
    p_legacy_key TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    v_updated INTEGER;
BEGIN
    INSERT INTO account_auth_state (account_id, signer_commitment, last_auth_timestamp)
    SELECT p_account_id, p_signer, p_ts
    WHERE NOT EXISTS (
        SELECT 1
          FROM account_auth_state legacy
         WHERE legacy.account_id = p_account_id
           AND legacy.signer_commitment = p_legacy_key
           AND legacy.last_auth_timestamp >= p_ts
    )
    ON CONFLICT (account_id, signer_commitment) DO UPDATE
       SET last_auth_timestamp = EXCLUDED.last_auth_timestamp
     WHERE account_auth_state.last_auth_timestamp < EXCLUDED.last_auth_timestamp;
    GET DIAGNOSTICS v_updated = ROW_COUNT;
    RETURN v_updated > 0;
END;
$$;
