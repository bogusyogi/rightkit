CREATE TABLE IF NOT EXISTS pinned_files (
  path         TEXT PRIMARY KEY,
  display_name TEXT NOT NULL,
  format       TEXT NOT NULL,        -- 'md' | 'mdx' | 'txt' | 'pdf'
  sort_order   INTEGER NOT NULL,
  pinned_at    INTEGER NOT NULL,
  kind         TEXT NOT NULL DEFAULT 'file'   -- 'file' | 'dir'
);

CREATE TABLE IF NOT EXISTS recent_files (
  path           TEXT PRIMARY KEY,
  display_name   TEXT NOT NULL,
  format         TEXT NOT NULL,
  opened_at      INTEGER NOT NULL,
  last_view      TEXT,               -- 'preview' | 'source' | 'split'
  last_scroll_pos INTEGER DEFAULT 0,
  view_meta      TEXT                -- per-format view metadata (JSON)
);

CREATE INDEX IF NOT EXISTS idx_recent_by_opened ON recent_files(opened_at DESC);

CREATE TABLE IF NOT EXISTS session_tabs (
  sort_order INTEGER PRIMARY KEY,
  path       TEXT NOT NULL,
  active     INTEGER NOT NULL DEFAULT 0
);
