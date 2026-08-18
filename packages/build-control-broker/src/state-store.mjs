import { randomUUID, createHash } from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';
import { dirname, isAbsolute, normalize } from 'node:path';
import { mkdir, readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
// Windows editors write a leading UTF-8 BOM by default; JSON.parse does not
// strip it. The provenance hash below is intentionally over the RAW bytes, not
// this stripped view — sourceSha256 must represent the file exactly as found.
function stripBom(text) { return typeof text === 'string' && text.charCodeAt(0) === 0xfeff ? text.slice(1) : text; }

const TERMINAL_STATES = new Set(['SUCCEEDED', 'FAILED', 'CANCELLED', 'TIMED_OUT']);
const OPEN_STATES = new Set(['SUBMITTED', 'CLASSIFIED', 'QUEUED', 'ADMITTED', 'STARTING', 'RUNNING', 'REAPING']);
const LIFECYCLE_STATES = new Set([...OPEN_STATES, ...TERMINAL_STATES]);
const NEXT_OPEN_STATE = new Map([
  ['SUBMITTED', 'CLASSIFIED'],
  ['CLASSIFIED', 'QUEUED'],
  ['QUEUED', 'ADMITTED'],
  ['ADMITTED', 'STARTING'],
  ['STARTING', 'RUNNING'],
  ['RUNNING', 'REAPING'],
]);
const RECOVERABLE_QUEUED_STATES = new Set(['SUBMITTED', 'CLASSIFIED', 'QUEUED']);
const NAMESPACE_NAME = /^[a-f0-9]{64}$/;
const MAINTENANCE_TERMINAL_STATES = new Set(['RELEASED', 'SUCCEEDED', 'FAILED', 'CANCELLED', 'TIMED_OUT']);

const requiredString = (value, name) => {
  if (typeof value !== 'string' || value.length === 0) throw new TypeError(`${name} is required`);
  return value;
};
const object = (value, name) => {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError(`${name} must be an object`);
  return value;
};
const state = (value) => {
  requiredString(value, 'state');
  if (!LIFECYCLE_STATES.has(value)) throw new TypeError('recognized lifecycle state is required');
  return value;
};
const timestamp = (value) => {
  requiredString(value, 'occurredAt');
  if (Number.isNaN(Date.parse(value))) throw new TypeError('occurredAt must be an ISO timestamp');
  return value;
};
const namespaceName = (value) => {
  if (typeof value !== 'string' || !NAMESPACE_NAME.test(value)) throw new TypeError('64-character hexadecimal namespace is required');
  return value;
};
const generation = (value) => {
  if (!Number.isSafeInteger(value) || value < 0) throw new TypeError('non-negative safe namespace generation is required');
  return value;
};
const namespaceState = (value) => requiredString(value, 'namespace state');
const optionalTimestamp = (value, name) => {
  if (value !== null && value !== undefined) timestamp(value);
  return value ?? null;
};
const encode = (value) => JSON.stringify(value);
const decode = (value) => JSON.parse(value);
const rowToEntry = (row) => Object.freeze({
  id: row.id,
  state: row.state,
  data: Object.freeze(decode(row.data)),
  createdAt: row.created_at,
  updatedAt: row.updated_at,
  terminalAt: row.terminal_at ?? null,
});
const rowToNamespace = (row) => Object.freeze({
  namespace: row.namespace,
  generation: row.generation,
  state: row.state,
  terminalAt: row.terminal_at ?? null,
  lruAt: row.lru_at ?? null,
  trashId: row.trash_id ?? null,
  trashedAt: row.trashed_at ?? null,
  deletedAt: row.deleted_at ?? null,
  repositoryIdentity: row.repository_identity ?? null,
});

/**
 * SQLite-backed broker lifecycle state. A terminal transition & its receipt
 * share one SQL transaction; `terminal_receipts.request_id` makes retrying it
 * idempotent across a process restart.
 */
export class DurableStateStore {
  #database;

  static async open({ databasePath, legacyStatePath = null }) {
    requiredString(databasePath, 'databasePath');
    const path = normalize(databasePath);
    if (!isAbsolute(path)) throw new TypeError('databasePath must be absolute');
    await mkdir(dirname(path), { recursive: true });
    let legacy = null;
    if (legacyStatePath && normalize(legacyStatePath) !== path) {
      try { const raw = await readFile(normalize(legacyStatePath), 'utf8'); legacy = { value: JSON.parse(stripBom(raw)), sourceSha256: createHash('sha256').update(raw).digest('hex') }; } catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
    return new DurableStateStore(path, legacy);
  }

  constructor(databasePath, legacy = null) {
    this.#database = new DatabaseSync(databasePath, { allowExtension: false });
    this.#database.exec(`
      PRAGMA journal_mode = WAL;
      PRAGMA synchronous = FULL;
      PRAGMA foreign_keys = ON;
      CREATE TABLE IF NOT EXISTS requests (
        id TEXT PRIMARY KEY,
        state TEXT NOT NULL,
        data TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        terminal_at TEXT
      ) STRICT;
      CREATE TABLE IF NOT EXISTS lifecycle_transitions (
        sequence INTEGER PRIMARY KEY,
        request_id TEXT NOT NULL REFERENCES requests(id),
        from_state TEXT,
        to_state TEXT NOT NULL,
        occurred_at TEXT NOT NULL,
        data TEXT NOT NULL
      ) STRICT;
      CREATE INDEX IF NOT EXISTS lifecycle_transitions_request_sequence
        ON lifecycle_transitions(request_id, sequence);
      CREATE TABLE IF NOT EXISTS terminal_receipts (
        request_id TEXT PRIMARY KEY REFERENCES requests(id),
        state TEXT NOT NULL,
        receipt TEXT NOT NULL,
        created_at TEXT NOT NULL
      ) STRICT;
      CREATE TABLE IF NOT EXISTS namespaces (
        namespace TEXT PRIMARY KEY,
        generation INTEGER NOT NULL,
        state TEXT NOT NULL,
        terminal_at TEXT,
        lru_at TEXT,
        trash_id TEXT,
        trashed_at TEXT,
        deleted_at TEXT,
        maintenance_lease_id TEXT,
        maintenance_lease_at TEXT,
        repository_identity TEXT
      ) STRICT;
      CREATE INDEX IF NOT EXISTS namespaces_retention
        ON namespaces(state, lru_at, terminal_at);
      CREATE TABLE IF NOT EXISTS migration_markers (name TEXT PRIMARY KEY, completed_at TEXT NOT NULL, details TEXT NOT NULL) STRICT;
      CREATE TABLE IF NOT EXISTS reservation_ledger (domain TEXT PRIMARY KEY, capacity INTEGER NOT NULL, reserved INTEGER NOT NULL, updated_at TEXT NOT NULL) STRICT;
      CREATE TABLE IF NOT EXISTS reservations (request_id TEXT NOT NULL, domain TEXT NOT NULL, amount INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY(request_id, domain)) STRICT;
      CREATE INDEX IF NOT EXISTS reservations_domain ON reservations(domain);
      CREATE TABLE IF NOT EXISTS pending_trash_intents (namespace TEXT NOT NULL, generation INTEGER NOT NULL, trash_id TEXT NOT NULL, lease_id TEXT NOT NULL, source_path TEXT NOT NULL, target_path TEXT NOT NULL, state TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY(namespace,generation,trash_id)) STRICT;
      CREATE TABLE IF NOT EXISTS maintenance_queue (namespace TEXT NOT NULL, generation INTEGER NOT NULL, trash_id TEXT NOT NULL, cursor TEXT, state TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY(namespace, generation, trash_id)) STRICT;
      CREATE TABLE IF NOT EXISTS broker_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL) STRICT;
    `);
    this.#migrateNamespaceRepositoryIdentity();
    this.#reconcileReservations();
    const marker = this.#database.prepare("SELECT details FROM migration_markers WHERE name = 'v1-json-to-v2'").get();
    if (legacy && marker) { const details = decode(marker.details); if (details.sourceSha256 !== legacy.sourceSha256) throw Object.assign(new Error('legacy state changed after v2 admission'), { code: 'RK_V1_LEGACY_DRIFT' }); }
    if (legacy && !marker) this.#migrateLegacy(legacy);
  }

  /** Adds the nullable app-identity column that cap eviction groups by. Fresh
   * databases already carry it from the CREATE TABLE above, so the ALTER runs
   * only when an older file lacks it; the marker records either outcome and
   * makes a second open a no-op. */
  #migrateNamespaceRepositoryIdentity() {
    const name = 'namespaces-repository-identity';
    if (this.#database.prepare('SELECT 1 AS present FROM migration_markers WHERE name = ?').get(name)?.present === 1) return;
    const present = this.#database.prepare("SELECT 1 AS present FROM pragma_table_info('namespaces') WHERE name = 'repository_identity'").get()?.present === 1;
    this.#transaction(() => {
      if (!present) this.#database.exec('ALTER TABLE namespaces ADD COLUMN repository_identity TEXT');
      this.#database.prepare('INSERT INTO migration_markers (name,completed_at,details) VALUES (?,?,?)').run(name, new Date().toISOString(), encode({ altered: !present, column: 'repository_identity' }));
    });
  }

  #migrateLegacy(input) {
    const legacy = input.value ?? input; const sourceSha256 = input.sourceSha256 ?? createHash('sha256').update(JSON.stringify(legacy)).digest('hex');
    const entries = legacy.entries ?? [];
    if (entries.some((candidate) => ['ACTIVE', 'QUEUED', 'RUNNING', 'STARTING'].includes(candidate?.state))) {
      const candidate = entries.find((item) => ['ACTIVE', 'QUEUED', 'RUNNING', 'STARTING'].includes(item?.state));
      throw Object.assign(new Error('v1 migration requires proven drain before v2 cutover'), { code: 'RK_V1_DRAIN_REQUIRED', id: candidate.id, state: candidate.state });
    }
    this.#transaction(() => {
      for (const candidate of entries) {
        if (!candidate?.id || this.#database.prepare('SELECT 1 FROM requests WHERE id = ?').get(candidate.id)) continue;
        const occurredAt = candidate.terminalAt ?? candidate.createdAt ?? new Date().toISOString();
        if (!['RELEASED', 'FAILED', 'CANCELLED'].includes(candidate.state)) throw Object.assign(new Error('unknown v1 lifecycle state'), { code: 'RK_V1_UNKNOWN_STATE', id: candidate.id, state: candidate.state });
        const mapped = { RELEASED: 'SUCCEEDED', FAILED: 'FAILED', CANCELLED: 'CANCELLED' }[candidate.state];
        const data = { ...candidate, migratedFrom: 'v1-json' };
        this.#database.prepare('INSERT INTO requests (id,state,data,created_at,updated_at,terminal_at) VALUES (?,?,?,?,?,?)').run(candidate.id, mapped, encode(data), occurredAt, occurredAt, mapped === 'SUBMITTED' ? null : occurredAt);
        this.#database.prepare('INSERT INTO lifecycle_transitions (request_id,from_state,to_state,occurred_at,data) VALUES (?,?,?,?,?)').run(candidate.id, null, mapped, occurredAt, encode({ migratedFrom: 'v1-json', legacyState: candidate.state }));
        if (mapped !== 'SUBMITTED') this.#database.prepare('INSERT INTO terminal_receipts (request_id,state,receipt,created_at) VALUES (?,?,?,?)').run(candidate.id, mapped, encode({ id: candidate.id, state: mapped, reason: 'v1-migration' }), occurredAt);
        const ns = candidate.roots?.namespace;
        if (ns && NAMESPACE_NAME.test(ns)) { const gen = Number.isSafeInteger(candidate.generation) ? candidate.generation : 0; const lru = candidate.lruAt ?? candidate.terminalAt ?? null; const old = this.#database.prepare('SELECT lru_at FROM namespaces WHERE namespace=?').get(ns); if (!old || (lru && (!old.lru_at || lru > old.lru_at))) this.#database.prepare('INSERT OR REPLACE INTO namespaces (namespace,generation,state,terminal_at,lru_at) VALUES (?,?,?,?,?)').run(ns, gen, mapped === 'SUCCEEDED' ? 'RELEASED' : mapped, candidate.terminalAt ?? null, lru); }
      }
      this.#database.prepare("INSERT INTO migration_markers (name,completed_at,details) VALUES ('v1-json-to-v2',?,?)").run(new Date().toISOString(), encode({ entries: entries.length, cutover: 'v2-admission', sourceSha256, rollbackForbiddenAfterV2Admission: true }));
    });
  }

  /** Durable key/value settings row. Used for cross-restart broker-owned
   * process ownership (the sccache server) that belongs to no single request. */
  readSetting(key) { requiredString(key, 'key'); const row = this.#database.prepare('SELECT value FROM broker_settings WHERE key = ?').get(key); return row ? decode(row.value) : null; }
  writeSetting(key, value, occurredAt = new Date().toISOString()) { requiredString(key, 'key'); timestamp(occurredAt); this.#database.prepare('INSERT OR REPLACE INTO broker_settings (key,value,updated_at) VALUES (?,?,?)').run(key, encode(value), occurredAt); return true; }
  clearSetting(key) { requiredString(key, 'key'); this.#database.prepare('DELETE FROM broker_settings WHERE key = ?').run(key); return true; }

  close() { this.#database.close(); }

  entry(id) {
    requiredString(id, 'id');
    const row = this.#database.prepare('SELECT id, state, data, created_at, updated_at, terminal_at FROM requests WHERE id = ?').get(id);
    return row ? rowToEntry(row) : null;
  }

  receipt(id) {
    requiredString(id, 'id');
    const row = this.#database.prepare('SELECT request_id, state, receipt, created_at FROM terminal_receipts WHERE request_id = ?').get(id);
    return row ? Object.freeze({ id: row.request_id, state: row.state, receipt: Object.freeze(decode(row.receipt)), createdAt: row.created_at }) : null;
  }

  updateTerminalReceipt({ id, receipt }) {
    requiredString(id, 'id'); object(receipt, 'receipt');
    return this.#transaction(() => {
      const current = this.entry(id); const existing = this.receipt(id);
      if (!current || !TERMINAL_STATES.has(current.state) || !existing) throw new Error('terminal receipt update requires a terminal request');
      this.#database.prepare('UPDATE terminal_receipts SET receipt = ? WHERE request_id = ? AND state = ?').run(encode(receipt), id, current.state);
      return this.receipt(id).receipt;
    });
  }

  entries({ states = null } = {}) {
    if (states !== null && (!Array.isArray(states) || states.some((candidate) => !LIFECYCLE_STATES.has(candidate)))) throw new TypeError('states must contain recognized lifecycle states');
    const rows = states === null
      ? this.#database.prepare('SELECT id, state, data, created_at, updated_at, terminal_at FROM requests ORDER BY created_at, id').all()
      : states.length === 0 ? [] : this.#database.prepare(`SELECT id, state, data, created_at, updated_at, terminal_at FROM requests WHERE state IN (${states.map(() => '?').join(', ')}) ORDER BY created_at, id`).all(...states);
    return Object.freeze(rows.map(rowToEntry));
  }

  /** Persists a namespace generation. A newer generation clears stale trash & leases. */
  upsertNamespace({ namespace, generation: nextGeneration, state: nextState, terminalAt = null, lruAt = null, repositoryIdentity = null }) {
    namespaceName(namespace); generation(nextGeneration); namespaceState(nextState); optionalTimestamp(terminalAt, 'terminalAt'); optionalTimestamp(lruAt, 'lruAt');
    if (repositoryIdentity !== null && typeof repositoryIdentity !== 'string') throw new TypeError('repositoryIdentity must be a string or null');
    // Last writer wins: the owning app of a namespace only changes when the
    // repository behind it changes, and the newest job knows it best.
    const identity = repositoryIdentity;
    return this.#transaction(() => {
      const current = this.getNamespace({ namespace });
      if (!current) {
        this.#database.prepare('INSERT INTO namespaces (namespace, generation, state, terminal_at, lru_at, repository_identity) VALUES (?, ?, ?, ?, ?, ?)').run(namespace, nextGeneration, nextState, terminalAt, lruAt, identity);
      } else if (nextGeneration < current.generation) {
        throw new Error('stale namespace generation');
      } else if (nextGeneration > current.generation) {
        this.#database.prepare('UPDATE namespaces SET generation = ?, state = ?, terminal_at = ?, lru_at = ?, repository_identity = ?, trash_id = NULL, trashed_at = NULL, deleted_at = NULL, maintenance_lease_id = NULL, maintenance_lease_at = NULL WHERE namespace = ?').run(nextGeneration, nextState, terminalAt, lruAt, identity ?? current.repositoryIdentity, namespace);
      } else {
        if (['TRASHED', 'DELETED'].includes(current.state)) throw new Error('trashed namespace requires a newer generation');
        if (!MAINTENANCE_TERMINAL_STATES.has(nextState) && this.#namespaceHasMaintenanceLease(namespace, nextGeneration)) throw new Error('active namespace cannot replace a maintenance lease without a newer generation');
        this.#database.prepare('UPDATE namespaces SET state = ?, terminal_at = ?, lru_at = ?, repository_identity = ? WHERE namespace = ?').run(nextState, terminalAt, lruAt, identity ?? current.repositoryIdentity, namespace);
      }
      return this.getNamespace({ namespace });
    });
  }

  getNamespace({ namespace }) {
    namespaceName(namespace);
    const row = this.#database.prepare('SELECT namespace, generation, state, terminal_at, lru_at, trash_id, trashed_at, deleted_at, repository_identity FROM namespaces WHERE namespace = ?').get(namespace);
    return row ? rowToNamespace(row) : null;
  }

  /**
   * Atomically claims a namespace for admission before its directory exists.
   * A GC lease or pre-rename journal is a hard stop: callers must not recreate
   * source while maintenance may still rename it. Committed old-generation
   * trash is independent, so deletion may complete after its successor starts.
   */
  activateNamespace({ namespace, occurredAt = new Date().toISOString(), repositoryIdentity = null }) {
    namespaceName(namespace); timestamp(occurredAt);
    if (repositoryIdentity !== null && typeof repositoryIdentity !== 'string') throw new TypeError('repositoryIdentity must be a string or null');
    return this.#transaction(() => {
      const unresolved = this.#database.prepare("SELECT 1 AS present FROM pending_trash_intents WHERE namespace = ? AND state = 'PENDING' LIMIT 1").get(namespace);
      if (unresolved?.present === 1) throw Object.assign(new Error('namespace activation waits for pending trash reconciliation'), { code: 'RK_NAMESPACE_GC_PENDING' });
      const current = this.getNamespace({ namespace });
      if (!current) {
        this.#database.prepare("INSERT INTO namespaces (namespace, generation, state, repository_identity) VALUES (?, 0, 'ACTIVE', ?)").run(namespace, repositoryIdentity);
        return this.getNamespace({ namespace });
      }
      if (this.#namespaceHasMaintenanceLease(namespace, current.generation)) {
        throw Object.assign(new Error('namespace activation waits for maintenance lease'), { code: 'RK_NAMESPACE_GC_LEASED' });
      }
      if (['TRASHED', 'DELETED'].includes(current.state)) {
        this.#database.prepare("UPDATE namespaces SET generation = ?, state = 'ACTIVE', terminal_at = NULL, lru_at = NULL, repository_identity = ?, trash_id = NULL, trashed_at = NULL, deleted_at = NULL WHERE namespace = ? AND generation = ?").run(current.generation + 1, repositoryIdentity ?? current.repositoryIdentity, namespace, current.generation);
      } else {
        this.#database.prepare("UPDATE namespaces SET state = 'ACTIVE', terminal_at = NULL, lru_at = NULL, repository_identity = ? WHERE namespace = ? AND generation = ?").run(repositoryIdentity ?? current.repositoryIdentity, namespace, current.generation);
      }
      return this.getNamespace({ namespace });
    });
  }

  claimMaintenanceLease({ namespace, generation: expectedGeneration }) {
    namespaceName(namespace); generation(expectedGeneration);
    return this.#transaction(() => {
      const current = this.getNamespace({ namespace });
      if (!current || current.generation !== expectedGeneration || !MAINTENANCE_TERMINAL_STATES.has(current.state)) return null;
      const leaseId = randomUUID();
      const result = this.#database.prepare('UPDATE namespaces SET maintenance_lease_id = ?, maintenance_lease_at = ? WHERE namespace = ? AND generation = ? AND maintenance_lease_id IS NULL').run(leaseId, new Date().toISOString(), namespace, expectedGeneration);
      return result.changes === 1 ? Object.freeze({ leaseId }) : null;
    });
  }

  releaseMaintenanceLease({ namespace, generation: expectedGeneration, leaseId }) {
    namespaceName(namespace); generation(expectedGeneration); requiredString(leaseId, 'leaseId');
    this.#transaction(() => {
      this.#database.prepare('UPDATE namespaces SET maintenance_lease_id = NULL, maintenance_lease_at = NULL WHERE namespace = ? AND generation = ? AND maintenance_lease_id = ?').run(namespace, expectedGeneration, leaseId);
    });
  }

  markNamespaceTrashed({ namespace, generation: expectedGeneration, leaseId, trashId, trashedAt }) {
    namespaceName(namespace); generation(expectedGeneration); requiredString(leaseId, 'leaseId'); requiredString(trashId, 'trashId'); timestamp(trashedAt);
    this.#transaction(() => {
      const current = this.getNamespace({ namespace });
      if (!current || current.generation !== expectedGeneration) throw new Error('stale namespace generation');
      if (current.state === 'TRASHED' && current.trashId === trashId) return;
      if (!MAINTENANCE_TERMINAL_STATES.has(current.state) || !this.#leaseMatches(namespace, expectedGeneration, leaseId)) throw new Error('maintenance lease is not current');
      const result = this.#database.prepare("UPDATE namespaces SET state = 'TRASHED', trash_id = ?, trashed_at = ? WHERE namespace = ? AND generation = ? AND maintenance_lease_id = ?").run(trashId, trashedAt, namespace, expectedGeneration, leaseId);
      if (result.changes !== 1) throw new Error('maintenance lease is not current');
      this.#database.prepare('INSERT OR REPLACE INTO maintenance_queue (namespace, generation, trash_id, cursor, state, updated_at) VALUES (?, ?, ?, NULL, \'PENDING\', ?)').run(namespace, expectedGeneration, trashId, trashedAt);
    });
  }

  markTrashDeleted({ namespace, generation: expectedGeneration, trashId, deletedAt }) {
    namespaceName(namespace); generation(expectedGeneration); requiredString(trashId, 'trashId'); timestamp(deletedAt);
    this.#transaction(() => {
      const current = this.getNamespace({ namespace });
      const queue = this.#database.prepare('SELECT state FROM maintenance_queue WHERE namespace = ? AND generation = ? AND trash_id = ?').get(namespace, expectedGeneration, trashId);
      if (!queue) throw new Error('trash deletion requires durable maintenance queue');
      if (queue.state === 'DONE') return;
      if (!current || current.generation < expectedGeneration) throw new Error('stale namespace generation');
      if (current.generation === expectedGeneration) {
        if (current.state === 'DELETED' && current.trashId === trashId) return;
        if (current.state !== 'TRASHED' || current.trashId !== trashId) throw new Error('trash deletion requires matching durable namespace state');
        this.#database.prepare("UPDATE namespaces SET state = 'DELETED', deleted_at = ? WHERE namespace = ? AND generation = ? AND trash_id = ?").run(deletedAt, namespace, expectedGeneration, trashId);
      }
      this.#database.prepare('UPDATE maintenance_queue SET state = \'DONE\', updated_at = ?, cursor = NULL WHERE namespace = ? AND generation = ? AND trash_id = ?').run(deletedAt, namespace, expectedGeneration, trashId);
      this.#database.prepare("UPDATE pending_trash_intents SET state = 'DELETED', updated_at = ? WHERE namespace = ? AND generation = ? AND trash_id = ? AND state = 'COMMITTED'").run(deletedAt, namespace, expectedGeneration, trashId);
    });
  }

  pendingMaintenance({ limit = 128 } = {}) {
    if (!Number.isSafeInteger(limit) || limit < 1) throw new TypeError('positive maintenance limit is required');
    return Object.freeze(this.#database.prepare("SELECT namespace, generation, trash_id AS trashId, cursor FROM maintenance_queue WHERE state = 'PENDING' ORDER BY updated_at LIMIT ?").all(limit).map((row) => Object.freeze(row)));
  }

  /** Authoritative old-generation trash state; independent of current namespace generation. */
  maintenanceEntry({ namespace, generation: expectedGeneration, trashId }) {
    namespaceName(namespace); generation(expectedGeneration); requiredString(trashId, 'trashId');
    const row = this.#database.prepare('SELECT namespace, generation, trash_id AS trashId, cursor, state, updated_at AS updatedAt FROM maintenance_queue WHERE namespace = ? AND generation = ? AND trash_id = ?').get(namespace, expectedGeneration, trashId);
    return row ? Object.freeze(row) : null;
  }

  updateMaintenanceCursor({ namespace, generation: expectedGeneration, trashId, cursor }) {
    this.#database.prepare("UPDATE maintenance_queue SET cursor = ?, updated_at = ? WHERE namespace = ? AND generation = ? AND trash_id = ? AND state = 'PENDING'").run(cursor ?? null, new Date().toISOString(), namespace, expectedGeneration, trashId);
  }

  prepareNamespaceTrash({ namespace, generation: expectedGeneration, leaseId, trashId, sourcePath, targetPath, occurredAt = new Date().toISOString() }) {
    namespaceName(namespace); generation(expectedGeneration); requiredString(leaseId, 'leaseId'); requiredString(trashId, 'trashId'); requiredString(sourcePath, 'sourcePath'); requiredString(targetPath, 'targetPath'); timestamp(occurredAt);
    return this.#transaction(() => { const current = this.getNamespace({ namespace }); if (!current || current.generation !== expectedGeneration || !MAINTENANCE_TERMINAL_STATES.has(current.state) || !this.#leaseMatches(namespace, expectedGeneration, leaseId)) throw new Error('maintenance lease is not current'); this.#database.prepare('INSERT OR REPLACE INTO pending_trash_intents (namespace,generation,trash_id,lease_id,source_path,target_path,state,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?)').run(namespace, expectedGeneration, trashId, leaseId, sourcePath, targetPath, 'PENDING', occurredAt, occurredAt); return Object.freeze({ namespace, generation: expectedGeneration, leaseId, trashId, sourcePath, targetPath, state: 'PENDING' }); });
  }
  pendingTrashIntents({ limit = 128 } = {}) { if (!Number.isSafeInteger(limit) || limit < 1) throw new TypeError('positive intent limit is required'); return Object.freeze(this.#database.prepare("SELECT namespace,generation,trash_id AS trashId,lease_id AS leaseId,source_path AS sourcePath,target_path AS targetPath,state FROM pending_trash_intents WHERE state='PENDING' ORDER BY updated_at LIMIT ?").all(limit).map((row) => Object.freeze(row))); }
  commitNamespaceTrash({ namespace, generation: expectedGeneration, trashId, occurredAt = new Date().toISOString() }) {
    namespaceName(namespace); generation(expectedGeneration); requiredString(trashId, 'trashId'); timestamp(occurredAt);
    return this.#transaction(() => { const intent = this.#database.prepare('SELECT lease_id AS leaseId,source_path AS sourcePath,target_path AS targetPath FROM pending_trash_intents WHERE namespace=? AND generation=? AND trash_id=? AND state=\'PENDING\'').get(namespace, expectedGeneration, trashId); if (!intent) throw new Error('trash intent not found'); if (existsSync(intent.sourcePath) === existsSync(intent.targetPath)) throw Object.assign(new Error('ambiguous trash filesystem state'), { code: 'RK_TRASH_AMBIGUOUS' }); if (existsSync(intent.sourcePath)) throw Object.assign(new Error('trash rename has not completed'), { code: 'RK_TRASH_PRE_RENAME' }); const current = this.getNamespace({ namespace }); if (!current || current.generation !== expectedGeneration || !MAINTENANCE_TERMINAL_STATES.has(current.state) || !this.#leaseMatches(namespace, expectedGeneration, intent.leaseId)) throw new Error('maintenance lease is not current'); this.#database.prepare("UPDATE namespaces SET state='TRASHED',trash_id=?,trashed_at=?,maintenance_lease_id=NULL,maintenance_lease_at=NULL WHERE namespace=? AND generation=? AND maintenance_lease_id=?").run(trashId, occurredAt, namespace, expectedGeneration, intent.leaseId); this.#database.prepare('INSERT OR REPLACE INTO maintenance_queue VALUES (?,?,?,NULL,\'PENDING\',?)').run(namespace, expectedGeneration, trashId, occurredAt); this.#database.prepare('UPDATE pending_trash_intents SET state=\'COMMITTED\',updated_at=? WHERE namespace=? AND generation=? AND trash_id=?').run(occurredAt, namespace, expectedGeneration, trashId); return this.getNamespace({ namespace }); });
  }
  finalizeNamespaceTrash(args) { return this.commitNamespaceTrash(args); }
  clearNamespaceTrashIntent({ namespace, generation: expectedGeneration, trashId, occurredAt = new Date().toISOString() }) { namespaceName(namespace); generation(expectedGeneration); requiredString(trashId, 'trashId'); timestamp(occurredAt); return this.#transaction(() => { const intent = this.#database.prepare('SELECT lease_id AS leaseId,source_path AS sourcePath,target_path AS targetPath FROM pending_trash_intents WHERE namespace=? AND generation=? AND trash_id=? AND state=\'PENDING\'').get(namespace, expectedGeneration, trashId); if (!intent) return false; if (!existsSync(intent.sourcePath) || existsSync(intent.targetPath)) throw new Error('cannot clear trash intent before source is restored'); this.#database.prepare('DELETE FROM pending_trash_intents WHERE namespace=? AND generation=? AND trash_id=?').run(namespace, expectedGeneration, trashId); this.#database.prepare('UPDATE namespaces SET maintenance_lease_id=NULL,maintenance_lease_at=NULL WHERE namespace=? AND generation=? AND maintenance_lease_id=?').run(namespace, expectedGeneration, intent.leaseId); return true; }); }

  reserve(input, domain, capacity, amount, occurredAt = new Date().toISOString()) { if (typeof input === 'object') { const value = input; return this.#reserve({ requestId: value.requestId, domain: value.domain, capacity: value.capacity, amount: value.amount, occurredAt: value.occurredAt ?? new Date().toISOString() }); } return this.#reserve({ requestId: input, domain, capacity, amount, occurredAt }); }
  reserveRequest({ requestId, domain, capacity, amount, occurredAt = new Date().toISOString() }) { return this.#reserve({ requestId, domain, capacity, amount, occurredAt }); }
  #reserve({ requestId, domain, capacity, amount, occurredAt }) {
    requiredString(requestId, 'requestId'); requiredString(domain, 'domain'); if (![capacity, amount].every((v) => Number.isSafeInteger(v) && v >= 0)) throw new TypeError('reservation values must be non-negative safe integers'); timestamp(occurredAt);
    return this.#transaction(() => { const prior = this.#database.prepare('SELECT amount FROM reservations WHERE request_id = ? AND domain = ?').get(requestId, domain); if (prior) return Object.freeze({ requestId, domain, amount: prior.amount, idempotent: true }); const row = this.#database.prepare('SELECT capacity,reserved FROM reservation_ledger WHERE domain = ?').get(domain); const current = row ?? { capacity, reserved: 0 }; if (row && current.capacity !== capacity) throw new Error('reservation capacity mismatch'); if (current.reserved + amount > capacity) throw Object.assign(new Error('reservation capacity overcommit'), { code: 'RK_RESERVATION_OVERCOMMIT' }); this.#database.prepare('INSERT INTO reservations VALUES (?,?,?,?,?)').run(requestId, domain, amount, occurredAt, occurredAt); this.#database.prepare('INSERT OR REPLACE INTO reservation_ledger VALUES (?,?,?,?)').run(domain, capacity, current.reserved + amount, occurredAt); return Object.freeze({ requestId, domain, capacity, amount, reserved: current.reserved + amount, idempotent: false }); });
  }
  reservationTotals({ domain = null } = {}) { const rows = domain ? this.#database.prepare('SELECT domain,SUM(amount) AS reserved FROM reservations WHERE domain=? GROUP BY domain').all(domain) : this.#database.prepare('SELECT domain,SUM(amount) AS reserved FROM reservations GROUP BY domain').all(); return Object.freeze(rows.map((row) => Object.freeze({ domain: row.domain, reserved: row.reserved }))); }

  releaseReservation({ requestId = null, domain = null, amount = null, occurredAt = new Date().toISOString() }) { requiredString(requestId, 'requestId'); timestamp(occurredAt); return this.#transaction(() => { const rows = this.#database.prepare('SELECT domain,amount FROM reservations WHERE request_id=?').all(requestId); if (!rows.length) return Object.freeze({ requestId, released: [], idempotent: true }); this.#database.prepare('DELETE FROM reservations WHERE request_id=?').run(requestId); for (const row of rows) this.#rebuildLedger(row.domain, occurredAt); return Object.freeze({ requestId, released: rows.map((row) => Object.freeze({ domain: row.domain, amount: row.amount })), idempotent: false }); }); }
  #rebuildLedger(domain, occurredAt) { const row = this.#database.prepare('SELECT capacity FROM reservation_ledger WHERE domain=?').get(domain); if (!row) return; const sum = this.#database.prepare('SELECT COALESCE(SUM(amount),0) AS reserved FROM reservations WHERE domain=?').get(domain).reserved; this.#database.prepare('UPDATE reservation_ledger SET reserved=?,updated_at=? WHERE domain=?').run(sum, occurredAt, domain); }

  create({ id, data = {}, occurredAt = new Date().toISOString() }) {
    requiredString(id, 'id'); object(data, 'data'); timestamp(occurredAt);
    const tx = () => this.#transaction(() => {
      if (this.entry(id)) throw new Error('request id already exists');
      this.#database.prepare('INSERT INTO requests (id, state, data, created_at, updated_at) VALUES (?, ?, ?, ?, ?)').run(id, 'SUBMITTED', encode(data), occurredAt, occurredAt);
      this.#database.prepare('INSERT INTO lifecycle_transitions (request_id, from_state, to_state, occurred_at, data) VALUES (?, ?, ?, ?, ?)').run(id, null, 'SUBMITTED', occurredAt, encode(data));
      return this.entry(id);
    });
    return tx();
  }

  transition({ id, from, to, data = {}, occurredAt = new Date().toISOString(), receipt = null }) {
    requiredString(id, 'id'); state(from); state(to); object(data, 'data'); timestamp(occurredAt);
    if (TERMINAL_STATES.has(from)) throw new Error('terminal requests cannot transition');
    if (to === 'SUBMITTED') throw new Error('SUBMITTED may only be created');
    if (receipt !== null && !TERMINAL_STATES.has(to)) throw new TypeError('only terminal transitions may create a receipt');
    if (TERMINAL_STATES.has(to) && receipt === null) throw new TypeError('terminal transition requires a receipt');
    if (receipt !== null) object(receipt, 'receipt');
    if (!TERMINAL_STATES.has(to) && NEXT_OPEN_STATE.get(from) !== to) throw new Error(`invalid lifecycle transition from ${from} to ${to}`);
    if (to === 'SUCCEEDED' && from !== 'REAPING') throw new Error('SUCCEEDED requires REAPING');
    if (to === 'TIMED_OUT' && !['RUNNING', 'REAPING'].includes(from)) throw new Error('TIMED_OUT requires RUNNING or REAPING');
    const tx = () => this.#transaction(() => this.#transition({ id, from, to, data, occurredAt, receipt }));
    return tx();
  }

  /**
   * Recovery intentionally terminalizes only work that has not been admitted.
   * STARTING/RUNNING/REAPING retain persisted `data.ownership` until the
   * integration owner reports an explicit reaping result via recoverOpenRequest.
   */
  recoverOpenRequests({ occurredAt = new Date().toISOString(), receiptFor = null } = {}) {
    timestamp(occurredAt);
    if (receiptFor !== null && typeof receiptFor !== 'function') throw new TypeError('receiptFor must be a function');
    const tx = () => this.#transaction(() => {
      const recovered = [];
      for (const entry of this.entries({ states: [...RECOVERABLE_QUEUED_STATES] })) {
        const receipt = receiptFor ? receiptFor(entry) : { ...entry.data, id: entry.id, state: 'CANCELLED', cancellationReason: 'broker-restart', recoveredFrom: entry.state, occurredAt };
        object(receipt, 'recovery receipt');
        recovered.push(this.#transition({ id: entry.id, from: entry.state, to: 'CANCELLED', data: { ...entry.data, cancellationReason: 'broker-restart', recoveredFrom: entry.state }, occurredAt, receipt }));
      }
      return Object.freeze(recovered);
    });
    return tx();
  }

  recoverOpenRequest({ id, state: terminalState = 'CANCELLED', data = {}, receipt = null, occurredAt = new Date().toISOString() }) {
    requiredString(id, 'id'); state(terminalState); object(data, 'data'); timestamp(occurredAt);
    if (!TERMINAL_STATES.has(terminalState)) throw new TypeError('recovery requires a terminal state');
    const current = this.entry(id);
    if (!current) throw new Error('request not found');
    if (TERMINAL_STATES.has(current.state)) {
      const existing = this.receipt(id);
      if (existing && current.state === terminalState) return Object.freeze({ entry: current, receipt: existing.receipt, idempotent: true });
      throw new Error('request is already terminal');
    }
    const recoveredReceipt = receipt ?? { ...current.data, ...data, id, state: terminalState, recoveredFrom: current.state, occurredAt };
    object(recoveredReceipt, 'recovery receipt');
    return this.transition({ id, from: current.state, to: terminalState, data: { ...data, recoveredFrom: current.state }, receipt: recoveredReceipt, occurredAt });
  }

  #transition({ id, from, to, data, occurredAt, receipt }) {
    const current = this.entry(id);
    if (!current) throw new Error('request not found');
    if (current.state !== from) {
      const existing = this.receipt(id);
      if (TERMINAL_STATES.has(to) && existing && current.state === to) return Object.freeze({ entry: current, receipt: existing.receipt, idempotent: true });
      throw new Error(`expected ${from} but request is ${current.state}`);
    }
    const nextData = { ...current.data, ...data };
    const terminalAt = TERMINAL_STATES.has(to) ? occurredAt : null;
    this.#database.prepare('UPDATE requests SET state = ?, data = ?, updated_at = ?, terminal_at = ? WHERE id = ?').run(to, encode(nextData), occurredAt, terminalAt, id);
    this.#database.prepare('INSERT INTO lifecycle_transitions (request_id, from_state, to_state, occurred_at, data) VALUES (?, ?, ?, ?, ?)').run(id, from, to, occurredAt, encode(data));
    let terminalReceipt = null;
    if (receipt !== null) {
      this.#database.prepare('INSERT INTO terminal_receipts (request_id, state, receipt, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(request_id) DO NOTHING').run(id, to, encode(receipt), occurredAt);
      terminalReceipt = this.receipt(id)?.receipt ?? null;
    }
    return Object.freeze({ entry: this.entry(id), receipt: terminalReceipt, idempotent: false });
  }

  #leaseMatches(namespace, expectedGeneration, leaseId) {
    return this.#database.prepare('SELECT 1 AS present FROM namespaces WHERE namespace = ? AND generation = ? AND maintenance_lease_id = ?').get(namespace, expectedGeneration, leaseId)?.present === 1;
  }

  #namespaceHasMaintenanceLease(namespace, expectedGeneration) {
    return this.#database.prepare('SELECT 1 AS present FROM namespaces WHERE namespace = ? AND generation = ? AND maintenance_lease_id IS NOT NULL').get(namespace, expectedGeneration)?.present === 1;
  }

  #reconcileReservations() {
    this.#transaction(() => {
      this.#database.prepare("DELETE FROM reservations WHERE request_id NOT IN (SELECT id FROM requests) OR request_id IN (SELECT id FROM requests WHERE state IN ('SUCCEEDED','FAILED','CANCELLED','TIMED_OUT'))").run();
      for (const row of this.#database.prepare('SELECT domain FROM reservation_ledger').all()) this.#rebuildLedger(row.domain, new Date().toISOString());
    });
  }

  #transaction(work) {
    this.#database.exec('BEGIN IMMEDIATE');
    try {
      const result = work();
      this.#database.exec('COMMIT');
      return result;
    } catch (error) {
      this.#database.exec('ROLLBACK');
      throw error;
    }
  }
}

export const BUILD_LIFECYCLE = Object.freeze({
  open: Object.freeze([...OPEN_STATES]),
  terminal: Object.freeze([...TERMINAL_STATES]),
});
