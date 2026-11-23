-- The default modules a first registration copies into its account, in one call.
-- Every module write takes the module_state_version row lock, so a registration
-- that held it across a round trip per row made every other registration and
-- operator write wait for all of them. Here the lock is taken, the account is
-- checked and its rows and history are written inside one statement, so the lock
-- is held for that statement and its commit only.
--
-- p_modules is the assignment in evaluation order:
-- [{"module_id": "...", "params": {...}}, ...]. Writes only into an account with
-- no assignment and no history, so an operator's earlier write always wins.
-- Returns the version the copy committed as, or NULL when it wrote nothing (the
-- version does not move then).
CREATE FUNCTION guardian_module_seed_assignment(
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
