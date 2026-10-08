import { lstat, readdir, readFile, writeFile, mkdir, mkdtemp, rename, rm, realpath, stat, chmod } from 'node:fs/promises';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { randomUUID } from 'node:crypto';
import { existsSync } from 'node:fs';
import { tmpdir } from 'node:os';

export const STORAGE_DEFAULTS = Object.freeze({ runKeep: 5, runMinAgeMs: 24 * 60 * 60_000, runCapBytes: 20_000_000_000, artifactKeep: 3, deviceKeep: 2, deviceRecentMs: 30 * 86400_000, ownershipThresholdBytes: 5_000_000_000 });
export const safeKey = (value) => { if (typeof value !== 'string' || !/^[a-zA-Z0-9][a-zA-Z0-9._-]{0,199}$/.test(value) || value === '..') throw new TypeError('unsafe storage identifier'); return value; };
export const inside = (path, root) => { const r = relative(resolve(root), resolve(path)); return r === '' || (!r.startsWith(`..${sep}`) && r !== '..' && !r.startsWith(sep)); };
export function managedStorageRoot(env = process.env, platform = process.platform, exists = existsSync) {
  if (env.RIGHTKIT_MANAGED_ROOT) return resolve(env.RIGHTKIT_MANAGED_ROOT);
  const [workstation, volume] = platform === 'win32' ? ['D:\\.rightkit-managed', 'D:\\'] : ['/Volumes/D/.rightkit-managed', '/Volumes/D'];
  if (exists(volume)) return resolve(workstation);
  // Hosted CI and other machines: runner temp, then the system temp dir.
  return join(env.RUNNER_TEMP && exists(env.RUNNER_TEMP) ? env.RUNNER_TEMP : tmpdir(), 'rightkit-managed');
}
const trustedSystemAliases = process.platform === 'darwin'
  ? new Map([['/tmp', '/private/tmp'], ['/var', '/private/var'], ['/etc', '/private/etc']])
  : new Map();
