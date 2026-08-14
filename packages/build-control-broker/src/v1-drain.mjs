import { createHash, randomUUID } from 'node:crypto';
import { mkdir, open, readFile, rename } from 'node:fs/promises';
import { dirname, isAbsolute, normalize } from 'node:path';

const V1_TERMINAL = new Set(['RELEASED', 'FAILED', 'CANCELLED']);
const V1_OPEN = new Set(['QUEUED', 'ACTIVE']);

const sha256 = (value) => createHash('sha256').update(value).digest('hex');
const requiredAbsolutePath = (value, field) => {
  if (typeof value !== 'string' || !isAbsolute(value)) throw new TypeError(`${field} must be an absolute path`);
  return normalize(value);
};
const requiredCallback = (value, field) => {
  if (typeof value !== 'function') throw new TypeError(`${field} callback is required`);
  return value;
};
const proven = (value, field) => value === true || value?.[field] === true;

async function durableReplace(destination, contents) {
  await mkdir(dirname(destination), { recursive: true });
  const temporary = `${destination}.${randomUUID()}.tmp`;
  const handle = await open(temporary, 'wx', 0o600);
  try { await handle.writeFile(contents, 'utf8'); await handle.sync(); } finally { await handle.close(); }
  await rename(temporary, destination);
  const directory = await open(dirname(destination), 'r');
  try { await directory.sync(); } catch (error) { if (!['EINVAL', 'ENOTSUP', 'EPERM'].includes(error.code)) throw error; } finally { await directory.close(); }
}

async function contentAddressedBackup(path, raw, digest) {
  const backup = `${path}.v1-pre-v2.${digest}.bak`;
  try {
    const existing = await readFile(backup, 'utf8');
    if (existing !== raw) throw Object.assign(new Error('v1 backup path contains different content'), { code: 'RK_V1_BACKUP_CONFLICT', backup });
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    await durableReplace(backup, raw);
  }
  return backup;
}

function parseLegacy(raw) {
  let value;
  try { value = JSON.parse(raw); } catch (cause) { throw Object.assign(new Error('legacy v1 state is not valid JSON'), { code: 'RK_V1_DRAIN_INVALID_JSON', cause }); }
  if (!value || typeof value !== 'object' || Array.isArray(value) || !Array.isArray(value.entries)) throw Object.assign(new Error('legacy v1 state must contain an entries array'), { code: 'RK_V1_DRAIN_INVALID_STATE' });
  return value;
}

function validateTerminal(candidate) {
  if (!candidate || typeof candidate !== 'object' || typeof candidate.id !== 'string' || candidate.id.length === 0) throw Object.assign(new Error('legacy v1 entry id is required'), { code: 'RK_V1_DRAIN_INVALID_ENTRY' });
  if (V1_TERMINAL.has(candidate.state)) return;
  if (!V1_OPEN.has(candidate.state)) throw Object.assign(new Error(`unknown v1 lifecycle state for ${candidate.id}`), { code: 'RK_V1_UNKNOWN_STATE', id: candidate.id, state: candidate.state });
}

function ownershipFor(candidate) {
  const ownership = candidate.ownership;
  const pgid = ownership?.pgid ?? ownership?.processGroupId;
  if (!ownership || typeof ownership !== 'object' || !Number.isSafeInteger(ownership.pid) || ownership.pid <= 0 || !Number.isSafeInteger(pgid) || pgid <= 0) {
    throw Object.assign(new Error(`active v1 request ${candidate.id} has no verifiable pid/pgid ownership`), { code: 'RK_V1_OWNERSHIP_UNKNOWN', id: candidate.id });
  }
  return Object.freeze({ ...ownership, pgid });
}

/**
 * Finalize a stopped v1 JSON broker before v2 migration. This is deliberately
 * not a broker control surface: caller must prove service stop & process-group
 * absence before this function can replace state.
 */
