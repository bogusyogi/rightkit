import { randomUUID } from 'node:crypto';
import { lstat as nodeLstat, mkdir as nodeMkdir, opendir as nodeOpendir, rename as nodeRename, rmdir as nodeRmdir, unlink as nodeUnlink } from 'node:fs/promises';
import { dirname, isAbsolute, join, normalize, relative } from 'node:path';

const namespaceName = /^[a-f0-9]{64}$/;
const trashName = /^[a-f0-9-]{37,160}$/;
const terminalStates = new Set(['RELEASED', 'SUCCEEDED', 'FAILED', 'CANCELLED', 'TIMED_OUT']);
const maxCursorBytes = 4096;
const maxSizeCacheEntries = 4096;

/**
 * Minimal durable-state seam. Each read returns an immutable namespace snapshot:
 * `{ namespace, generation, state, terminalAt?, lruAt?, trashId? }`.
 * Claims are compare-and-swap operations and return `{ leaseId }` or `null`.
 * A successful claim must block activation of that namespace generation until release.
 * The implementation must make `markNamespaceTrashed` and `markTrashDeleted`
 * idempotent for the supplied namespace generation.
 */
export const MAINTENANCE_STATE_STORE_CONTRACT = Object.freeze({
  getNamespace: '({ namespace }) -> snapshot | null',
  claimMaintenanceLease: '({ namespace, generation }) -> { leaseId } | null; atomically rejects nonterminal records & blocks same-generation activation until release',
  releaseMaintenanceLease: '({ namespace, generation, leaseId }) -> void',
  markNamespaceTrashed: '({ namespace, generation, leaseId, trashId, trashedAt }) -> void',
  prepareNamespaceTrash: '({ namespace, generation, leaseId, trashId, sourcePath, targetPath }) -> intent',
  commitNamespaceTrash: '({ namespace, generation, trashId }) -> snapshot',
  clearNamespaceTrashIntent: '({ namespace, generation, trashId }) -> void',
  pendingTrashIntents: '({ limit }) -> intents',
  maintenanceEntry: '({ namespace, generation, trashId }) -> { state, cursor } | null; authoritative even after a newer namespace generation',
  markTrashDeleted: '({ namespace, generation, trashId, deletedAt }) -> void'
});

/** Background-only namespace inventory, retention, & trash deletion.
 * It intentionally has no broker, scheduler, or admission dependency. */
export class MaintenanceWorker {
  #workspacesRoot; #trashRoot; #stateStore; #operations; #now; #maxBatchEntries; #maxDepth; #inventoryCursor = null; #sizeCache = new Map();