export async function noSymlinkPath(path) {
  const absolute = resolve(path); let cursor = absolute;
  for (;;) {
    const s = await lstat(cursor).catch(e => e.code === 'ENOENT' ? null : Promise.reject(e));
    if (s?.isSymbolicLink()) {
      const trustedTarget = trustedSystemAliases.get(cursor);
      const canonical = trustedTarget && await realpath(cursor).catch(() => null);
      if (!trustedTarget || canonical !== trustedTarget) throw new Error(`symlink storage path refused: ${cursor}`);
    }
    if (dirname(cursor) === cursor) break;
    cursor = dirname(cursor);
  }
  return absolute;
}
const json = async (path) => JSON.parse(await readFile(path, 'utf8'));
const entries = async (path) => readdir(path, { withFileTypes: true }).catch(e => e.code === 'ENOENT' ? [] : Promise.reject(e));
export function pidAlive(pid, kill = process.kill) { if (!Number.isSafeInteger(pid) || pid < 1) return false; try { kill(pid, 0); return true; } catch (e) { return e.code !== 'ESRCH'; } }
export function ownerAlive(meta, now = Date.now(), alive = pidAlive) { return alive(meta.ownerPid) || meta.leaseUntil > now; }
export async function measureTree(path, rows = null) {
  const s = await lstat(path); if (s.isSymbolicLink()) return 0;
  if (!s.isDirectory()) { if (rows) rows.push({ path, bytes: s.size, type: 'file' }); return s.size; }
  let bytes = 0; for (const e of await entries(path)) bytes += await measureTree(join(path, e.name), rows).catch(err => err.code === 'ENOENT' ? 0 : Promise.reject(err));
  if (rows) rows.push({ path, bytes, type: 'directory' }); return bytes;
}
export async function withStorageLock(parent, fn) {
  await noSymlinkPath(parent); await mkdir(parent, { recursive: true });
  const lock = join(parent, '.retention-lock'); await mkdir(lock).catch(e => { if (e.code === 'EEXIST') throw new Error(`storage retention busy: ${lock}`); throw e; });
  try { await writeFile(join(lock, 'owner.json'), JSON.stringify({ pid: process.pid, createdAt: Date.now() }), { flag: 'wx' }); return await fn(); }
  finally { await rm(lock, { recursive: true, force: true }); }
}
function validMeta(meta, kind, app, key) {
  return meta?.schemaVersion === 1 && meta.app === app && meta[kind === 'runs' ? 'runId' : 'version'] === key && Number.isFinite(meta.createdAt) && meta.createdAt > 0 && Number.isSafeInteger(meta.ownerPid) && meta.ownerPid >= 0 && Number.isFinite(meta.leaseUntil) && meta.leaseUntil >= 0;
}
export async function retentionPlan({ managedRoot, app, kind = 'runs', now = Date.now(), alive = pidAlive, ...policy }) {
  safeKey(app); if (!['runs', 'artifacts'].includes(kind)) throw new Error('invalid retention kind');
  const root = await noSymlinkPath(join(managedRoot, kind, app)); const known = [], skipped = []; let unknownBytes = 0;
  let published;
  if (kind === 'artifacts') { try { const value = await json(join(root, 'published.json')); if (!Array.isArray(value.versions) || value.versions.some(v => typeof v !== 'string')) throw new Error('invalid publication pointer'); published = value.versions; } catch { published = null; } }
  for (const entry of await entries(root)) {
    if (!entry.isDirectory() || entry.name.startsWith('.')) continue;
    const path = join(root, entry.name);
    let meta; try { safeKey(entry.name); meta = await json(join(path, kind === 'runs' ? '.rightkit-run.json' : '.rightkit-artifact.json')); } catch { const bytes = await measureTree(path); unknownBytes += bytes; skipped.push({ path, bytes, reason: 'unknown-or-malformed-owner' }); continue; }
    if (!validMeta(meta, kind, app, entry.name)) { const bytes = await measureTree(path); unknownBytes += bytes; skipped.push({ path, bytes, reason: 'invalid-owner-metadata' }); continue; }
    known.push({ path, bytes: await measureTree(path), meta });
  }
  const date = meta => kind === 'artifacts' ? meta.completedAt ?? meta.createdAt : meta.createdAt;
  known.sort((a, b) => date(b.meta) - date(a.meta) || b.path.localeCompare(a.path));
  const keep = policy.keep ?? (kind === 'runs' ? STORAGE_DEFAULTS.runKeep : STORAGE_DEFAULTS.artifactKeep);
  const minAgeMs = policy.minAgeMs ?? STORAGE_DEFAULTS.runMinAgeMs;
  const capBytes = policy.capBytes ?? STORAGE_DEFAULTS.runCapBytes;
  if (!Number.isSafeInteger(keep) || keep < 0 || !Number.isFinite(minAgeMs) || minAgeMs < 0 || !Number.isSafeInteger(capBytes) || capBytes < 1) throw new Error('invalid retention policy');
  let remainingBytes = unknownBytes + known.reduce((n, row) => n + row.bytes, 0); const remove = [], retain = [];
  let artifactIndex = 0;
  const candidates = known.map((row, index) => ({ ...row, index: kind === 'artifacts' ? (Number.isFinite(row.meta.completedAt) ? artifactIndex++ : Number.MAX_SAFE_INTEGER) : index })).reverse();
  for (const row of candidates) {
    let reason;
    if (ownerAlive(row.meta, now, alive)) reason = 'live-owner-or-lease';
    else if (kind === 'artifacts' && !Number.isFinite(row.meta.completedAt)) reason = 'incomplete-artifact';
    else if (kind === 'artifacts' && published === null) reason = 'publication-state-unknown';
    else if (kind === 'artifacts' && published.includes(row.meta.version)) reason = 'published';
    else if (kind === 'runs' && now - row.meta.createdAt < minAgeMs) reason = 'younger-than-minimum';
    else if (row.index < keep && (kind === 'artifacts' || remainingBytes <= capBytes)) reason = 'newest';
    if (reason) retain.push({ path: row.path, bytes: row.bytes, reason });
    else { remainingBytes -= row.bytes; remove.push({ path: row.path, bytes: row.bytes, reason: row.index >= keep ? 'retention-count' : 'app-byte-budget', meta: row.meta }); }
  }
  return { kind, app, root, remove, retain, skipped, remainingBytes, ...(kind === 'artifacts' ? { publicationState: published === null ? 'unknown' : 'known' } : {}), overCapBytes: kind === 'runs' ? Math.max(0, remainingBytes - capBytes) : 0 };
}
export async function pruneManaged(options) {
  if (!options.apply) return retentionPlan(options);
  const parent = join(options.managedRoot, options.kind ?? 'runs', safeKey(options.app));
  return withStorageLock(parent, async () => {
    const plan = await retentionPlan(options); const applied = [];
    for (const row of plan.remove) {
      await noSymlinkPath(row.path);
      const marker = join(row.path, plan.kind === 'runs' ? '.rightkit-run.json' : '.rightkit-artifact.json');
      const meta = await json(marker);
      if (JSON.stringify(meta) !== JSON.stringify(row.meta) || ownerAlive(meta, options.now ?? Date.now(), options.alive ?? pidAlive)) throw new Error(`retention owner changed: ${row.path}`);
      if (plan.kind === 'runs') await removeQaAppRoot(row.path, meta);
      const tomb = join(parent, `.pruning-${randomUUID()}`); await rename(row.path, tomb); await rm(tomb, { recursive: true }); applied.push(row.path);
    }
    return { ...plan, applied };
  });
}
export async function startStorageRun({ managedRoot = managedStorageRoot(), app, runId = `${Date.now()}-${randomUUID()}`, now = Date.now(), retention = {} }) {
  safeKey(app); safeKey(runId);
  return withStorageLock(join(managedRoot, 'runs', app), async () => {
    // Same policy selection while holding creation/pruning lock.
    const plan = await retentionPlan({ managedRoot, app, now, ...retention });
    if (plan.overCapBytes > 0) throw new Error(`managed run budget blocked by protected or unknown data: ${plan.overCapBytes} bytes over cap`);
    for (const row of plan.remove) { await removeQaAppRoot(row.path, row.meta); const tomb = join(plan.root, `.pruning-${randomUUID()}`); await rename(row.path, tomb); await rm(tomb, { recursive: true }); }
    const root = join(managedRoot, 'runs', app, runId); await mkdir(root);
    const marker = join(root, '.rightkit-run.json'); const meta = { schemaVersion: 1, app, runId, createdAt: now, ownerPid: process.pid, leaseUntil: 0 };
    await writeFile(marker, JSON.stringify(meta), { flag: 'wx' });
    for (const dir of ['scratch', 'captures', 'evidence', 'DerivedData']) await mkdir(join(root, dir));
    // Share QA's registered companion protocol; Unix sockets cannot use long run roots.
    const parent = join(process.platform === 'darwin' ? '/private/tmp' : await realpath(tmpdir()), 'rightkit-qa');
    await noSymlinkPath(parent); await mkdir(parent, { recursive: true });
    const home = await mkdtemp(join(parent, `${app.slice(0, 16)}-`));
    try {
      await writeFile(join(home, '.rightkit-app.json'), JSON.stringify({ runRoot: await realpath(root), app, runId }), { flag: 'wx' });
      for (const dir of ['tmp', 'evidence', 'captures']) await mkdir(join(home, dir));
      meta.appRoot = home;
      await writeFile(marker, JSON.stringify(meta));
    } catch (error) { await rm(home, { recursive: true, force: true }); throw error; }
    const temp = join(home, 'tmp');
    return { root, env: { RIGHTKIT_RUN_ROOT: root, RIGHTKIT_QA_RUN_ID: runId, RIGHTKIT_QA_EVIDENCE_DIR: join(root, 'evidence'), TMPDIR: temp, TMP: temp, TEMP: temp }, finish: async () => { await noSymlinkPath(root); const temporary = `${marker}.${randomUUID()}.tmp`; const current = await json(marker); await writeFile(temporary, JSON.stringify({ ...current, ownerPid: 0, leaseUntil: 0, completedAt: Date.now() }), { flag: 'wx' }); await rename(temporary, marker); } };
  });
}