export async function drainStoppedV1Broker({ statePath, receiptsDir, assertBrokerStopped, assertOwnershipAbsent, occurredAt = new Date().toISOString() } = {}) {
  const source = requiredAbsolutePath(statePath, 'statePath');
  const receipts = requiredAbsolutePath(receiptsDir, 'receiptsDir');
  const stopped = requiredCallback(assertBrokerStopped, 'assertBrokerStopped');
  const absent = requiredCallback(assertOwnershipAbsent, 'assertOwnershipAbsent');
  if (Number.isNaN(Date.parse(occurredAt))) throw new TypeError('occurredAt must be an ISO timestamp');

  const stopProof = await stopped();
  if (!proven(stopProof, 'stopped')) throw Object.assign(new Error('v1 broker stop is not proven'), { code: 'RK_V1_BROKER_STILL_RUNNING' });

  const raw = await readFile(source, 'utf8');
  const legacy = parseLegacy(raw);
  const sourceSha256 = sha256(raw);
  const candidates = legacy.entries.map((entry) => ({ ...entry }));
  const changed = [];

  for (const candidate of candidates) {
    validateTerminal(candidate);
    if (candidate.state === 'QUEUED') {
      if (candidate.ownership !== undefined && candidate.ownership !== null) throw Object.assign(new Error(`queued v1 request ${candidate.id} unexpectedly owns a process`), { code: 'RK_V1_OWNERSHIP_UNKNOWN', id: candidate.id });
      changed.push(candidate.id);
      continue;
    }
    if (candidate.state !== 'ACTIVE') continue;
    const ownership = ownershipFor(candidate);
    const proof = await absent(Object.freeze({ id: candidate.id, ownership: Object.freeze({ ...ownership }), entry: Object.freeze({ ...candidate }) }));
    if (!proven(proof, 'absent')) throw Object.assign(new Error(`active v1 request ${candidate.id} still has live or unproven ownership`), { code: 'RK_V1_OWNERSHIP_LIVE', id: candidate.id, ownership });
    changed.push(candidate.id);
  }

  if (changed.length === 0) return Object.freeze({ changed: false, sourceSha256, backupPath: null, receiptPath: null, cancelledIds: Object.freeze([]) });

  const finalizedEntries = candidates.map((candidate) => !changed.includes(candidate.id) ? candidate : {
    ...candidate,
    state: 'CANCELLED',
    cancellationReason: 'v2-cutover-drain',
    terminalAt: candidate.terminalAt ?? occurredAt,
    lruAt: candidate.lruAt ?? occurredAt,
    cutover: Object.freeze({ sourceState: candidate.state, sourceSha256, occurredAt }),
  });
  const finalState = `${JSON.stringify({ ...legacy, entries: finalizedEntries })}\n`;
  const backupPath = await contentAddressedBackup(source, raw, sourceSha256);
  const receipt = Object.freeze({
    kind: 'rightkit-v1-cutover-drain',
    sourceStatePath: source,
    sourceSha256,
    backupPath,
    brokerStopped: true,
    occurredAt,
    cancelledIds: Object.freeze([...changed]),
    entries: Object.freeze(finalizedEntries.filter((entry) => changed.includes(entry.id)).map(({ id, state, cancellationReason, terminalAt, lruAt, cutover }) => ({ id, state, cancellationReason, terminalAt, lruAt, cutover }))),
  });
  const receiptPath = `${receipts}/v1-cutover-drain-${sourceSha256}.json`;
  try {
    const existing = await readFile(receiptPath, 'utf8');
    if (existing !== `${JSON.stringify(receipt)}\n`) throw Object.assign(new Error('v1 drain receipt conflicts with existing evidence'), { code: 'RK_V1_RECEIPT_CONFLICT', receiptPath });
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    await durableReplace(receiptPath, `${JSON.stringify(receipt)}\n`);
  }
  await durableReplace(source, finalState);
  return Object.freeze({ changed: true, sourceSha256, backupPath, receiptPath, cancelledIds: Object.freeze([...changed]) });
}
