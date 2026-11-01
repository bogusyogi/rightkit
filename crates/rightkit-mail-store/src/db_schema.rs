// SQLite/SQLCipher schema for the mail mirror — pure SQL string constants and
// the schema version, extracted from db.rs (T24, first decomposition slice).
// `db::migrations::MIGRATIONS` runs each versioned step in order, one transaction per step. No logic here.

/// Current schema version: the position of the last step in `db::migrations::MIGRATIONS`.
// Migration v8: candidate-driven search shape.
// has_attachments is the high-frequency anchor for `has:attachment` search.
// The old (account_id, has_attachments) index forces a per-account scan and
// the planner still walks 125k+ messages for the all-inboxes case. The new
// partial index leads the planner to the row set without scanning the
// messages table for `has_attachments = 0` rows.
//
//   CREATE INDEX messages_has_attachments_only ... WHERE has_attachments = 1
//
// The partial form is the smallest index that lets the candidate-driven
// `SELECT DISTINCT account_id, thread_id FROM messages WHERE has_attachments
// = 1` walk only the rows that match. On the pinned dev DB this turns the
// `has:attachment` p95 from multi-second into a sub-500ms candidate scan.
pub const SCHEMA_V8: &str = r#"
CREATE INDEX IF NOT EXISTS idx_messages_has_attachments_partial
  ON messages(has_attachments, account_id, thread_id)
  WHERE has_attachments = 1;
"#;

// Migration v9: rowid-keyed FTS.
// fts_messages rows historically carried auto-assigned rowids with no relation
// to the messages table, so the per-message maintenance delete
// (`DELETE FROM fts_messages WHERE account_id=? AND message_id=?`) was a full
// scan of every indexed email body — FTS5 cannot use an index for WHERE
// equality on its columns (and message_id/account_id are UNINDEXED anyway).
// On a large mailbox that made every sync batch O(mailbox) inside the global
// connection lock (~60 s per 25 messages on a 2 GB DB — the "app hangs during
// download-all" bug, 2026-07-03).
//
// v9 re-keys every FTS row to `rowid = messages.rowid` so per-message deletes
// become rowid lookups. DROP + CREATE is the instant truncation for a regular
// (content-storing) FTS5 table ('delete-all' only exists for contentless ones);
// the CREATE must stay byte-identical to the SCHEMA_V1 definition. The reinsert
// is one pass over messages ⋈ message_bodies — a one-time cost at first launch
// after upgrade. Writers keep the invariant: rebuild_fts_for_message_ids /
// rebuild_rollups insert with explicit rowid, upsert_messages preserves rowids
// (ON CONFLICT DO UPDATE), and nothing runs VACUUM (which could renumber
// messages.rowid; if ever introduced, re-run this rebuild afterwards).
pub const SCHEMA_V9: &str = r#"
DROP TABLE IF EXISTS fts_messages;
CREATE VIRTUAL TABLE fts_messages USING fts5(
  message_id UNINDEXED,
  account_id UNINDEXED,
  subject,
  from_text,
  body,
  tokenize = 'porter unicode61'
);
INSERT INTO fts_messages(rowid, message_id, account_id, subject, from_text, body)
SELECT m.rowid, m.id, m.account_id, COALESCE(m.subject, ''),
       COALESCE(m.from_name, '') || ' ' || COALESCE(m.from_addr, ''),
       COALESCE(
         (SELECT body FROM message_bodies b
            WHERE b.account_id=m.account_id AND b.message_id=m.id AND b.mime_type='text/plain'),
         (SELECT body FROM message_bodies b
            WHERE b.account_id=m.account_id AND b.message_id=m.id AND b.mime_type='text/html'),
         m.snippet, '')
FROM messages m;
"#;

