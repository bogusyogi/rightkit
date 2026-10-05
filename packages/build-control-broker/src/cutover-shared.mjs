// OS-agnostic cutover contract. Service control differs by OS; active-root
// proof and path validation do not.
import { randomBytes } from 'node:crypto';
import { mkdir, open, readFile, readdir, rm } from 'node:fs/promises';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { createJobCommand, JOB_REQUEST_TYPES, JOB_RESPONSE_TYPES, loadRuntimeIdentity, sendJobCommand } from '@rightkit/build-control-clients';

const CUTOVER_DRAIN_SCHEMA = 'rightkit.build-control.cutover-drain.v1';

export function cutoverDrainFencePath(externalRoot) {
  return join(resolve(externalRoot), 'rightkit-build-control', 'state', 'cutover-drain.json');
}

function processAlive(pid) {
  try { process.kill(pid, 0); return true; }
  catch (error) { return error?.code === 'EPERM'; }
}

export async function beginCutoverDrain(externalRoot, { alive = processAlive } = {}) {
  const path = cutoverDrainFencePath(externalRoot);
  await mkdir(dirname(path), { recursive: true });
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const token = randomBytes(32).toString('hex');
    try {
      const handle = await open(path, 'wx', 0o600);
      try { await handle.writeFile(`${JSON.stringify({ schema: CUTOVER_DRAIN_SCHEMA, pid: process.pid, token, createdAt: new Date().toISOString() })}\n`, 'utf8'); await handle.sync(); }
      finally { await handle.close(); }
      return Object.freeze({ path, token });
    } catch (error) {
      if (error?.code !== 'EEXIST') throw error;
      let current;
      try { current = JSON.parse(await readFile(path, 'utf8')); } catch { throw new Error('existing cutover drain fence is invalid'); }
      if (current?.schema === CUTOVER_DRAIN_SCHEMA && Number.isSafeInteger(current.pid) && alive(current.pid)) throw new Error(`cutover drain already owned by live process ${current.pid}`);
      await rm(path, { force: true });
    }
  }
  throw new Error('could not acquire cutover drain fence');
}

export async function endCutoverDrain(fence) {
  if (!fence?.path || !fence?.token) throw new TypeError('owned cutover drain fence is required');
  const current = await readFile(fence.path, 'utf8').then(JSON.parse).catch((error) => error?.code === 'ENOENT' ? null : Promise.reject(error));
  if (current && current.token !== fence.token) throw new Error('cutover drain fence ownership changed');
  if (current) await rm(fence.path, { force: true });
}

export async function cutoverDrainPending(externalRoot, { alive = processAlive } = {}) {
  let current;
  try { current = JSON.parse(await readFile(cutoverDrainFencePath(externalRoot), 'utf8')); }
  catch (error) { if (error?.code === 'ENOENT') return false; throw new Error(`cutover drain fence is invalid: ${error.message}`); }
  if (current?.schema !== CUTOVER_DRAIN_SCHEMA || !Number.isSafeInteger(current.pid) || typeof current.token !== 'string') throw new Error('cutover drain fence is invalid');
  return alive(current.pid);
}

export async function proveBrokerRequestsDrained(externalRoot, endpoint, { loadIdentity = loadRuntimeIdentity, send = sendJobCommand } = {}) {
  const runtimeIdentity = await loadIdentity({ manifestPath: join(resolve(externalRoot), 'runtime-current', 'runtime-manifest.json') });
  let status;
  try { status = await send(createJobCommand(JOB_REQUEST_TYPES.DRAIN_STATUS, { runtimeIdentity }), { endpoint, completeOn: new Set([JOB_RESPONSE_TYPES.DRAIN_STATUS]) }); }
  catch (error) {
    if (/RightKit broker unavailable:/i.test(error?.message ?? '')) return Object.freeze({ present: false, open: 0, inFlight: Object.freeze([]), runtime: runtimeIdentity });
    throw error;
  }
  if (!Number.isSafeInteger(status.open) || !Array.isArray(status.inFlight)) throw new Error('broker returned invalid cutover drain status');
  if (status.open !== 0) throw new Error(`cannot activate while ${status.open} broker request(s) are in flight: ${status.inFlight.map(({ id, state, command, elapsedMs }) => `${id} (${state ?? 'UNKNOWN'}, ${elapsedMs ?? 0}ms, ${command ?? 'command pending'})`).join('; ')}. Inspect with 'rightkit list' or 'rightkit status <request-id>'.`);
  return Object.freeze({ present: true, open: 0, inFlight: Object.freeze([]), runtime: runtimeIdentity });
}

