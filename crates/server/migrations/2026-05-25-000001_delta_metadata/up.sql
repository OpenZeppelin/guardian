-- Typed dashboard metadata blob persisted at push time. Nullable:
-- rows without a decodable TransactionSummary (e.g. EVM) and
-- pre-migration historical rows stay NULL. See
-- `crates/server/src/delta_summary/mod.rs`.

ALTER TABLE deltas ADD COLUMN metadata JSONB;
