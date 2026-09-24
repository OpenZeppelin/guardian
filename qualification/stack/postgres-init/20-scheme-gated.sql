-- The scheme-gate scenario needs a GUARDIAN configured to refuse one scheme,
-- and account metadata is per database, so it cannot share the main server's.
CREATE DATABASE guardian_scheme_gated OWNER guardian;