async function removeQaAppRoot(runRoot, meta) {
  if (meta.appRoot === undefined) return;
  if (typeof meta.appRoot !== 'string') throw new Error('invalid QA app temp root');
  const home = await noSymlinkPath(meta.appRoot);
  const s = await lstat(home).catch(e => e.code === 'ENOENT' ? null : Promise.reject(e));
  if (!s) return;
  let temp = await realpath(tmpdir());
  if (process.platform === 'darwin' && !inside(temp, '/private/tmp') && !inside(temp, '/private/var/folders')) temp = '/private/tmp';
  const parent = dirname(home), candidateTemp = dirname(parent);
  const allowedTemp = candidateTemp === temp || process.platform === 'darwin' && (candidateTemp === '/private/tmp' || inside(candidateTemp, '/private/var/folders') && basename(candidateTemp) === 'T');
  if (basename(parent) !== 'rightkit-qa' || !allowedTemp || !s.isDirectory()) throw new Error('invalid QA app temp root');
  const binding = await json(join(home, '.rightkit-app.json'));
  if (binding.runRoot !== await realpath(runRoot) || binding.app !== meta.app || binding.runId !== meta.runId) throw new Error('QA app temp ownership mismatch');
  await rm(home, { recursive: true });
}

export function isLegacyQaTempRoot(path, managedRoot) {
  return dirname(resolve(path)) === join(resolve(managedRoot), 'tmp', 'rightkit-qa') && /^[A-Za-z0-9][A-Za-z0-9._-]*-[A-Za-z0-9][A-Za-z0-9._-]*$/.test(basename(path));
}

