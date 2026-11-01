// Migration v21: agent `attach_file` (a user-approved path). The approval kind
// gains `attach`; SQLite cannot widen a CHECK, so `agent_approvals` is rebuilt
// with every row copied by column name. `agent_attach_requests` holds the exact
// absolute path of one pending attach approval (the path is the request; the
// size and time are read when the card opens, and they bind the card's hash).
pub const SCHEMA_V21: &str = r#"
CREATE TABLE agent_approvals__new (
  id         TEXT PRIMARY KEY,
  agent_id   TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  account_id TEXT NOT NULL,
  draft_id   TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  boot_id    TEXT NOT NULL,
  status     TEXT NOT NULL CHECK(status IN ('pending','approved','sent','denied','failed')),
  reason     TEXT,
  decided_at INTEGER,
  result_id  TEXT,
  kind       TEXT NOT NULL DEFAULT 'send' CHECK(kind IN ('send','forward','delete','attach')),
  target_key TEXT
);
INSERT INTO agent_approvals__new(id, agent_id, account_id, draft_id, created_at, expires_at,
                                 boot_id, status, reason, decided_at, result_id, kind, target_key)
  SELECT id, agent_id, account_id, draft_id, created_at, expires_at,
         boot_id, status, reason, decided_at, result_id, kind, target_key
  FROM agent_approvals ORDER BY rowid;
DROP TABLE agent_approvals;
ALTER TABLE agent_approvals__new RENAME TO agent_approvals;
CREATE INDEX IF NOT EXISTS idx_agent_approvals_agent ON agent_approvals(agent_id, created_at);
CREATE INDEX IF NOT EXISTS idx_agent_approvals_account ON agent_approvals(account_id, created_at);
CREATE INDEX IF NOT EXISTS idx_agent_approvals_status ON agent_approvals(status);
CREATE TABLE IF NOT EXISTS agent_attach_requests (
  approval_id TEXT PRIMARY KEY REFERENCES agent_approvals(id) ON DELETE CASCADE,
  path        TEXT NOT NULL
);
"#;
