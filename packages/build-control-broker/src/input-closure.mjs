// Class 2 (ordinary dev builds): re-digest the locked input closure before spawn
// and again at completion. A mismatch marks the receipt `inputs-mutated`; such a
// receipt can never be promoted to evidence. Class 1 reuses the same digest over
// its broker-owned clone (defence in depth).
//
// Limit: the check compares two points in time. A source that changes and is
// reverted (A -> B -> A) between the pre-spawn and completion snapshots is not
// detected. Only a change that is still present at completion is reported.
import { createHash } from 'node:crypto';
import { execFile as nodeExecFile } from 'node:child_process';
import { lstat, open, readlink, realpath } from 'node:fs/promises';
import { isAbsolute, join, normalize, sep } from 'node:path';
import { promisify } from 'node:util';

export const INPUTS_MUTATED = 'inputs-mutated';
export const INPUTS_VERIFIED = 'verified';
export const INPUTS_UNVERIFIED = 'unverified';
const DIGEST_CONCURRENCY = 8;
const execFile = promisify(nodeExecFile);

/** Lists the git-visible closure (tracked + untracked-but-not-ignored); .git itself is never listed. */
export async function gitListedClosure(root, { exec = execFile } = {}) {
  const { stdout } = await exec('git', ['-C', root, 'ls-files', '-z', '--cached', '--others', '--exclude-standard'], { maxBuffer: 64 * 1024 * 1024 });
  return stdout.split('\0').filter(Boolean).map((path) => normalizeClosurePath(path)).sort();
}

export function normalizeClosurePath(path) {
  if (typeof path !== 'string' || path.length === 0 || path.includes('\0') || isAbsolute(path)) throw Object.assign(new TypeError(`closure path is invalid: ${JSON.stringify(path)}`), { code: 'RK_INPUT_PATH_INVALID' });
  const clean = normalize(path).split(sep).join('/');
  if (clean === '..' || clean.startsWith('../') || clean === '.git' || clean.startsWith('.git/')) throw Object.assign(new TypeError(`closure path is outside the input closure: ${path}`), { code: 'RK_INPUT_PATH_INVALID' });
  return clean;
}

async function hashEntry(root, relative) {
  const absolute = join(root, relative);
  const info = await lstat(absolute).catch((error) => { if (error.code === 'ENOENT') return null; throw error; });
  if (!info) return 'MISSING';
  if (info.isSymbolicLink()) return `link:${createHash('sha256').update(await readlink(absolute)).digest('hex')}`;
  if (!info.isFile()) return `special:${info.mode}`;
  const hash = createHash('sha256');
  const handle = await open(absolute, 'r');
  try {
    const buffer = Buffer.allocUnsafe(1024 * 1024);
    for (;;) {
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, null);
      if (bytesRead === 0) break;
      hash.update(buffer.subarray(0, bytesRead));
    }
  } finally { await handle.close(); }
  return hash.digest('hex');
}

/** Digest over sorted (path, entry-hash) pairs plus caller-supplied authenticated digests (catalog, trust, approvals). */
export async function digestInputClosure(root, paths, { extraDigests = {}, concurrency = DIGEST_CONCURRENCY } = {}) {
  const sorted = [...new Set(paths.map(normalizeClosurePath))].sort();
  const entries = new Array(sorted.length);
  let next = 0;
  const worker = async () => { while (next < sorted.length) { const index = next; next += 1; entries[index] = await hashEntry(root, sorted[index]); } };
  await Promise.all(Array.from({ length: Math.min(concurrency, sorted.length) }, worker));
  const files = Object.freeze(Object.fromEntries(sorted.map((path, index) => [path, entries[index]])));
  const extras = Object.freeze(Object.fromEntries(Object.keys(extraDigests).sort().map((key) => [key, String(extraDigests[key])])));
  const digest = createHash('sha256').update(JSON.stringify({ files, extras })).digest('hex');
  return Object.freeze({ digest, files, extras, count: sorted.length });
}