// Pre-governance roots have no run marker. Inspect nested owners/services/locks
// before moving an old root; unknown locks & symlinks fail closed.
async function legacyQaOwner(path, alive, now) {
  // QA app homes carry a binding rather than a local PID: inspect their run owner.
  if (existsSync(join(path, '.rightkit-app.json'))) {
    const binding = await json(await noSymlinkPath(join(path, '.rightkit-app.json')));
    if (typeof binding.runRoot !== 'string' || typeof binding.app !== 'string' || typeof binding.runId !== 'string') return 'unknown-lock-owner';
    const runRoot = await noSymlinkPath(binding.runRoot);
    const meta = await json(await noSymlinkPath(join(runRoot, '.rightkit-run.json')));
    if (!validMeta(meta, 'runs', binding.app, binding.runId) || meta.appRoot !== await realpath(path)) return 'unknown-lock-owner';
    if (ownerAlive(meta, now, alive)) return 'live-owner-or-lock';
  }
  for (const entry of await entries(path)) {
    const child = join(path, entry.name);
    if (entry.isSymbolicLink()) return 'symlink-or-unknown-owner';
    if (entry.isDirectory()) {
      if (/lock$/i.test(entry.name) && !(await entries(child)).some(e => e.name === 'owner.json')) return 'unknown-lock-owner';
      const reason = await legacyQaOwner(child, alive, now); if (reason) return reason;
    } else if (/(?:owner|service|pid|lock)/i.test(entry.name) || entry.name === '.rightkit-run.json') {
      const text = await readFile(child, 'utf8');
      let owner; try { owner = JSON.parse(text); } catch { owner = /^\d+\s*$/.test(text) ? Number(text) : null; }
      const pid = typeof owner === 'number' ? owner : owner?.ownerPid ?? owner?.pid ?? owner?.processId;
      if (!Number.isSafeInteger(pid) || pid < 0) return 'unknown-lock-owner';
      if (alive(pid) || owner?.leaseUntil > now) return 'live-owner-or-lock';
    }
  }
  return null;
}

/** Explicit app run folders, including unmarked pre-governance cache/QA runs. */
export async function trashRun({ path, managedRoot = managedStorageRoot(), apply = false, now = Date.now(), minAgeMs = STORAGE_DEFAULTS.runMinAgeMs, alive = pidAlive, move = moveToVolumeTrash }) {
  if (!Number.isFinite(minAgeMs) || minAgeMs < 0) throw new Error('invalid run minimum age');
  path = await noSymlinkPath(path);
  const markerPath = join(path, '.rightkit-run.json');
  const marked = existsSync(markerPath);
  const cacheRun = ['.cache', '.audit'].includes(basename(dirname(path)));
  const legacyRun = basename(dirname(path)) === 'rightkit-qa' || isLegacyQaTempRoot(path, managedRoot);
  if (!marked && !cacheRun && !legacyRun) throw new Error('run Trash requires an app cache run, legacy QA run, or completed run marker');
  const check = async () => {
    const s = await lstat(path);
    if (!s.isDirectory() || s.isSymbolicLink()) return 'symlink-or-not-directory';
    if (process.getuid && s.uid !== process.getuid()) return 'different-owner';
    let completedAt = 0;
    if (marked) {
      const meta = await json(await noSymlinkPath(markerPath));
      if (!validMeta(meta, 'runs', meta.app, basename(path)) || typeof meta.app !== 'string' || !Number.isFinite(meta.completedAt) || meta.completedAt <= 0) return 'incomplete-or-invalid-run';
      completedAt = meta.completedAt;
      if (ownerAlive(meta, now, alive)) return 'live-owner-or-lock';
      // Registered companions must be removed through managed retention, not orphaned in Trash.
      if (meta.appRoot !== undefined) return 'registered-companion-use-managed-retention';
    }
    const owner = await legacyQaOwner(path, alive, now);
    if (owner) return owner;
    if (now - Math.max(s.mtimeMs, completedAt) < minAgeMs) return 'younger-than-minimum';
    return null;
  };
  const inspect = () => check().catch(() => 'unreadable-owner');
  const reason = await inspect();
  if (reason) return { source: path, kind: 'run', applied: false, skipped: true, reason };
  const bytes = await measureTree(path);
  if (apply && await inspect()) throw new Error(`run Trash owner changed: ${path}`);
  return { ...await move(path, { apply, now }), kind: 'run', bytes, reason: 'completed-app-run' };
}

