-- Add last_active_at column for idle session timeout.
-- Defaults to created_at for existing rows.
ALTER TABLE sessions ADD COLUMN last_active_at TEXT NOT NULL DEFAULT (datetime('now'));