/** Compares two snapshots; returns the exact differences so the receipt names what moved. */
export function diffInputSnapshots(before, after) {
  const changed = []; const added = []; const removed = [];
  for (const [path, hash] of Object.entries(before.files)) {
    if (!(path in after.files)) removed.push(path); else if (after.files[path] !== hash) changed.push(path);
  }
  for (const path of Object.keys(after.files)) if (!(path in before.files)) added.push(path);
  const extraKeys = new Set([...Object.keys(before.extras), ...Object.keys(after.extras)]);
  const extrasChanged = [...extraKeys].filter((key) => before.extras[key] !== after.extras[key]).sort();
  return Object.freeze({ ok: changed.length === 0 && added.length === 0 && removed.length === 0 && extrasChanged.length === 0, changed: changed.sort(), added: added.sort(), removed: removed.sort(), extrasChanged });
}

/** Guard used by the job pipeline. Digest failures are recorded as unverified and never masquerade as verified. */
export class InputClosureGuard {
  #list; #digest; #extras;

  constructor({ list = gitListedClosure, digest = digestInputClosure, extraDigests = async () => ({}) } = {}) {
    this.#list = list; this.#digest = digest; this.#extras = extraDigests;
  }

  /** Pre-spawn snapshot. Returns an opaque snapshot, or a descriptor with status `unverified` when the closure cannot be read. */
  async snapshot(root) {
    try {
      const paths = await this.#list(root);
      const extraDigests = await this.#extras(root);
      return Object.freeze({ status: INPUTS_VERIFIED, root, ...(await this.#digest(root, paths, { extraDigests })) });
    } catch (error) {
      return Object.freeze({ status: INPUTS_UNVERIFIED, root, reason: String(error?.message ?? error).slice(0, 500) });
    }
  }

  /** Completion re-digest. Returns a verdict; `status` is `inputs-mutated` whenever the closure moved. */
  async verify(snapshot) {
    if (!snapshot || snapshot.status !== INPUTS_VERIFIED) return Object.freeze({ status: INPUTS_UNVERIFIED, reason: snapshot?.reason ?? 'no pre-spawn snapshot', changed: [], added: [], removed: [], extrasChanged: [] });
    const after = await this.snapshot(snapshot.root);
    if (after.status !== INPUTS_VERIFIED) return Object.freeze({ status: INPUTS_UNVERIFIED, reason: after.reason, changed: [], added: [], removed: [], extrasChanged: [] });
    const diff = diffInputSnapshots(snapshot, after);
    return Object.freeze({ status: diff.ok ? INPUTS_VERIFIED : INPUTS_MUTATED, preDigest: snapshot.digest, postDigest: after.digest, changed: diff.changed, added: diff.added, removed: diff.removed, extrasChanged: diff.extrasChanged });
  }
}

/** Attaches the verdict to a receipt. Never clears an existing mutation marker. */
export function recordInputClosure(receipt, verdict) {
  return Object.freeze({ ...receipt, inputClosure: Object.freeze({ ...(verdict ?? { status: INPUTS_UNVERIFIED, reason: 'no verdict' }) }) });
}

/** Evidence promotion gate: only a receipt whose closure verified at completion may become evidence. */
export function assertPromotableReceipt(receipt) {
  const status = receipt?.inputClosure?.status;
  if (status === INPUTS_MUTATED) throw Object.assign(new Error(`receipt ${receipt.id ?? '<unknown>'} is inputs-mutated and cannot be promoted to evidence`), { code: 'RK_INPUTS_MUTATED' });
  if (status !== INPUTS_VERIFIED) throw Object.assign(new Error(`receipt ${receipt?.id ?? '<unknown>'} has no verified input closure (${status ?? 'absent'})`), { code: 'RK_INPUTS_UNVERIFIED' });
  return true;
}
