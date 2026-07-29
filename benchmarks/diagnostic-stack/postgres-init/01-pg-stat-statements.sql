-- shared_preload_libraries loads the pg_stat_statements library into the
-- server, but the SQL-visible views and functions only exist after the
-- extension is created in the database. Without this, run-diag.sh's query
-- attribution degrades to a warning and the database half of every bottleneck
-- claim is missing.
--
-- Runs once, on first initialisation of the data volume. `docker compose down -v`
-- drops the volume and this runs again on the next `up`.
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
