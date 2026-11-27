-- Default copies at registration look for an account's history under the module
-- state lock; without this index that lookup scanned the whole history.
CREATE INDEX module_params_history_account_idx
    ON module_params_history (account_id)
    WHERE account_id IS NOT NULL;

-- Everything one module evaluation reads, in one call: the params version, the
-- account's layers and assignments and, unless the caller's cached version is
-- current, the instance rows. STABLE, so every statement below sees the snapshot
-- of the calling statement: the rows belong to the returned version without the
-- caller re-reading the version.
CREATE FUNCTION guardian_module_snapshot(
    p_account_id     TEXT,
    p_cached_version BIGINT
)
RETURNS TABLE (
    version        BIGINT,
    instances      JSONB,
    account_params JSONB
)
LANGUAGE plpgsql STABLE AS $$
DECLARE
    v_version   BIGINT;
    v_instances JSONB;
    v_account   JSONB;
BEGIN
    SELECT s.version INTO v_version FROM module_state_version s;

    IF p_cached_version IS DISTINCT FROM v_version THEN
        SELECT COALESCE(jsonb_agg(jsonb_build_object(
                   'module_id', i.module_id,
                   'enabled', i.enabled,
                   'global_params', i.global_params,
                   'seeded_params', i.seeded_params,
                   'updated_at', i.updated_at,
                   'updated_by', i.updated_by
               ) ORDER BY i.module_id), '[]'::jsonb)
          INTO v_instances
          FROM module_instances i;
    END IF;

    SELECT COALESCE(jsonb_agg(jsonb_build_object(
               'account_id', p.account_id,
               'module_id', p.module_id,
               'kind', p.kind,
               'position', p.position,
               'params', p.params,
               'updated_at', p.updated_at,
               'updated_by', p.updated_by
           ) ORDER BY p.position, p.module_id), '[]'::jsonb)
      INTO v_account
      FROM module_account_params p
     WHERE p.account_id = p_account_id;

    RETURN QUERY SELECT v_version, v_instances, v_account;
END;
$$;
