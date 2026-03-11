-- Add last_active_at column for idle session timeout.
-- SQLite ALTER TABLE does not support non-constant defaults, so we add the
-- column with a constant default and then backfill from created_at.
ALTER TABLE sessions ADD COLUMN last_active_at TEXT NOT NULL DEFAULT '1970-01-01 00:00:00';
UPDATE sessions SET last_active_at = created_at WHERE last_active_at = '1970-01-01 00:00:00';