// Migration v10: partial index for the unread hot path (L9, 2026-07-04).
// `unread_counts` (db/threads/listing.rs:204) is one of the two highest-
// frequency queries — it powers every sidebar badge refresh. It used to scan
// every threads row in the account and filter on `WHERE t.unread = 1`, which
// is cheap on a small mailbox but wasteful when most threads are read (the
// steady state for any active account).
//
// The partial index leads the planner to the exact unread subset in O(unread)
// instead of O(threads). Shape matches the existing threads ordering —
// `(account_id, last_message_at DESC)` — so it also serves the Unread folder
// view (list_threads(label='UNREAD') orders by last_message_at DESC and
// benefits from the same index, not just the unread-count query).
//
//   CREATE INDEX idx_threads_unread_partial
//     ON threads(account_id, last_message_at DESC)
//     WHERE unread = 1
//
// The partial form is critical: a full index on (account_id, last_message_at)
// would already exist alongside idx_threads_by_account (v1), so the partial
// is the smallest index that makes the unread path an index-only walk.
pub const SCHEMA_V10: &str = r#"
CREATE INDEX IF NOT EXISTS idx_threads_unread_partial
  ON threads(account_id, last_message_at DESC)
  WHERE unread = 1;
"#;

// Migration v11: normalized recipient metadata (L10, 2026-07-05).
// `to:`/`cc:`/`bcc:`/`from:` search read the raw header-string columns
// (messages.to_addrs/cc_addrs/bcc_addrs/from_addr) with `LIKE '%term%'` —
// a leading wildcard defeats any index and forces a full scan of every
// message in scope (coverage-audit #1/#5/#6, P17/P22 in DISPATCH.md). Gmail
// header values are comma-joined "Name <addr@x.com>" strings, so even an
// exact-address search had to substring-match the whole blob.
//
// message_recipients normalizes every message's from/to/cc/bcc into one row
// per (message, recipient), with addr_norm/name_norm already lowercased so
// operator resolution becomes an indexed prefix lookup
// (`addr_norm LIKE 'term%'`) instead of a full scan. The covering index
// leads the planner straight to the matching (account_id, message_id) rows
// without touching the messages table at all for the recipient predicate.
//
//   CREATE INDEX idx_message_recipients_lookup
//     ON message_recipients(account_id, addr_norm, kind)
//
// ADD only: the existing to_addrs/cc_addrs/bcc_addrs/from_addr columns are
// untouched and remain the write path from Gmail sync (db.rs::upsert_messages
// also writes message_recipients going forward — see db/recipients.rs). This
// table is fully derived and rebuildable from those columns, so a stale/
// corrupt copy can always be repopulated via
// `recipients::backfill_message_recipients` without any data loss (see
// docs/LOCAL_SEARCH_ARCHITECTURE.md Phase 2 backfill-verification rules).
pub const SCHEMA_V11: &str = r#"
CREATE TABLE IF NOT EXISTS message_recipients (
  account_id TEXT NOT NULL,
  message_id TEXT NOT NULL,
  kind       TEXT NOT NULL CHECK(kind IN ('to','cc','bcc','from')),
  addr_norm  TEXT NOT NULL,
  name_norm  TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_message_recipients_lookup
  ON message_recipients(account_id, addr_norm, kind);
CREATE INDEX IF NOT EXISTS idx_message_recipients_message
  ON message_recipients(account_id, message_id);
"#;

// Migration v12: search-parser operator parity (L11, 2026-07-05).
// - `rfc822msgid:` needs an exact-match lookup on messages.rfc822_message_id.
//   The column has existed since SCHEMA_V1 but was never indexed. The index
//   is added here; the write path (Gmail fetch → FetchedMessage → upsert)
//   is wired in L11b (gmail/message_fetch.rs parses the Message-Id header,
//   db.rs::upsert_messages persists it) so rows fetched/re-fetched after
//   L11b populate this column. Rows fetched before L11b (or metadata-only
//   fetches that omit the header) stay NULL until next re-fetch — never
//   fabricated.
// - `size:`/`larger:`/`smaller:` need a byte-size column on messages. Gmail
//   exposes `sizeEstimate` at the top level of the message resource for both
//   format=metadata and format=full. L11b wires the write path the same way
//   as rfc822_message_id above. Rows without a fetched size stay NULL — a
//   numeric comparison against NULL is never true, so the operator simply
//   matches nothing for those rows rather than false-matching.
pub const SCHEMA_V12: &str = r#"
ALTER TABLE messages ADD COLUMN size_estimate INTEGER;
CREATE INDEX IF NOT EXISTS idx_messages_rfc822_message_id
  ON messages(account_id, rfc822_message_id);
CREATE INDEX IF NOT EXISTS idx_messages_size_estimate
  ON messages(account_id, size_estimate);
"#;

// Migration v13: local semantic search (C5, 2026-07-05).
// `message_vec` is a `vec0` virtual table (sqlite-vec, registered as a static
// SQLite extension in db/semantic/vec_ext.rs BEFORE this connection was
// opened) storing one 256-dim Matryoshka-truncated float32 embedding per
// message, keyed by `rowid = messages.rowid` — the same schema-v9 rowid
// invariant recipients/FTS already rely on, so per-message vector
// maintenance is a rowid lookup rather than a scan.
//
// `semantic_index_state` tracks per-account indexing progress (resumable
// cursor, mirrors the metadata-backfill/download-all cursor pattern in
// sync/backfill.rs) plus the content hash the row was embedded from, so a
// model/dimension change (recorded in `settings`: semantic_embed_model_id /
// semantic_embed_dim) can detect "needs reindex" without a full table scan
// of vec0 itself (vec0 has no extra columns to store that metadata on).
//
// vec0's KNN distance column is `distance` (cosine, since sqlite-vec's
// `vec0` defaults to L2 unless declared otherwise — this schema uses the
// default L2 distance over L2-normalized vectors, which ranks identically
// to cosine distance for unit vectors: `||a-b||^2 = 2 - 2*cos(a,b)`).
pub const SCHEMA_V13: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS message_vec USING vec0(
  embedding float[256]
);
CREATE TABLE IF NOT EXISTS semantic_index_state (
  account_id     TEXT PRIMARY KEY,
  content_hash   TEXT NOT NULL DEFAULT '',
  embed_model_id TEXT NOT NULL DEFAULT '',
  embed_dim      INTEGER NOT NULL DEFAULT 0,
  cursor_rowid   INTEGER NOT NULL DEFAULT 0,
  done           INTEGER NOT NULL DEFAULT 0,
  indexed_count  INTEGER NOT NULL DEFAULT 0,
  updated_at     INTEGER NOT NULL DEFAULT 0
);
"#;

// Migration v14: label-first index for the unified (all accounts) label list.
// The v6 index leads with account_id, so a label list with no account filter
// scanned every account prefix; measured 180 ms -> 0.4 ms on a 127k-thread mailbox.
pub const SCHEMA_V14: &str = r#"
CREATE INDEX IF NOT EXISTS idx_thread_labels_label_account_thread
  ON thread_labels(label, account_id, thread_id);
"#;

// v15 (external-content FTS over plain text) has no SQL constant: its steps
// are code in db/fts_index.rs because the rebuild runs as a resumable
// background job, not inside the migration transaction.
// Migration v16: agent gate (docs/plans/AGENT-DRIVABLE-MAILRIGHT.md). Lives in
// the encrypted app database so a caller cannot edit policy. `key_hash` is the
// SHA-256 (hex) of a pairing key; the key itself is never stored. `agent_audit`
// is append-only, enforced by triggers; `detail` holds counts and codes only.
pub const SCHEMA_V16: &str = r#"
CREATE TABLE IF NOT EXISTS agents (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL,
  key_hash   TEXT NOT NULL,
  profile    TEXT NOT NULL CHECK(profile IN ('read_only','draft','send_with_approval')),
  created_at INTEGER NOT NULL,
  revoked_at INTEGER
);
CREATE TABLE IF NOT EXISTS agent_accounts (
  agent_id   TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  PRIMARY KEY (agent_id, account_id)
);
CREATE TABLE IF NOT EXISTS agent_audit (
  seq        INTEGER PRIMARY KEY AUTOINCREMENT,
  at         INTEGER NOT NULL,
  agent_id   TEXT,
  agent_name TEXT,
  account_id TEXT,
  method     TEXT NOT NULL,
  outcome    TEXT NOT NULL,
  detail     TEXT
);
CREATE TRIGGER IF NOT EXISTS agent_audit_no_update
BEFORE UPDATE ON agent_audit
BEGIN
  SELECT RAISE(ABORT, 'agent_audit is append-only');
END;
CREATE TRIGGER IF NOT EXISTS agent_audit_no_delete
BEFORE DELETE ON agent_audit
BEGIN
  SELECT RAISE(ABORT, 'agent_audit is append-only');
END;
"#;

// Migration v17: agent send (docs/plans/AGENT-DRIVABLE-MAILRIGHT.md, stage 3).
// `agent_drafts` records drafts created through the agent `draft` command (only
// these can be sent); `content_hash` is SHA-256 over the validated recipients,
// subject and body. `agent_approvals` is one row per send request: its status
// moves pending -> approved -> sent|failed, or pending -> denied, and each move
// is a single guarded UPDATE. It holds ids, times and reason codes only, never
// recipients, subject or body.
pub const SCHEMA_V17: &str = r#"
CREATE TABLE IF NOT EXISTS agent_drafts (
  agent_id     TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  account_id   TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  draft_id     TEXT NOT NULL,
  created_at   INTEGER NOT NULL,
  content_hash TEXT NOT NULL,
  PRIMARY KEY (agent_id, account_id, draft_id)
);
CREATE TABLE IF NOT EXISTS agent_approvals (
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
  result_id  TEXT
);
CREATE INDEX IF NOT EXISTS idx_agent_approvals_agent ON agent_approvals(agent_id, created_at);
CREATE INDEX IF NOT EXISTS idx_agent_approvals_account ON agent_approvals(account_id, created_at);
CREATE INDEX IF NOT EXISTS idx_agent_approvals_status ON agent_approvals(status);
"#;

// Migration v18: agent forward and delete (docs/plans/AGENT-DRIVABLE-MAILRIGHT.md).
// Both reuse the send approval table. `agent_approvals.kind` is send | forward |
// delete; for a delete, `draft_id` holds the thread id, and status `sent` means
// "completed" (the thread was moved to Trash). `agent_drafts.kind` is draft |
// forward: only a `draft` record can be sent through `send`, so a forward draft
// can never be approved under the plain Send heading. Additive columns with
// defaults: every existing row is a send. The ALTERs run from
// `db::migrations` (add_column_if_missing) so a repeated run is harmless.
pub const V18_APPROVAL_KIND: &str =
    "TEXT NOT NULL DEFAULT 'send' CHECK(kind IN ('send','forward','delete'))";
pub const V18_DRAFT_KIND: &str = "TEXT NOT NULL DEFAULT 'draft' CHECK(kind IN ('draft','forward'))";

// Migration v19: decline cooldown and forward attachment digest. Additive
// nullable columns, applied by `db::migrations` (add_column_if_missing).
// `agent_approvals.target_key` names what a request is about (a draft id for a
// send, a thread id for a delete, a hash of the original message id and the
// recipient set for a forward) so a user's denial can block a repeat for a
// while; it holds ids or a hash, never an address. `agent_drafts.attachment_digest`
// is one SHA-256 over the bytes of a draft's attachments; it is stored,
// not compared at approve.
pub const V19_TARGET_KEY: &str = "TEXT";
pub const V19_ATTACHMENT_DIGEST: &str = "TEXT";

// Migration v20: embedding pipeline key. Additive nullable column on
// `semantic_index_state`, applied by `db::migrations` (add_column_if_missing).
// It records which embedding pipeline (model, quantisation, prompts,
// truncation) produced an account's stored vectors; NULL (every pre-v20 row)
// means unknown, so those accounts re-embed once.
pub const V20_PIPELINE_KEY: &str = "TEXT";

pub const SCHEMA_VERSION: i64 = 22;

pub const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS accounts (
  id                TEXT PRIMARY KEY,            -- gmail address
  display_name      TEXT NOT NULL DEFAULT '',
  color             TEXT NOT NULL DEFAULT '#C46A3D',
  history_id        TEXT,
  last_full_sync_at INTEGER,
  last_sync_at      INTEGER,
  last_error        TEXT,
  created_at        INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS labels (
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  id         TEXT NOT NULL,                      -- gmail label id
  name       TEXT NOT NULL,
  type       TEXT,                               -- system | user
  PRIMARY KEY (account_id, id)
);

CREATE TABLE IF NOT EXISTS threads (
  account_id      TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  id              TEXT NOT NULL,                 -- gmail thread id
  subject         TEXT,
  snippet         TEXT,
  unread          INTEGER NOT NULL DEFAULT 0,
  starred         INTEGER NOT NULL DEFAULT 0,
  label_ids       TEXT,                          -- JSON array
  last_message_at INTEGER NOT NULL DEFAULT 0,
  message_count   INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, id)
);
CREATE INDEX IF NOT EXISTS idx_threads_unified ON threads(last_message_at DESC);
CREATE INDEX IF NOT EXISTS idx_threads_by_account ON threads(account_id, last_message_at DESC);

CREATE TABLE IF NOT EXISTS messages (
  account_id        TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  id                TEXT NOT NULL,               -- gmail message id
  thread_id         TEXT NOT NULL,
  from_addr         TEXT,
  from_name         TEXT,
  to_addrs          TEXT,                        -- JSON array
  cc_addrs          TEXT,                        -- JSON array
  bcc_addrs         TEXT,                        -- JSON array
  subject           TEXT,
  snippet           TEXT,
  internal_date     INTEGER NOT NULL DEFAULT 0,
  rfc822_message_id TEXT,
  label_ids         TEXT,                        -- JSON array
  unread            INTEGER NOT NULL DEFAULT 0,
  has_attachments   INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, id)
);
CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(account_id, thread_id, internal_date);

