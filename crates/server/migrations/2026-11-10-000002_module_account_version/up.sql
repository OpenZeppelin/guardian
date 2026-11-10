-- Per-account params versions. module_state_version now moves only on writes to the
-- instance rows, so replicas rebuild their instance cache only when an instance changes.
-- A write to one account's layers or assignment, including the default copy at
-- registration, bumps that account's row here instead: it serializes with writes to the
-- same account only, and every other account and every replica's cache is untouched.
-- A signed delta records both versions.
CREATE TABLE module_account_version (
    account_id TEXT   PRIMARY KEY,
    version    BIGINT NOT NULL
);

-- History rows keep the version they were written with. An account's earlier rows carry
-- global numbers, increasing in that account's write order, so its next version continues
-- above the highest of them.
INSERT INTO module_account_version (account_id, version)
SELECT account_id, MAX(version)
  FROM module_params_history
 WHERE account_id IS NOT NULL
 GROUP BY account_id;

DROP FUNCTION guardian_module_snapshot(TEXT, BIGINT);

-- Everything one module evaluation reads, in one call: the instance params version, the
-- account's params version and rows and, unless the caller's cached instance version is
-- current, the instance rows. STABLE, so every statement below sees the snapshot of the
-- calling statement: the rows belong to the returned versions.
CREATE FUNCTION guardian_module_snapshot(
    p_account_id     TEXT,
    p_cached_version BIGINT
)
RETURNS TABLE (
    version         BIGINT,
    account_version BIGINT,
    instances       JSONB,
    account_params  JSONB
)
LANGUAGE plpgsql STABLE AS $$
DECLARE
    v_version   BIGINT;
    v_account_version BIGINT;
    v_instances JSONB;
    v_account   JSONB;
BEGIN
    SELECT s.version INTO v_version FROM module_state_version s;

    SELECT COALESCE(MAX(a.version), 0) INTO v_account_version
      FROM module_account_version a
     WHERE a.account_id = p_account_id;

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

    RETURN QUERY SELECT v_version, v_account_version, v_instances, v_account;
END;
$$;

-- The default modules a first registration copies into its account, in one call. It locks
-- only this account's version row, so registrations of different accounts and operator
-- writes to other accounts or to instances never wait for it.
--
-- p_modules is the assignment in evaluation order:
-- [{"module_id": "...", "params": {...}}, ...]. Writes only into an account no write has
-- touched yet (version 0, no assignment), so an operator's earlier write always wins.
-- Returns the account version the copy committed as, or NULL when it wrote nothing (the
-- version does not move then).
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
    INSERT INTO module_account_version (account_id, version)
    VALUES (p_account_id, 0)
    ON CONFLICT (account_id) DO NOTHING;

    SELECT a.version INTO v_version
      FROM module_account_version a
     WHERE a.account_id = p_account_id
       FOR UPDATE;

    IF v_version <> 0
       OR EXISTS (SELECT 1 FROM module_account_params
                   WHERE account_id = p_account_id AND kind = 'assignment') THEN
        RETURN NULL;
    END IF;

    v_version := 1;
    UPDATE module_account_version SET version = v_version WHERE account_id = p_account_id;

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

-- The account history lookup the default copy made is gone; the version row answers it.
DROP INDEX IF EXISTS module_params_history_account_idx;

-- A module's history is listed newest first by row id, since the versions of its global and
-- account entries come from different counters.
DROP INDEX module_params_history_module_idx;
CREATE INDEX module_params_history_module_idx ON module_params_history (module_id, id);
