-- Back to one params version for every module write. Account writes made meanwhile committed
-- as their account's versions, which can be above the instance version, and a write that
-- changed nothing moved its account's version without a history row; the single version
-- continues above every one of them, so no later write reuses a number a delta recorded.
UPDATE module_state_version
   SET version = GREATEST(version,
                          (SELECT COALESCE(MAX(version), 0) FROM module_params_history),
                          (SELECT COALESCE(MAX(version), 0) FROM module_account_version));

DROP INDEX module_params_history_module_idx;
CREATE INDEX module_params_history_module_idx ON module_params_history (module_id, version);

DROP FUNCTION guardian_module_snapshot(TEXT, BIGINT);

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

CREATE OR REPLACE FUNCTION guardian_module_seed_assignment(
    p_account_id TEXT,
    p_modules    JSONB,
    p_operator   TEXT,
    p_reason     TEXT,
    p_changed_at TIMESTAMPTZ
)
RETURNS BIGINT
LANGUAGE plpgsql AS $$
DECLARE
    v_version BIGINT;
BEGIN
    PERFORM 1 FROM module_state_version FOR UPDATE;

    IF EXISTS (SELECT 1 FROM module_account_params
                WHERE account_id = p_account_id AND kind = 'assignment')
       OR EXISTS (SELECT 1 FROM module_params_history
                   WHERE account_id = p_account_id) THEN
        RETURN NULL;
    END IF;

    UPDATE module_state_version SET version = version + 1
    RETURNING version INTO v_version;

    INSERT INTO module_account_params
        (account_id, module_id, kind, position, params, updated_at, updated_by)
    SELECT p_account_id, m.value->>'module_id', 'assignment',
           (m.ordinality - 1)::INTEGER, m.value->'params', p_changed_at, p_operator
      FROM jsonb_array_elements(p_modules) WITH ORDINALITY AS m(value, ordinality);

    INSERT INTO module_params_history
        (version, module_id, account_id, previous_params, new_params, operator, reason,
         changed_at)
    SELECT v_version, m.value->>'module_id', p_account_id, 'null'::jsonb,
           m.value->'params', p_operator, p_reason, p_changed_at
      FROM jsonb_array_elements(p_modules) AS m(value)
     WHERE m.value->'params' <> 'null'::jsonb
     ORDER BY m.value->>'module_id';

    RETURN v_version;
END;
$$;

CREATE INDEX module_params_history_account_idx
    ON module_params_history (account_id)
    WHERE account_id IS NOT NULL;

DROP TABLE module_account_version;