// Deliberately narrower than config.mjs's absolute(): this only ever runs on
// the same host it validates paths for (a cutover CLI's own argv), where
// node:path is already that host's semantics — unlike config.mjs, which
// validates a config that may describe a different OS's paths than whatever
// host is currently reading it, and so must recognize Windows drive-letter
// paths even when running on POSIX.
export function absoluteHostPath(value, name) {
  if (typeof value !== 'string' || !isAbsolute(value)) throw new TypeError(`--${name} must be an absolute path`);
  return resolve(value);
}

export async function assertFile(path, label, statFile) {
  const metadata = await statFile(path);
  if (!metadata.isFile()) throw new TypeError(`${label} must be a regular file`);
}
export async function assertDirectory(path, label, statFile) {
  const metadata = await statFile(path);
  if (!metadata.isDirectory()) throw new TypeError(`${label} must be a directory`);
}

export function contained(path, root) { const value = relative(root, path); return value === '' || (!value.startsWith('..') && !isAbsolute(value)); }

/** Node's zero-signal existence probe is portable: on Windows it maps to
 * OpenProcess + a liveness check internally, no real signal delivered, same
 * ESRCH-on-absent contract as POSIX. */
/** Prove no thin root owns a process before service replacement. */
/** Multi-request client runs (`right-release dev`) lease `global/runs/<id>.json` for their whole
 * duration: a cutover between their build and follow-up requests changed workspace keys mid-run
 * (2026-10-05). Leases whose owning process is gone are ignored, so a crashed run never wedges cutover. */
export async function proveDevelopmentRunsDrained(externalRoot, { list = readdir, load = readFile, alive = processAlive } = {}) {
  const runsDirectory = join(resolve(externalRoot), 'rightkit-build-control', 'global', 'runs');
  let files;
  try { files = await list(runsDirectory); }
  catch (error) { if (error?.code === 'ENOENT') return Object.freeze({ runsDirectory, live: Object.freeze([]) }); throw new Error(`cannot prove client run drain: ${error.message}`, { cause: error }); }
  const live = [];
  for (const file of files.map((entry) => (typeof entry === 'string' ? entry : entry.name)).filter((name) => name.endsWith('.json'))) {
    let lease; try { lease = JSON.parse(await load(join(runsDirectory, file), 'utf8')); } catch { continue; }
    if (Number.isSafeInteger(lease?.pid) && lease.pid > 0 && alive(lease.pid)) live.push(Object.freeze({ id: file.slice(0, -5), pid: lease.pid, command: lease.command ?? null, startedAt: lease.startedAt ?? null }));
  }
  if (live.length) throw new Error(`cannot activate while ${live.length} client run(s) are in progress: ${live.map(({ id, pid, command }) => `${id} (pid ${pid}, ${command ?? 'command unavailable'})`).join('; ')}. Retry after they finish.`);
  return Object.freeze({ runsDirectory, live: Object.freeze([]) });
}

export async function proveThinRequestsDrained(externalRoot, { list = readdir, load = readFile, now = Date.now, alive = processAlive } = {}) {
  await proveDevelopmentRunsDrained(externalRoot, { list, load, alive });
  const activeDirectory = join(resolve(externalRoot), 'rightkit-build-control', 'global', 'active');
  let entries;
  try { entries = await list(activeDirectory, { withFileTypes: true }); }
  catch (error) {
    if (error?.code === 'ENOENT') return Object.freeze({ activeDirectory, open: 0, present: false, inFlight: Object.freeze([]) });
    throw new Error(`cannot prove thin request drain: ${error.message}`, { cause: error });
  }
  const inFlight = [];
  for (const entry of entries) {
    const name = typeof entry === 'string' ? entry : entry.name;
    const file = typeof entry === 'string' || entry.isFile() ? name : null;
    if (!file?.endsWith('.json')) continue;
    let marker;
    try { marker = JSON.parse(await load(join(activeDirectory, file), 'utf8')); }
    catch { marker = null; }
    const startedAt = marker?.startedAt ?? null; const startedMs = typeof startedAt === 'string' ? Date.parse(startedAt) : NaN;
    inFlight.push(Object.freeze({ id: marker?.id ?? file.slice(0, -5), workspaceKey: marker?.workspaceKey ?? null, command: marker?.command ?? null, startedAt, elapsedMs: Number.isFinite(startedMs) ? Math.max(0, now() - startedMs) : null }));
  }
  if (inFlight.length) throw new Error(`cannot activate while ${inFlight.length} thin request(s) are in flight: ${inFlight.map(({ id, command, elapsedMs }) => `${id} (${elapsedMs ?? 0}ms, ${command ?? 'command unavailable'})`).join('; ')}. Inspect with 'rightkit list' or 'rightkit status <request-id>'.`);
  return Object.freeze({ activeDirectory, open: 0, present: true, inFlight: Object.freeze([]) });
}