  constructor({ workspacesRoot, trashRoot, stateStore, operations = {}, now = Date.now, maxBatchEntries = 128, maxDepth = 64 } = {}) {
    if (!isAbsolute(workspacesRoot) || !isAbsolute(trashRoot)) throw new TypeError('absolute workspacesRoot & trashRoot are required');
    if (!contained(dirname(workspacesRoot), trashRoot)) throw new TypeError('trashRoot must remain below managed storage');
    if (!stateStore || typeof stateStore !== 'object' || ['getNamespace', 'claimMaintenanceLease', 'releaseMaintenanceLease', 'markTrashDeleted'].some((name) => typeof stateStore[name] !== 'function') || (!(typeof stateStore.prepareNamespaceTrash === 'function' && typeof stateStore.commitNamespaceTrash === 'function') && typeof stateStore.markNamespaceTrashed !== 'function')) throw new TypeError('durable maintenance state-store contract is required');
    if (typeof now !== 'function' || !Number.isSafeInteger(maxBatchEntries) || maxBatchEntries < 1 || !Number.isSafeInteger(maxDepth) || maxDepth < 1) throw new TypeError('valid maintenance bounds are required');
    this.#workspacesRoot = normalize(workspacesRoot); this.#trashRoot = normalize(trashRoot); this.#stateStore = stateStore; this.#now = now;
    this.#operations = Object.freeze({ lstat: operations.lstat ?? nodeLstat, mkdir: operations.mkdir ?? nodeMkdir, opendir: operations.opendir ?? nodeOpendir, rename: operations.rename ?? nodeRename, rmdir: operations.rmdir ?? nodeRmdir, unlink: operations.unlink ?? nodeUnlink });
    if (Object.values(this.#operations).some((operation) => typeof operation !== 'function')) throw new TypeError('maintenance filesystem operations must be functions');
    this.#maxBatchEntries = maxBatchEntries; this.#maxDepth = maxDepth;
  }

  /** Bounded, directory-driven inventory. Unknown namespace directories remain dry-run findings. */
  async inventory({ limit = this.#maxBatchEntries, cursor = null } = {}) {
    assertLimit(limit, 'inventory limit'); assertCursor(cursor);
    const listed = await this.#listDirectories(this.#workspacesRoot, limit, cursor); const entries = [];
    for (const item of listed.entries) {
      const namespace = item.name;
      if (!namespaceName.test(namespace)) { entries.push(Object.freeze({ namespace, path: item.path, status: 'PROTECTED_INVALID_NAME' })); continue; }
      if (!item.directory || item.symbolicLink) { entries.push(Object.freeze({ namespace, path: item.path, status: 'PROTECTED_NON_DIRECTORY' })); continue; }
      const record = await this.#stateStore.getNamespace({ namespace });
      entries.push(Object.freeze({ namespace, path: item.path, status: record ? 'KNOWN' : 'ORPHAN', record: record ? normalizeRecord(record, namespace) : null }));
    }
    return Object.freeze({ entries: Object.freeze(entries), truncated: listed.truncated, cursor: listed.nextCursor, nextCursor: listed.nextCursor });
  }

  /** Lists unknown directories only; it never changes disk or durable state. */
  async orphanDryRun(options = {}) {
    const inventory = await this.inventory(options);
    return Object.freeze({ mode: 'dry-run', orphans: Object.freeze(inventory.entries.filter((entry) => entry.status === 'ORPHAN')), truncated: inventory.truncated, cursor: inventory.cursor, nextCursor: inventory.nextCursor });
  }

  /** Moves retention-eligible known namespaces to trash. Physical deletion is separate. */
  async retentionBatch({ expiredBefore, maxNamespaces = this.#maxBatchEntries, inventoryLimit = this.#maxBatchEntries, inventoryCursor = null, cursor = null } = {}) {
    const before = validTimestamp(expiredBefore, 'expiredBefore'); assertLimit(maxNamespaces, 'maxNamespaces');
    const pageCursor = inventoryCursor ?? cursor;
    const inventory = await this.inventory({ limit: inventoryLimit, cursor: pageCursor }); const moved = []; const skipped = [];
    for (const entry of inventory.entries) {
      if (moved.length >= maxNamespaces) break;
      if (entry.status !== 'KNOWN' || !eligibleForRetention(entry.record, before)) continue;
      const outcome = await this.moveToTrash({ namespace: entry.namespace, generation: entry.record.generation, expiredBefore: before });
      if (outcome.status === 'TRASHED') moved.push(outcome); else skipped.push(outcome);
    }
    return Object.freeze({ moved: Object.freeze(moved), skipped: Object.freeze(skipped), inventoryTruncated: inventory.truncated, inventoryCursor: inventory.nextCursor, nextCursor: inventory.nextCursor });
  }

  /**
   * Bounded on-disk measurement of one namespace root.
   *
   * A terminal namespace is immutable for its generation, so its measurement is
   * cached under `namespace:generation` and dropped when the root is trashed.
   * A walk that exhausts its entry budget reports `complete: false` and
   * undercounts, which can only make cap enforcement more conservative.
   */
  async measureNamespace({ namespace, generation = null, cacheable = false, maxEntries = 1_000_000 } = {}) {
    assertNamespace(namespace); assertLimit(maxEntries, 'maxEntries');
    const key = generation === null ? null : `${namespace}:${generation}`;
    if (key && cacheable && this.#sizeCache.has(key)) return this.#sizeCache.get(key);
    let bytes = 0; let scanned = 0; let complete = true; const stack = [join(this.#workspacesRoot, namespace)];
    while (stack.length && complete) {
      const current = stack.pop();
      const handle = await this.#operations.opendir(current, { bufferSize: 1 }).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
      if (!handle) continue;
      try {
        for await (const item of handle) {
          if (scanned >= maxEntries) { complete = false; break; }
          scanned += 1;
          const path = join(current, item.name);
          const metadata = await this.#operations.lstat(path).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
          if (!metadata || metadata.isSymbolicLink()) continue;
          if (metadata.isDirectory()) stack.push(path); else bytes += occupiedBytes(metadata);
        }
      } finally { await handle.close().catch(() => {}); }
    }
    const measurement = Object.freeze({ namespace, bytes, complete, scanned });
    // Bounded so a long-lived broker cannot accumulate one entry per retired
    // namespace generation for its whole lifetime.
    if (key && cacheable && complete) {
      if (this.#sizeCache.size >= maxSizeCacheEntries) this.#sizeCache.delete(this.#sizeCache.keys().next().value);
      this.#sizeCache.set(key, measurement);
    }
    return measurement;
  }

  /**
   * Size-driven LRU. Doc section 7 caps total target-cache bytes; age retention
   * alone leaves a full disk full until entries expire. Measures every root,
   * and while the total exceeds `capBytes` moves the least-recently-used
   * retention-eligible roots to trash. Non-terminal roots are never selected,
   * so a cap below the live working set under-reclaims rather than breaking a
   * running build.
   */
  // A truncated walk undercounts, which would silently stop the cap from ever
  // triggering, so the per-root budget is sized to complete real target trees;
  // terminal roots are measured once and cached.
  async capBatch({ capBytes, evictableBefore = null, maxNamespaces = this.#maxBatchEntries, maxNamespacesScanned = 4096, scanEntries = 1_000_000 } = {}) {
    assertLimit(capBytes, 'capBytes'); assertLimit(maxNamespaces, 'maxNamespaces'); assertLimit(maxNamespacesScanned, 'maxNamespacesScanned');
    // Without a floor the least-recently-used terminal root is the one just
    // built, so a workspace permanently over cap would evict its own warm
    // output every tick and defeat the point of a stable target root.
    const floor = evictableBefore === null ? null : validTimestamp(evictableBefore, 'evictableBefore');
    const candidates = []; let total = 0;
    for (const name of await this.#listAllNamespaces(maxNamespacesScanned)) {
      if (!namespaceName.test(name)) continue;
      const metadata = await this.#operations.lstat(join(this.#workspacesRoot, name)).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
      if (!metadata || !metadata.isDirectory() || metadata.isSymbolicLink()) continue;
      const stored = await this.#stateStore.getNamespace({ namespace: name });
      const record = stored ? normalizeRecord(stored, name) : null;
      const eligible = record !== null && eligibleForRetention(record, floor);
      const measurement = await this.measureNamespace({ namespace: name, generation: record?.generation ?? null, cacheable: eligible, maxEntries: scanEntries });
      total += measurement.bytes;
      if (eligible) candidates.push({ namespace: name, generation: record.generation, bytes: measurement.bytes, lruAt: record.lruAt ?? record.terminalAt });
    }
    if (total <= capBytes) return Object.freeze({ withinCap: true, totalBytes: total, projectedBytes: total, capBytes, reclaimedBytes: 0, moved: Object.freeze([]), skipped: Object.freeze([]) });
    candidates.sort((left, right) => left.lruAt.localeCompare(right.lruAt));
    const moved = []; const skipped = []; let projected = total; let reclaimed = 0;
    for (const candidate of candidates) {
      if (projected <= capBytes || moved.length >= maxNamespaces) break;
      // The root can vanish between measurement and eviction; that is the
      // outcome this pass wanted, not a reason to abort the tick.
      const outcome = await this.moveToTrash({ namespace: candidate.namespace, generation: candidate.generation })
        .catch((error) => error.code === 'ENOENT' ? Object.freeze({ status: 'SKIPPED_ALREADY_ABSENT', namespace: candidate.namespace, generation: candidate.generation }) : Promise.reject(error));
      if (outcome.status !== 'TRASHED') { skipped.push(outcome); continue; }
      this.#sizeCache.delete(`${candidate.namespace}:${candidate.generation}`);
      moved.push(Object.freeze({ ...outcome, bytes: candidate.bytes })); projected -= candidate.bytes; reclaimed += candidate.bytes;
    }
    return Object.freeze({ withinCap: false, totalBytes: total, projectedBytes: projected, capBytes, reclaimedBytes: reclaimed, moved: Object.freeze(moved), skipped: Object.freeze(skipped) });
  }

  /** Acquire, revalidate generation, then atomically rename a namespace into managed trash. */
  async moveToTrash({ namespace, generation, expiredBefore = null } = {}) {
    assertNamespace(namespace); assertGeneration(generation); if (expiredBefore !== null) validTimestamp(expiredBefore, 'expiredBefore'); const root = join(this.#workspacesRoot, namespace); await assertDirectory(root, this.#operations.lstat);
    const claim = await this.#stateStore.claimMaintenanceLease({ namespace, generation });
    if (!claim?.leaseId || typeof claim.leaseId !== 'string') return Object.freeze({ status: 'SKIPPED_UNCLAIMED', namespace, generation });
    let released = false;
    try {
      const current = await this.#stateStore.getNamespace({ namespace });
      if (!current || !sameGeneration(current, namespace, generation)) return Object.freeze({ status: 'SKIPPED_STALE_GENERATION', namespace, generation });
      if (!eligibleForRetention(normalizeRecord(current, namespace), expiredBefore)) return Object.freeze({ status: 'SKIPPED_NOT_RETENTION_ELIGIBLE', namespace, generation });
      await assertDirectory(root, this.#operations.lstat);
      await this.#operations.mkdir(this.#trashRoot, { recursive: true });
      const trashId = `${namespace}-${generation}-${randomUUID()}`; const target = join(this.#trashRoot, trashId);
      const journaled = typeof this.#stateStore.prepareNamespaceTrash === 'function';
      if (journaled) await this.#stateStore.prepareNamespaceTrash({ namespace, generation, leaseId: claim.leaseId, trashId, sourcePath: root, targetPath: target, occurredAt: new Date(this.#now()).toISOString() });
      await this.#operations.rename(root, target);
      if (journaled) await this.#stateStore.commitNamespaceTrash({ namespace, generation, trashId, occurredAt: new Date(this.#now()).toISOString() });
      else await this.#stateStore.markNamespaceTrashed({ namespace, generation, leaseId: claim.leaseId, trashId, trashedAt: new Date(this.#now()).toISOString() });
      released = true;
      return Object.freeze({ status: 'TRASHED', namespace, generation, trashId, path: target });
    } finally {
      if (!released) await this.#stateStore.releaseMaintenanceLease({ namespace, generation, leaseId: claim.leaseId });
    }
  }

  async reconcileTrashIntent(intent) {
    if (!intent || typeof intent !== 'object') throw new TypeError('trash intent is required');
    const { namespace, generation, trashId, sourcePath, targetPath, leaseId } = intent;
    assertNamespace(namespace); assertGeneration(generation);
    const source = join(this.#workspacesRoot, namespace); const target = join(this.#trashRoot, trashId);
    if (sourcePath !== source || targetPath !== target) throw Object.assign(new Error('trash intent path mismatch'), { code: 'RK_TRASH_AMBIGUOUS' });
    const sourceMeta = await this.#operations.lstat(source).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    const targetMeta = await this.#operations.lstat(target).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (sourceMeta?.isSymbolicLink?.() || targetMeta?.isSymbolicLink?.()) throw Object.assign(new Error('trash intent path cannot be symbolic link'), { code: 'RK_TRASH_AMBIGUOUS' });
    if (sourceMeta && targetMeta) throw Object.assign(new Error('ambiguous trash filesystem state'), { code: 'RK_TRASH_AMBIGUOUS' });
    if (sourceMeta) { await this.#stateStore.clearNamespaceTrashIntent({ namespace, generation, trashId }); if (leaseId) await this.#stateStore.releaseMaintenanceLease({ namespace, generation, leaseId }); return Object.freeze({ status: 'CLEARED', namespace, generation, trashId }); }
    if (targetMeta) { await this.#stateStore.commitNamespaceTrash({ namespace, generation, trashId, occurredAt: new Date(this.#now()).toISOString() }); return Object.freeze({ status: 'COMMITTED', namespace, generation, trashId }); }
    throw Object.assign(new Error('missing trash source & target'), { code: 'RK_TRASH_AMBIGUOUS' });
  }

  /**
   * Deletes one trash tree in bounded first-child DFS steps.
   *
   * Directory enumeration is deliberately not sorted or accumulated: a wide
   * directory must never turn one maintenance tick into an unbounded scan.
   * `cursor` contains only a bounded stack of directory names.  It is safe to
   * replay after a crash because every step reopens current directory & removes
   * its first remaining child; a vanished stack suffix is simply rewound.
   */
  async deleteTrashBatch({ namespace, generation, trashId, maxEntries = this.#maxBatchEntries, cursor = null } = {}) {
    assertNamespace(namespace); assertGeneration(generation); if (typeof trashId !== 'string' || !trashName.test(trashId) || !trashId.startsWith(`${namespace}-${generation}-`)) throw new TypeError('valid namespace-qualified trashId is required'); assertLimit(maxEntries, 'maxEntries');
    const queued = typeof this.#stateStore.maintenanceEntry === 'function'
      ? await this.#stateStore.maintenanceEntry({ namespace, generation, trashId })
      : null;
    if (queued) {
      if (queued.state !== 'PENDING') throw new Error('trash deletion requires pending durable maintenance queue');
    } else {
      // Compatibility for a pre-v2 state store. New stores must use their
      // maintenance queue so old trash remains deletable after activation.
      const record = await this.#stateStore.getNamespace({ namespace });
      if (!record || !sameGeneration(record, namespace, generation) || record.trashId !== trashId) throw new Error('trash deletion requires matching durable namespace state');
    }
    assertCursor(cursor);
    const root = join(this.#trashRoot, trashId); const metadata = await this.#operations.lstat(root).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (!metadata) { await this.#stateStore.markTrashDeleted({ namespace, generation, trashId, deletedAt: new Date(this.#now()).toISOString() }); return Object.freeze({ deletedEntries: 0, complete: true, cursor: null, nextCursor: null }); }
    if (!metadata.isDirectory() || metadata.isSymbolicLink()) throw new Error('trash root must be a real directory');
    const traversal = decodeTraversalCursor(cursor, this.#maxDepth); let deletedEntries = 0; let scannedEntries = 0;
    while (deletedEntries < maxEntries && scannedEntries < maxEntries) {
      const outcome = await deleteOneBounded(root, traversal, this.#operations, this.#maxDepth);
      scannedEntries += outcome.scannedEntries;
      deletedEntries += outcome.deletedEntries;
      if (outcome.complete) break;
      // A race can make an empty directory non-empty between `opendir` &
      // `rmdir`; yield rather than spin when no bounded progress was possible.
      if (outcome.scannedEntries === 0 && outcome.deletedEntries === 0) break;
    }
    const exists = await this.#operations.lstat(root).then(() => true).catch((error) => error.code === 'ENOENT' ? false : Promise.reject(error));
    if (!exists) await this.#stateStore.markTrashDeleted({ namespace, generation, trashId, deletedAt: new Date(this.#now()).toISOString() });
    const nextCursor = exists ? encodeTraversalCursor(traversal) : null;
    return Object.freeze({ deletedEntries, scannedEntries, complete: !exists, cursor: nextCursor, nextCursor });
  }

  /** Full bounded listing that owns its handle, so cap passes never disturb the
   * paginated inventory cursor shared by retention. */
  async #listAllNamespaces(limit) {
    const handle = await this.#operations.opendir(this.#workspacesRoot, { bufferSize: 1 }).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (!handle) return [];
    const names = [];
    try { for await (const item of handle) { if (names.length >= limit) break; names.push(item.name); } }
    finally { await handle.close().catch(() => {}); }
    return names;
  }

  async #listDirectories(root, limit, cursor = null) {
    let state;
    if (cursor === null) {
      if (this.#inventoryCursor?.handle) await this.#inventoryCursor.handle.close().catch(() => {});
      const handle = await this.#operations.opendir(root, { bufferSize: 1 }).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
      if (!handle) return Object.freeze({ entries: Object.freeze([]), truncated: false, nextCursor: null });
      state = { token: `inv:${randomUUID()}`, handle, pending: null }; this.#inventoryCursor = state;
    } else {
      if (!this.#inventoryCursor || this.#inventoryCursor.token !== cursor) throw Object.assign(new Error('inventory cursor expired; restart bounded inventory'), { code: 'RK_INVENTORY_CURSOR_EXPIRED' });
      state = this.#inventoryCursor;
    }
    const entries = []; let complete = false;
    try {
      while (entries.length < limit) {
        const item = state.pending ?? (await state.handle.read()); state.pending = null;
        if (!item) { complete = true; break; }
        const path = join(root, item.name);
        // A namespace trashed by a concurrent cap or retention pass is already
        // gone by the time this cursor reaches its name; skip it rather than
        // failing the whole tick.
        const metadata = await this.#operations.lstat(path).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
        if (!metadata) continue;
        entries.push({ name: item.name, path, directory: metadata.isDirectory(), symbolicLink: metadata.isSymbolicLink() });
      }
      if (!complete) { state.pending = await state.handle.read(); complete = state.pending === null; }
    } catch (error) { await state.handle.close().catch(() => {}); this.#inventoryCursor = null; throw error; }
    if (complete) { await state.handle.close().catch(() => {}); this.#inventoryCursor = null; }
    const nextCursor = complete ? null : state.token;
    return Object.freeze({ entries: Object.freeze(entries.map((entry) => Object.freeze(entry))), truncated: !complete, nextCursor });
  }
}

async function deleteOneBounded(root, traversal, operations, maxDepth) {
  await rewindTraversal(root, traversal, operations);
  const current = join(root, ...traversal.stack);
  const child = await firstDirectoryEntry(current, operations);
  if (child === null) {
    if (traversal.stack.length === 0) {
      const removed = await removeDirectory(current, operations);
      // Root removal finalizes this batch but is not a namespace payload entry.
      return Object.freeze({ scannedEntries: 0, deletedEntries: 0, complete: removed });
    }
    const removed = await removeDirectory(current, operations);
    if (removed) traversal.stack.pop();
    return Object.freeze({ scannedEntries: 0, deletedEntries: removed ? 1 : 0, complete: false });
  }
  const childPath = join(current, child.name);
  const metadata = await operations.lstat(childPath).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
  if (!metadata) return Object.freeze({ scannedEntries: 1, deletedEntries: 0, complete: false });
  if (metadata.isDirectory() && !metadata.isSymbolicLink()) {
    if (traversal.stack.length + 1 > maxDepth) throw new RangeError('trash tree exceeds maintenance depth bound');
    if (!traversalCursorFits([...traversal.stack, child.name])) throw new RangeError('trash traversal cursor exceeds maintenance bound');
    traversal.stack.push(child.name);
    return Object.freeze({ scannedEntries: 1, deletedEntries: 0, complete: false });
  }
  await operations.unlink(childPath);
  return Object.freeze({ scannedEntries: 1, deletedEntries: 1, complete: false });
}

async function rewindTraversal(root, traversal, operations) {
  for (let index = 0; index < traversal.stack.length; index += 1) {
    const path = join(root, ...traversal.stack.slice(0, index + 1));
    const metadata = await operations.lstat(path).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (!metadata) { traversal.stack.length = index; return; }
    if (!metadata.isDirectory() || metadata.isSymbolicLink()) throw new Error('trash traversal cursor points outside real directory tree');
  }
}

async function firstDirectoryEntry(path, operations) {
  // Node's Dir iterator has its own read buffer. One record prevents a wide
  // directory from being eagerly buffered behind our explicit scan budget.
  const handle = await operations.opendir(path, { bufferSize: 1 });
  try {
    const iterator = handle[Symbol.asyncIterator](); const next = await iterator.next();
    return next.done ? null : Object.freeze({ name: next.value.name });
  } finally { await handle.close().catch(() => {}); }
}

async function removeDirectory(path, operations) {
  try { await operations.rmdir(path); return true; } catch (error) { if (error.code === 'ENOTEMPTY' || error.code === 'ENOENT') return false; throw error; }
}

function decodeTraversalCursor(cursor, maxDepth) {
  if (cursor === null) return { stack: [] };
  // A pre-v2 cursor was an opaque path string. Resetting it is safe because
  // first-child traversal makes progress from root without trusting old order.
  let decoded;
  try { decoded = JSON.parse(cursor); } catch { return { stack: [] }; }
  if (!decoded || decoded.v !== 1 || !Array.isArray(decoded.stack) || decoded.stack.length > maxDepth || decoded.stack.some((item) => !validPathSegment(item))) throw new TypeError('valid bounded trash traversal cursor is required');
  return { stack: [...decoded.stack] };
}

const traversalCursorFits = (stack) => JSON.stringify({ v: 1, stack }).length <= maxCursorBytes;
const encodeTraversalCursor = (traversal) => {
  const cursor = JSON.stringify({ v: 1, stack: traversal.stack });
  if (cursor.length > maxCursorBytes) throw new RangeError('trash traversal cursor exceeds maintenance bound');
  return cursor;
};
const validPathSegment = (value) => typeof value === 'string' && value.length > 0 && value.length <= 255 && value !== '.' && value !== '..' && !value.includes('/') && !value.includes('\\');

// Allocated blocks match what the volume actually reclaims; apparent size is
// the fallback for stat implementations that omit block counts.
const occupiedBytes = (metadata) => Number.isSafeInteger(metadata.blocks) && metadata.blocks >= 0 ? metadata.blocks * 512 : (Number.isSafeInteger(metadata.size) ? metadata.size : 0);
const contained = (root, value) => { const diff = relative(normalize(root), normalize(value)); return diff === '' || (!diff.startsWith('..') && !isAbsolute(diff)); };
const assertLimit = (value, label) => { if (!Number.isSafeInteger(value) || value < 1) throw new TypeError(`${label} must be a positive safe integer`); };
const assertCursor = (value) => { if (value !== null && (typeof value !== 'string' || value.length > maxCursorBytes)) throw new TypeError('cursor must be null or a bounded string'); };
const assertNamespace = (value) => { if (typeof value !== 'string' || !namespaceName.test(value)) throw new TypeError('64-character hexadecimal namespace is required'); };
const assertGeneration = (value) => { if (!Number.isSafeInteger(value) || value < 0) throw new TypeError('non-negative safe namespace generation is required'); };
const validTimestamp = (value, label) => { if (typeof value !== 'string' || !Number.isFinite(Date.parse(value))) throw new TypeError(`${label} must be an ISO timestamp`); return value; };
const sameGeneration = (record, namespace, generation) => normalizeRecord(record, namespace).generation === generation;
function normalizeRecord(record, namespace) {
  if (!record || typeof record !== 'object' || record.namespace !== namespace) throw new TypeError('durable namespace record does not match requested namespace');
  assertGeneration(record.generation); if (typeof record.state !== 'string') throw new TypeError('durable namespace state is required');
  return Object.freeze({ ...record, namespace, generation: record.generation, state: record.state });
}
const eligibleForRetention = (record, before = null) => terminalStates.has(record.state) && typeof (record.lruAt ?? record.terminalAt) === 'string' && (before === null || (record.lruAt ?? record.terminalAt).localeCompare(before) <= 0);
async function assertDirectory(path, lstat) { const metadata = await lstat(path); if (!metadata.isDirectory() || metadata.isSymbolicLink()) throw new Error('maintenance namespace must be a real directory'); }