export async function pruneLegacyQaTemp({ managedRoot, apply = false, now = Date.now(), minAgeMs = STORAGE_DEFAULTS.runMinAgeMs, alive = pidAlive, move = moveToVolumeTrash } = {}) {
  if (!Number.isFinite(minAgeMs) || minAgeMs < 0) throw new Error('invalid run minimum age');
  const parent = await noSymlinkPath(join(managedRoot, 'tmp', 'rightkit-qa'));
  const result = { root: parent, remove: [], skipped: [], moves: [] };
  for (const entry of await entries(parent)) {
    const path = join(parent, entry.name);
    if (!isLegacyQaTempRoot(path, managedRoot)) continue;
    if (!entry.isDirectory() || entry.isSymbolicLink()) { result.skipped.push({ path, reason: 'symlink-or-not-directory' }); continue; }
    await noSymlinkPath(path);
    const check = async () => {
      const s = await lstat(path);
      if (!s.isDirectory() || s.isSymbolicLink()) return 'symlink-or-not-directory';
      if (now - s.mtimeMs < minAgeMs) return 'younger-than-minimum';
      if (process.getuid && s.uid !== process.getuid()) return 'different-owner';
      return legacyQaOwner(path, alive, now);
    };
    let reason; try { reason = await check(); } catch { reason = 'unreadable-owner'; }
    if (reason) { result.skipped.push({ path, reason }); continue; }
    result.remove.push({ path, bytes: await measureTree(path), reason: 'legacy-qa-temp' });
    if (apply && await check()) throw new Error(`legacy QA temp owner changed: ${path}`);
    result.moves.push(await move(path, { apply, now }));
  }
  return result;
}
export async function volumeRoot(path) { let cursor = await realpath(path); const dev = (await stat(cursor)).dev; while (dirname(cursor) !== cursor && (await stat(dirname(cursor))).dev === dev) cursor = dirname(cursor); return cursor; }
async function ensureVolumeTrash(actualVolume, uid, device) {
  const root = join(actualVolume, '.Trashes'); const path = join(root, String(uid));
  await noSymlinkPath(root);
  let rootCreated = false;
  try { await mkdir(root, { mode: 0o1777 }); rootCreated = true; } catch (error) { if (error.code !== 'EEXIST') throw error; }
  if (rootCreated) await chmod(root, 0o1777);
  await noSymlinkPath(root);
  const rootStat = await lstat(root);
  const currentUid = process.getuid?.();
  if (!rootStat.isDirectory() || rootStat.dev !== device || (process.platform !== 'win32' && (rootStat.mode & 0o1000) === 0)) throw new Error('volume Trash root must be a same-volume sticky directory');
  await noSymlinkPath(path);
  let pathCreated = false;
  try { await mkdir(path, { mode: 0o700 }); pathCreated = true; } catch (error) { if (error.code !== 'EEXIST') throw error; }
  if (pathCreated) await chmod(path, 0o700);
  await noSymlinkPath(path);
  const pathStat = await lstat(path);
  if (!pathStat.isDirectory() || pathStat.dev !== device || (currentUid !== undefined && (uid !== currentUid || pathStat.uid !== currentUid)) || (process.platform !== 'win32' && (pathStat.mode & 0o777) !== 0o700)) throw new Error('volume Trash user directory must be current-user private storage');
  return path;
}
export async function moveToVolumeTrash(path, { volume, uid = process.getuid?.(), now = Date.now(), apply = false, locateVolume = volumeRoot } = {}) {
  const source = await realpath(await noSymlinkPath(path)); const s = await lstat(source); const actualVolume = await realpath(await locateVolume(source));
  if (volume && await realpath(volume) !== actualVolume) throw new Error('trash source is on another volume');
  if (!Number.isSafeInteger(uid) || uid < 0 || source === actualVolume || inside(source, join(actualVolume, '.Trashes'))) throw new Error('unsafe volume Trash selection');
  const trash = join(actualVolume, '.Trashes', String(uid));
  await noSymlinkPath(trash);
  const destination = join(trash, `${basename(source)}-${now}-${randomUUID()}`);
  if (apply) { await ensureVolumeTrash(actualVolume, uid, s.dev); await rename(source, destination); }
  return { source, destination, action: 'move-to-volume-trash', applied: apply };
}
const category = (name) => name === '.audit' ? 'qa-audit' : name === '.cache' ? 'app-cache' : /\.dmg$|\.msi$|\.exe$|\.pkg$/i.test(name) ? 'installers' : /archive/i.test(name) ? 'archives' : /research/i.test(name) ? 'research' : /trash/i.test(name) ? 'workspace-trash' : 'repository';
export async function storageReport({ managedRoot = managedStorageRoot(), apps = [], external = [] } = {}) {
  const usage = [], offenders = [], errors = [];
  const scan = async (path, managed, app, cat) => { try { await noSymlinkPath(path); const rows = []; const bytes = await measureTree(path, rows); usage.push({ path, managed, app, category: cat, bytes }); offenders.push(...rows.map(row => ({ ...row, managed, app, category: cat }))); } catch (e) { if (e.code !== 'ENOENT') errors.push({ path, error: e.message }); } };
  for (const entry of await entries(managedRoot)) if (entry.isDirectory()) {
    if (['runs', 'artifacts'].includes(entry.name)) { for (const app of await entries(join(managedRoot, entry.name))) if (app.isDirectory()) await scan(join(managedRoot, entry.name, app.name), true, app.name, entry.name); }
    else if (entry.name === 'rightkit-build-control') {
      const brokerRoot = join(managedRoot, entry.name);
      for (const child of await entries(brokerRoot)) {
        if (child.name === 'ws' && child.isDirectory()) {
          for (const workspace of await entries(join(brokerRoot, 'ws'))) if (workspace.isDirectory()) {
            const path = join(brokerRoot, 'ws', workspace.name); let app = null;
            try { const identity = await json(join(path, 'identity.json')); app = String(identity.canonicalRoot ?? '').split(/[\\/]/).findLast(part => /right$/i.test(part)) ?? null; } catch {}
            await scan(path, true, app, 'workspace-build-cache');
          }
        } else if (child.name === 'global' && child.isDirectory()) {
          for (const global of await entries(join(brokerRoot, 'global'))) await scan(join(brokerRoot, 'global', global.name), true, null, global.name);
        } else await scan(join(brokerRoot, child.name), true, null, `broker-${child.name}`);
      }
    } else await scan(join(managedRoot, entry.name), true, null, entry.name);
  }
  for (const item of apps) for (const entry of await entries(item.path)) if (entry.name !== '.git') await scan(join(item.path, entry.name), false, item.app, category(entry.name));
  for (const path of external) await scan(resolve(path), false, null, category(basename(path)));
  const totals = { managedBytes: 0, unmanagedBytes: 0, byCategory: {}, byApp: {} };
  for (const row of usage) { totals[row.managed ? 'managedBytes' : 'unmanagedBytes'] += row.bytes; const key = `${row.managed ? 'managed' : 'unmanaged'}:${row.category}`; totals.byCategory[key] = (totals.byCategory[key] ?? 0) + row.bytes; if (row.app) totals.byApp[row.app] = (totals.byApp[row.app] ?? 0) + row.bytes; }
  return { usage, totals, topOffenders: offenders.sort((a, b) => b.bytes - a.bytes).slice(0, 30), errors };
}
