-- rightkit-sync-worker 0.1.0: account erase and tombstone compaction (issue #147).

-- Cursors older than floor_seq are answered with 410 token_expired.
ALTER TABLE zones ADD COLUMN floor_seq INTEGER NOT NULL DEFAULT 0;

-- Set when a tombstone was reduced to (name, type, updated_at, deleted_at).
ALTER TABLE records ADD COLUMN compacted_at INTEGER;

CREATE INDEX idx_records_compactable ON records (deleted_at)
  WHERE deleted_at IS NOT NULL AND compacted_at IS NULL;

-- Written by account erase. Sync tokens issued at or before erased_at are revoked.
CREATE TABLE erased_accounts (
  app        TEXT    NOT NULL,
  account_id TEXT    NOT NULL,
  erased_at  INTEGER NOT NULL,             -- unix ms
  PRIMARY KEY (app, account_id)
);