CREATE TABLE IF NOT EXISTS message_bodies (
  account_id TEXT NOT NULL,
  message_id TEXT NOT NULL,
  mime_type  TEXT NOT NULL,                      -- text/plain | text/html
  body       TEXT,
  PRIMARY KEY (account_id, message_id, mime_type)
);

CREATE TABLE IF NOT EXISTS attachments (
  account_id    TEXT NOT NULL,
  message_id    TEXT NOT NULL,
  attachment_id TEXT NOT NULL,
  filename      TEXT,
  mime_type     TEXT,
  size          INTEGER,
  PRIMARY KEY (account_id, message_id, attachment_id)
);

CREATE TABLE IF NOT EXISTS sync_state (
  account_id       TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
  last_history_id  TEXT,
  last_full_sync_at INTEGER,
  lease_owner      TEXT,                         -- per-account sync lease
  lease_expires_at INTEGER,
  last_error       TEXT
);

CREATE TABLE IF NOT EXISTS aliases (
  account_id    TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  send_as_email TEXT NOT NULL,
  display_name  TEXT,
  is_default    INTEGER NOT NULL DEFAULT 0,
  is_primary    INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, send_as_email)
);

CREATE TABLE IF NOT EXISTS drafts (
  id            TEXT PRIMARY KEY,                -- local uuid
  account_id    TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  gmail_draft_id TEXT,
  thread_id     TEXT,
  to_addrs      TEXT,
  cc_addrs      TEXT,
  bcc_addrs     TEXT,
  subject       TEXT,
  body          TEXT,
  send_as       TEXT,
  updated_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS outbox (
  id          TEXT PRIMARY KEY,                  -- local uuid = idempotency key
  account_id  TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  draft_id    TEXT,
  rfc822      TEXT NOT NULL,                     -- built MIME
  status      TEXT NOT NULL DEFAULT 'queued',    -- queued|sending|sent|failed
  attempts    INTEGER NOT NULL DEFAULT 0,
  last_error  TEXT,
  created_at  INTEGER NOT NULL,
  sent_at     INTEGER
);
CREATE INDEX IF NOT EXISTS idx_outbox_status ON outbox(status, created_at);

CREATE VIRTUAL TABLE IF NOT EXISTS fts_messages USING fts5(
  message_id UNINDEXED,
  account_id UNINDEXED,
  subject,
  from_text,
  body,
  tokenize = 'porter unicode61'
);

CREATE TABLE IF NOT EXISTS ai_summaries (
  account_id TEXT NOT NULL,
  thread_id  TEXT NOT NULL,
  provider   TEXT,
  model      TEXT,
  summary    TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (account_id, thread_id)
);

CREATE TABLE IF NOT EXISTS settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_log (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  at         INTEGER NOT NULL,
  kind       TEXT NOT NULL,                      -- oauth|send|sync|mcp|ai
  account_id TEXT,
  detail     TEXT
);
"#;

// Migration v2: normalize per-thread labels into their own indexable table,
// replacing the label_ids LIKE substring match. FK to accounts so removing an
// account cascades its label rows; no FK to threads (apply_messages rebuilds
// threads each sync and re-derives these rows in the same transaction).
pub const SCHEMA_V2: &str = r#"
CREATE TABLE IF NOT EXISTS thread_labels (
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  thread_id  TEXT NOT NULL,
  label      TEXT NOT NULL,
  PRIMARY KEY (account_id, thread_id, label)
);
CREATE INDEX IF NOT EXISTS idx_thread_labels_label ON thread_labels(account_id, label);
"#;

// Migration v3: store the extra message headers the reader needs — reply target,
// one-click unsubscribe, and (already-declared) recipients. ALTER ADD COLUMN is
// non-destructive; existing rows get NULL and backfill on the next sync.
pub const SCHEMA_V3: &str = r#"
ALTER TABLE messages ADD COLUMN reply_to TEXT;
ALTER TABLE messages ADD COLUMN list_unsubscribe TEXT;
"#;

// Migration v4: MailRight-managed snooze (a local hide-until-time, since Gmail
// has no public Snooze API) + a send_at column on outbox for scheduled send.
pub const SCHEMA_V4: &str = r#"
CREATE TABLE IF NOT EXISTS snoozes (
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  thread_id  TEXT NOT NULL,
  until_ms   INTEGER NOT NULL,
  PRIMARY KEY (account_id, thread_id)
);
CREATE INDEX IF NOT EXISTS idx_snoozes_until ON snoozes(until_ms);
ALTER TABLE outbox ADD COLUMN send_at INTEGER;
"#;

pub const SCHEMA_V5: &str = r#"
-- Durable queue for offline label/archive/trash/star/read mutations.
-- These operations are applied optimistically to local state, then replayed
-- against Gmail once connectivity returns.
CREATE TABLE IF NOT EXISTS pending_ops (
  id          TEXT PRIMARY KEY,           -- local uuid = idempotency key
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  op_class    TEXT NOT NULL,             -- label_modify, read_write, etc (coarse)
  action      TEXT NOT NULL,             -- archive|trash|read|unread|star|...
  thread_id   TEXT NOT NULL,
  add_labels  TEXT NOT NULL,            -- JSON array of label ids to add
  rem_labels  TEXT NOT NULL,            -- JSON array of label ids to remove
  attempts    INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_pending_ops_account_created
  ON pending_ops(account_id, created_at);
"#;

pub const SCHEMA_V6: &str = r#"
-- Cover the hot visible-thread scope query:
-- EXISTS(thread_labels account,label,thread) for INBOX/category tabs.
CREATE INDEX IF NOT EXISTS idx_thread_labels_label_thread
  ON thread_labels(account_id, label, thread_id);
"#;

// Migration v7: MailRight-local search indexes for Gmail-parity v1.
// - Recipients (cc/bcc) already exist in SCHEMA_V1 and become indexed/searchable.
// - attachments.filename is indexable; messages.has_attachments / from_addr /
//   subject get explicit indexes so the search builder never falls back to
//   full-table scans.
// - threads.important covers is:important; db.rs adds the column idempotently
//   before running this index batch.
pub const SCHEMA_V7: &str = r#"
CREATE INDEX IF NOT EXISTS idx_messages_has_attachments
  ON messages(account_id, has_attachments);
CREATE INDEX IF NOT EXISTS idx_messages_from_addr
  ON messages(account_id, from_addr);
CREATE INDEX IF NOT EXISTS idx_messages_subject
  ON messages(account_id, subject);
CREATE INDEX IF NOT EXISTS idx_attachments_filename
  ON attachments(account_id, filename);
CREATE INDEX IF NOT EXISTS idx_threads_important
  ON threads(account_id, important);
"#;
