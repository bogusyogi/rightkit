-- Applied to every shard database (DB_0 .. DB_{SHARD_COUNT-1}); all shards share this schema.
-- rightkit-sync-worker 0.1.0: records, zones, rate limits (issue #146).
-- The server stores opaque encrypted bodies only. Every column except `enc` is
-- ordering/precondition metadata the protocol needs in clear.

CREATE TABLE zones (
  app        TEXT    NOT NULL,
  account_id TEXT    NOT NULL,
  zone       TEXT    NOT NULL,
  last_seq   INTEGER NOT NULL DEFAULT 0,   -- last issued per-zone sequence
  created_at INTEGER NOT NULL,             -- unix ms
  PRIMARY KEY (app, account_id, zone)
);

CREATE TABLE records (
  app        TEXT    NOT NULL,
  account_id TEXT    NOT NULL,
  zone       TEXT    NOT NULL,
  name       TEXT    NOT NULL,
  type       TEXT    NOT NULL,
  seq        INTEGER NOT NULL,             -- zone sequence of the last write
  version    INTEGER NOT NULL DEFAULT 1,   -- row version; change_tag = decimal text of it
  updated_at INTEGER NOT NULL,             -- client updatedAt, unix ms
  deleted_at INTEGER,                      -- tombstone marker, unix ms
  enc        BLOB,                         -- inline ciphertext (<= BLOB_SPILL_BYTES)
  r2_key     TEXT,                         -- spilled ciphertext in R2
  size_bytes INTEGER NOT NULL DEFAULT 0,   -- ciphertext length, for the storage cap
  created_at INTEGER NOT NULL,             -- first server write, unix ms
  PRIMARY KEY (app, account_id, zone, name),
  CHECK (enc IS NULL OR r2_key IS NULL)
);

CREATE INDEX idx_records_feed ON records (app, account_id, zone, seq);

CREATE TABLE rate_limits (
  app          TEXT    NOT NULL,
  account_id   TEXT    NOT NULL,
  window_start INTEGER NOT NULL,           -- unix ms, floored to the window
  count        INTEGER NOT NULL,
  PRIMARY KEY (app, account_id, window_start)
);

CREATE INDEX idx_rate_limits_window ON rate_limits (window_start);
