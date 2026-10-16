-- Policy module state (RFC 0002, issue #251). Kept apart from account_metadata,
-- which configure_account rewrites in full: operators write these rows, cosigners
-- write the metadata row. Instance definitions stay in the catalog file.

CREATE TABLE module_instances (
    module_id     TEXT        PRIMARY KEY,
    enabled       BOOLEAN     NOT NULL,
    global_params JSONB       NOT NULL,
    seeded_params JSONB       NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL,
    updated_by    TEXT        NOT NULL
);

-- Account layers on scope = all instances (only add restrictions) and
-- assignments of default / assigned instances (position = evaluation order).
CREATE TABLE module_account_params (
    account_id TEXT        NOT NULL,
    module_id  TEXT        NOT NULL,
    kind       TEXT        NOT NULL CHECK (kind IN ('layer', 'assignment')),
    position   INTEGER     NOT NULL,
    params     JSONB       NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    updated_by TEXT        NOT NULL,
    PRIMARY KEY (account_id, module_id)
);

CREATE INDEX module_account_params_module_idx ON module_account_params (module_id);

-- One row. Every module write takes this row lock first and bumps it, so
-- writes serialize and the version moves in commit order; replicas key their
-- cache on it. A sequence would hand out numbers in start order instead, and a
-- slow commit with a lower number would never be noticed.
CREATE TABLE module_state_version (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    version   BIGINT  NOT NULL
);

INSERT INTO module_state_version (singleton, version) VALUES (TRUE, 0);

-- Append-only; rows of one write share its version.
CREATE TABLE module_params_history (
    id              BIGSERIAL   PRIMARY KEY,
    version         BIGINT      NOT NULL,
    module_id       TEXT        NOT NULL,
    account_id      TEXT        NULL,
    previous_params JSONB       NOT NULL,
    new_params      JSONB       NOT NULL,
    operator        TEXT        NOT NULL,
    reason          TEXT        NOT NULL,
    changed_at      TIMESTAMPTZ NOT NULL
);

CREATE INDEX module_params_history_module_idx ON module_params_history (module_id, version);
