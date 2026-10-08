-- The execution-refusals scenario needs a GUARDIAN that offers execution, and
-- account metadata is per database, so it cannot share the main server's.
CREATE DATABASE guardian_executing OWNER guardian;
