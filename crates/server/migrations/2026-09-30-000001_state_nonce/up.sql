-- Issue #191: the account nonce of the stored state, next to `commitment`, so
-- GET /state/nonce and gRPC GetCanonicalNonce answer from these two columns
-- instead of loading the row, decrypting `state_json` under storage
-- encryption, and decoding the account.
--
-- NULL means the nonce is not known, never nonce 0. SQL cannot decode a stored
-- account (a serialized, possibly encrypted blob), so existing rows start NULL.
-- The server decodes such a row once, on its first canonical-nonce read, and
-- stores the result only while the row still holds the commitment it decoded.
--
-- The column is plaintext like `commitment`, and it reveals the account's
-- transaction count (see "Storage encryption" in docs/PRODUCTION.md).

-- Fail fast rather than queue every states query behind this ALTER while a
-- long transaction on a replica still running the previous version holds the
-- table.
SET LOCAL lock_timeout = '5s';

-- A nullable column without a default or a constraint is a catalog-only
-- change: no table scan while the ACCESS EXCLUSIVE lock is held. The server
-- never writes a negative value, and reads a negative one as unknown.
ALTER TABLE states ADD COLUMN nonce BIGINT NULL;

-- Replicas still running the previous version keep writing during a rolling
-- deploy, and their writes move `state_json` and `commitment` without touching
-- `nonce`. Left alone, such a row would pair the new commitment with the old
-- state's nonce, and a client whose local nonce is above that stale value would
-- skip a fetch it needs. The trigger clears the nonce whenever an UPDATE changes
-- the commitment but leaves the nonce unchanged. Current writers set the new
-- nonce in the same statement. A current write that keeps the nonce while
-- changing the commitment (re-configuring an account with a different state
-- at the same nonce) is cleared too, and the next read recomputes it.
CREATE FUNCTION states_clear_stale_nonce()
    RETURNS trigger AS $$
BEGIN
    IF NEW.commitment IS DISTINCT FROM OLD.commitment
       AND NEW.nonce IS NOT DISTINCT FROM OLD.nonce THEN
        NEW.nonce := NULL;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER states_clear_stale_nonce
    BEFORE UPDATE ON states
    FOR EACH ROW EXECUTE FUNCTION states_clear_stale_nonce();
