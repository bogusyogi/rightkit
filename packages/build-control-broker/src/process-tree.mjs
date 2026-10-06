import { execFile as nodeExecFile, spawn as nodeSpawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { loadWindowsJobAddon } from './windows-job.mjs';
import { loadMacosResourceAddon } from './macos-resource.mjs';

const launcherPath = fileURLToPath(new URL('./managed-launcher.mjs', import.meta.url));
const closedChildren = new WeakSet();
const startupPromises = new WeakMap();
// Job/process handles are process-local & die with this broker process, so
// they cannot survive a restart — reapPersistedProcessTree therefore never
// reads this map; see its own doc comment for the cross-restart fallback.
const windowsJobHandles = new WeakMap();
const managedProcessResources = new WeakMap();
const managedProcessResourceFinal = new WeakSet();

/** Windows startup stays fail-closed until a native Job Object owner exists. */
export const WINDOWS_CONTAINMENT_CONTRACT = Object.freeze({
  code: 'RK_WINDOWS_CONTAINMENT_UNAVAILABLE',
  reason: 'windows-job-object-unsupported',
  required: 'native Job Object owner: synchronous AssignProcessToJobObject + KILL_ON_JOB_CLOSE before IPC START',
});
function windowsContainmentError() { return Object.assign(new Error(WINDOWS_CONTAINMENT_CONTRACT.required), WINDOWS_CONTAINMENT_CONTRACT); }

// stdin is 'ignore' unless a job explicitly pipes a credential (cargo login); the launcher passes its own
// stdin straight through to the payload, so 'ignore' still means the null device all the way down.
const launcherStdin = (spawnOptions) => spawnOptions.stdio?.[0] === 'pipe' ? 'pipe' : 'ignore';

export function spawnManagedProcess(command, argv, options = {}, spawn = nodeSpawn) {
  if (typeof command !== 'string' || !command || !Array.isArray(argv) || argv.some((value) => typeof value !== 'string')) throw new TypeError('managed command & argv are required');
  // `umask` travels in the payload, never in spawnOptions: it is applied by the
  // launcher immediately before exec so the payload's own file creations (an
  // sccache server socket, for example) get a private mode without inserting a
  // shell between the hash-verified tool and its exec.
  const { platform = process.platform, loadJobAddon = loadWindowsJobAddon, loadResourceAddon = loadMacosResourceAddon, umask, killOnDisconnect = false, resourceIntervalMs, ...spawnOptions } = options;
  const payloadObject = { command, argv, ...(Number.isSafeInteger(umask) ? { umask } : {}), ...(killOnDisconnect === true ? { killOnDisconnect: true } : {}), ...(platform === 'darwin' && Number.isSafeInteger(resourceIntervalMs) && resourceIntervalMs > 0 ? { resourceIntervalMs } : {}) };
  const payload = Buffer.from(JSON.stringify(payloadObject), 'utf8').toString('base64url');
  if (platform === 'win32') {
    const addon = loadJobAddon();
    if (!addon) throw windowsContainmentError();
    // windowsHide with no inherited stdio makes libuv pass CREATE_NO_WINDOW (never DETACHED_PROCESS): the
    // launcher gets a windowless console that cargo, rustc, build scripts & test binaries inherit, so their
    // console-subsystem children open no window whether or not the broker itself has a hidden console.
    const child = spawn(process.execPath, [launcherPath, payload], { ...spawnOptions, stdio: [launcherStdin(spawnOptions), spawnOptions.stdio?.[1] ?? 'pipe', spawnOptions.stdio?.[2] ?? 'pipe', 'ipc'], detached: false, windowsHide: true });
    // Immediately, before any await: Node owns process creation so its IPC
    // channel can hold this pinned launcher at the START gate. The launcher
    // cannot spawn the managed payload until startManagedProcess sends START,
    // which happens only after this assignment & durable ownership capture.
    if (!Number.isSafeInteger(child.pid) || child.pid < 1) throw new Error('managed launcher did not report a pid to contain');
    const handles = addon.assignRunningProcessToNewJob(child.pid);
    windowsJobHandles.set(child, handles);
    observeClose(child);
    child.once('close', () => { captureManagedProcessResources(child, { platform, loadJobAddon, loadResourceAddon }); });
    child.once('close', () => { try { addon.closeProcessHandle(handles.processHandle); } catch { /* best effort */ } });
    return child;
  }
  const child = spawn(process.execPath, [launcherPath, payload], { ...spawnOptions, stdio: [launcherStdin(spawnOptions), spawnOptions.stdio?.[1] ?? 'pipe', spawnOptions.stdio?.[2] ?? 'pipe', 'ipc'], detached: true });
  observeClose(child);
  if (platform === 'darwin') {
    child.on?.('message', (message) => {
      if (message?.type !== 'RESOURCE' && message?.type !== 'RESOURCE_FINAL') return;
      const snapshot = normalizeMacResourceSnapshot(message.snapshot);
      if (snapshot) {
        managedProcessResources.set(child, snapshot);
        if (message.type === 'RESOURCE_FINAL') managedProcessResourceFinal.add(child);
      }
    });
    child.once('close', () => {
      if (!managedProcessResourceFinal.has(child)) {
        managedProcessResources.set(child, Object.freeze({ status: 'UNAVAILABLE', reason: 'process-resources-final-sample-unavailable' }));
        return;
      }
      captureManagedProcessResources(child, { platform, loadResourceAddon });
    });
  }
  return child;
}

/** Send START only after Job/process-group ownership persistence; resolve after payload spawn. */
export function startManagedProcess(child, { timeoutMs = 30_000, platform = process.platform } = {}) {
  if (!child || typeof child.send !== 'function' || !child.connected) throw new Error('managed launcher IPC is unavailable');
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) throw new TypeError('positive launcher start timeout is required');
  const existing = startupPromises.get(child); if (existing) return existing;
  const startup = new Promise((resolve, reject) => {
    let settled = false;
    const finish = (error, value) => {
      if (settled) return; settled = true; clearTimeout(timer); child.off?.('message', message); child.off?.('close', close); child.off?.('error', errorEvent); child.off?.('disconnect', disconnect);
      error ? reject(error) : resolve(Object.freeze({ pid: value.pid }));
    };
    const failure = (code, detail) => Object.assign(new Error(detail), { code });
    const message = (value) => {
      if (value?.type === 'STARTED' && Number.isSafeInteger(value.pid) && value.pid > 0) finish(null, value);
      else if (value?.type === 'START_FAILED') finish(failure('RK_LAUNCHER_START_FAILED', value.error || 'managed payload failed to spawn'));
    };
    const close = () => finish(failure('RK_LAUNCHER_START_CLOSED', 'managed launcher closed before payload STARTED'));
    const errorEvent = (error) => finish(failure('RK_LAUNCHER_START_ERROR', error?.message || 'managed launcher errored before payload STARTED'));
    const disconnect = () => finish(failure('RK_LAUNCHER_START_CLOSED', 'managed launcher disconnected before payload STARTED'));
    const timer = setTimeout(() => finish(failure('RK_LAUNCHER_START_TIMEOUT', 'managed launcher STARTED timed out')), timeoutMs);
    child.on?.('message', message); child.once?.('close', close); child.once?.('error', errorEvent); child.once?.('disconnect', disconnect);
    try { child.send({ type: 'START' }, (error) => { if (error) finish(failure('RK_LAUNCHER_START_ERROR', error.message)); }); } catch (error) { finish(failure('RK_LAUNCHER_START_ERROR', error.message)); }
  });
  startupPromises.set(child, startup); return startup;
}

/**
 * Persist this record before START. `identity` must be an opaque process-start
 * token from a platform reader; a pid alone is never safe for restart reaping.
 */
export function managedProcessOwnership(child, { platform = process.platform, identity, uid = process.getuid?.(), sessionId = child?.pid } = {}) {
  if (!child || !Number.isSafeInteger(child.pid) || child.pid < 1) throw new TypeError('spawned child with pid is required');
  if (typeof platform !== 'string' || !platform) throw new TypeError('supported process platform is required');
  const observedIdentity = identityValue(identity);
  if (platform !== 'win32' && (!Number.isSafeInteger(uid) || uid < 0 || !Number.isSafeInteger(sessionId) || sessionId < 1)) throw new TypeError('POSIX ownership requires uid & session id');
  return Object.freeze({ pid: child.pid, processGroupId: platform === 'win32' ? null : child.pid, platform: platform === 'win32' ? 'win32' : 'posix', identity: observedIdentity, ...(platform === 'win32' ? {} : { uid, sessionId }) });
}

/**
 * Read a portable, opaque process-start token without relying on a live daemon.
 * Linux uses proc starttime; macOS uses `ps lstart`. Windows Job Object identity
 * is intentionally unsupported until a Job Object owner exists.
 */
export async function readProcessIdentity(pid, ownership = null, { platform = process.platform, readFile, execFile = nodeExecFile, timeoutMs = 250, loadJobAddon = loadWindowsJobAddon } = {}) {
  if (!Number.isSafeInteger(pid) || pid < 1) throw new TypeError('positive process pid is required');
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) throw new TypeError('positive process identity timeout is required');
  if (ownership?.platform === 'win32' || platform === 'win32') return readWindowsIdentity(pid, { execFile, timeoutMs, loadJobAddon });
  if (platform === 'linux') return readLinuxIdentity(pid, { readFile, timeoutMs });
  if (platform === 'darwin') return readMacosIdentity(pid, { execFile, timeoutMs });
  return unavailable('process-identity-platform-unsupported');
}

/** Retry bounded launcher identity reads while its 30-second start gate holds it alive. */
export async function readManagedProcessIdentity(pid, { attempts = 4, timeoutMs = 1_000, readIdentity = readProcessIdentity } = {}) {
  if (!Number.isSafeInteger(attempts) || attempts < 1) throw new TypeError('positive process identity attempts are required');
  let observed = unavailable('process-identity-unavailable');
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    observed = await readIdentity(pid, null, { timeoutMs });
    if (observed?.status === 'OK') return observed;
  }
  return observed;
}

/**
 * Enumerate the live descendants of one pid as `Map<pid, { pid, command }>`.
 * This module owns every `ps`-based process-relationship read; ownership proofs
 * elsewhere consume this rather than shelling out on their own.
 */
export async function enumerateLiveDescendants(rootPid, { execFile = nodeExecFile, timeoutMs = 250 } = {}) {
  if (!Number.isSafeInteger(rootPid) || rootPid < 1) throw new TypeError('positive root process pid is required');
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) throw new TypeError('positive process enumeration timeout is required');
  if (typeof execFile !== 'function') throw groupEnumerationError('process-descendant-enumeration-unavailable');
  let source;
  try { source = String(await boundedProcessGroup(runDescendantList(execFile, timeoutMs), timeoutMs)); }
  catch (error) { if (error?.code === 'RK_PROCESS_GROUP_ENUMERATION') throw error; throw groupEnumerationError('process-descendant-enumeration-unavailable'); }
  const children = new Map(); const commands = new Map(); let parsedRows = 0;
  for (const line of source.split(/\r?\n/)) {
    const match = line.trim().match(/^(\d+)\s+(\d+)\s+(.*)$/); if (!match) continue;
    const pid = Number(match[1]); const parentPid = Number(match[2]);
    if (!Number.isSafeInteger(pid) || pid < 1 || !Number.isSafeInteger(parentPid) || parentPid < 0) continue;
    parsedRows += 1; commands.set(pid, match[3].trim());
    if (!children.has(parentPid)) children.set(parentPid, []); children.get(parentPid).push(pid);
  }
  if (parsedRows === 0) throw groupEnumerationError('process-descendant-enumeration-malformed');
  const descendants = new Map(); const queue = [rootPid]; const seen = new Set([rootPid]);
  while (queue.length) {
    const current = queue.shift();
    for (const pid of children.get(current) ?? []) {
      if (seen.has(pid)) continue;
      seen.add(pid); queue.push(pid);
      descendants.set(pid, Object.freeze({ pid, command: commands.get(pid) ?? '' }));
    }
  }
  return descendants;
}
function runDescendantList(execFile, timeoutMs) {
  return new Promise((resolve, reject) => {
    execFile('ps', ['-axo', 'pid=,ppid=,comm='], { encoding: 'utf8', timeout: timeoutMs, maxBuffer: 4 * 1024 * 1024, windowsHide: true }, (error, stdout) => error ? reject(error) : resolve(stdout));
  });
}

export async function terminateProcessTree(child, { platform = process.platform, graceMs = 5_000, kill = process.kill, spawn = nodeSpawn, execFile = nodeExecFile, uid = process.getuid?.(), psTimeoutMs = 250, loadJobAddon = loadWindowsJobAddon, loadResourceAddon = loadMacosResourceAddon } = {}) {
  if (!child || !Number.isSafeInteger(child.pid) || child.pid < 1) throw new TypeError('spawned child with pid is required');
  if (!Number.isSafeInteger(graceMs) || graceMs < 0) throw new TypeError('graceMs must be non-negative');
  if (!Number.isSafeInteger(psTimeoutMs) || psTimeoutMs < 1) throw new TypeError('positive process enumeration timeout is required');
  observeClose(child);
  if (platform === 'win32') {
    const handles = windowsJobHandles.get(child);
    const addon = handles ? loadJobAddon() : null;
    if (handles && addon) {
      // Certain kill: closing the job (after an explicit TerminateJobObject)
      // takes every process it contains with it, not just the launcher —
      // this is the actual "reap the whole tree" guarantee taskkill /T can't
      // make (it walks a parent-PID snapshot, which a fast-exiting parent
      // can already have made stale).
      if (!alreadyExited(child)) { addon.terminateJob(handles.jobHandle, true); await waitForExit(child, graceMs); }
      captureManagedProcessResources(child, { platform, loadJobAddon });
      try { addon.closeJobHandle?.(handles.jobHandle); } catch { /* best effort */ }
      windowsJobHandles.delete(child);
      // TerminateJobObject is immediate & forceful, unlike the POSIX
      // SIGTERM-then-SIGKILL escalation path — there is no graceful step to
      // report, so escalated is unconditionally true here.
      return alreadyExited(child) ? reaped(true) : reapPending('process-still-live', true);
    }
    // No job handle: either the addon was unavailable at spawn time (already
    // fail-closed then) or this child was never spawned through
    // spawnManagedProcess. taskkill is best-effort only — REAP_PENDING is
    // correct here, not REAPED.
    if (!alreadyExited(child)) { await runTaskkill(child.pid, false, spawn); if (!await waitForExit(child, graceMs)) { await runTaskkill(child.pid, true, spawn); await waitForExit(child, graceMs); } }
    return reapPending('windows-job-object-unsupported', !alreadyExited(child));
  }
  // A launcher may exit while descendants still hold its process group. Probe
  // and reap that owned group even after the group leader has exited.
  const group = { platform, kill, execFile, uid, psTimeoutMs };
  let escalated = false;
  try {
    if (!await ownedGroupAlive(child.pid, group)) { const result = await terminalGroupState(child, true, graceMs); captureManagedProcessResources(child, { platform, loadResourceAddon }); return result; }
    await signalOwnedGroup(child.pid, 'SIGTERM', group);
    if (await waitForOwnedGroupExit(child.pid, graceMs, group)) { const result = await terminalGroupState(child, true, graceMs); captureManagedProcessResources(child, { platform, loadResourceAddon }); return result; }
    escalated = true; await signalOwnedGroup(child.pid, 'SIGKILL', group); const groupGone = await waitForOwnedGroupExit(child.pid, graceMs, group);
    const result = await terminalGroupState(child, groupGone, graceMs, true); captureManagedProcessResources(child, { platform, loadResourceAddon }); return result;
  } catch (error) {
    if (error?.code === 'RK_PROCESS_GROUP_ENUMERATION') return reapPending(error.reason ?? 'process-group-enumeration-unavailable', escalated);
    throw error;
  }
}

/** Read managed Windows Job Object accounting without changing enforcement.
 * A final snapshot is retained before the native handles are closed. */
export function readManagedProcessResources(child, { platform = process.platform, loadJobAddon = loadWindowsJobAddon, loadResourceAddon = loadMacosResourceAddon } = {}) {
  if (!child || !Number.isSafeInteger(child.pid) || child.pid < 1) throw new TypeError('spawned child with pid is required');
  if (platform === 'darwin') {
    const cached = managedProcessResources.get(child);
    if (cached?.status === 'OK') return cached;
    let addon;
    try { addon = loadResourceAddon?.(); } catch { return cached ?? unavailable('process-resources-addon-unavailable'); }
    if (typeof addon?.queryProcessTreeResourceUsage !== 'function') return cached ?? unavailable('process-resources-addon-unavailable');
    try {
      const result = normalizeMacResourceSnapshot(addon.queryProcessTreeResourceUsage(child.pid));
      if (!result) throw new Error('malformed resource snapshot');
      managedProcessResources.set(child, result);
      return result;
    } catch (error) {
      return Object.freeze({ status: 'ERROR', reason: 'process-resources-query-failed', error: String(error?.message ?? error) });
    }
  }
  if (platform !== 'win32') return unavailable('process-resources-platform-unsupported');
  const handles = windowsJobHandles.get(child);
  const cached = managedProcessResources.get(child);
  if (!handles) return cached ?? unavailable('process-resources-job-unavailable');
  let addon;
  try { addon = loadJobAddon?.(); } catch { return unavailable('process-resources-addon-unavailable'); }
  if (typeof addon?.queryJobResourceUsage !== 'function') return unavailable('process-resources-addon-unavailable');
  try {
    const usage = addon.queryJobResourceUsage(handles.jobHandle);
    const fields = ['cpuUserMs', 'cpuKernelMs', 'cpuTotalMs', 'peakCommittedBytes', 'totalProcesses', 'activeProcesses'];
    if (!usage || fields.some((field) => !Number.isFinite(usage[field]) || usage[field] < 0)) throw new Error('malformed resource snapshot');
    const result = Object.freeze({ status: 'OK', ...Object.fromEntries(fields.map((field) => [field, usage[field]])) });
    managedProcessResources.set(child, result);
    return result;
  } catch (error) {
    return Object.freeze({ status: 'ERROR', reason: 'process-resources-query-failed', error: String(error?.message ?? error) });
  }
}

function captureManagedProcessResources(child, options) {
  const result = readManagedProcessResources(child, options);
  managedProcessResources.set(child, result);
  return result;
}

function normalizeMacResourceSnapshot(usage) {
  const fields = ['cpuUserMs', 'cpuKernelMs', 'cpuTotalMs', 'peakMemoryBytes', 'totalProcesses', 'activeProcesses'];
  if (!usage || typeof usage !== 'object' || fields.some((field) => !Number.isFinite(usage[field]) || usage[field] < 0)) return null;
  return Object.freeze({ status: 'OK', ...Object.fromEntries(fields.map((field) => [field, usage[field]])), memoryKind: 'physical-footprint', ...(typeof usage.memorySemantics === 'string' ? { memorySemantics: usage.memorySemantics } : {}) });
}

/**
 * `readIdentity(pid, ownership)` is deliberately injected. Platform-specific
 * start-time/creation-token readers belong outside this portable module.
 */
/**
 * Windows cross-restart reap of a process this broker process did not spawn.
 * The live job handle from spawnManagedProcess is process-local & gone after
 * a restart — Win32 has no persisted-group-signalling equivalent to POSIX's
 * pgid enumeration, so this cannot certify a whole descendant tree the way
 * the live-process terminateProcessTree path can.
 *
 * In the common case this finds nothing to do: an anonymous Job Object with
 * JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE is torn down by the OS the instant the
 * owning process's last handle closes — which happens automatically on ANY
 * process exit, crash or graceful, with no code required to run. So a prior
 * broker's death should already have killed its whole managed tree before
 * this ever executes. This function exists for the residual case where that
 * did not happen (e.g. a handle was duplicated out-of-band): it verifies the
 * persisted PID via creation-time identity (immune to PID reuse, same as the
 * POSIX path), then terminates that single verified process if still alive.
 */
export async function reapPersistedProcessTree(ownership, { graceMs = 5_000, kill = process.kill, spawn = nodeSpawn, readIdentity = readProcessIdentity, platform = process.platform, execFile = nodeExecFile, uid = process.getuid?.(), psTimeoutMs = 250, loadJobAddon = loadWindowsJobAddon } = {}) {
  if (!ownership || !Number.isSafeInteger(ownership.pid) || ownership.pid < 1 || !['win32', 'posix'].includes(ownership.platform)) throw new TypeError('persisted process ownership is required');
  if (ownership.platform === 'win32') {
    const addon = loadJobAddon();
    if (!addon) return reapPending('windows-job-object-unsupported');
    // KILL_ON_JOB_CLOSE normally removes the whole tree before recovery starts.
    // A dead PID needs no identity proof because no process will be touched.
    if (!addon.isPidAlive(ownership.pid)) return reaped(false);
    const identity = await persistedIdentity(ownership, readIdentity);
    // A missing identity token means we cannot rule out PID reuse; refuse
    // rather than risk terminating an unrelated process that reused the pid.
    if (!identity.matches) return reapPending(identity.reason);
    try {
      const handles = addon.assignRunningProcessToNewJob(ownership.pid);
      addon.terminateJob(handles.jobHandle);
    } catch (error) { return reapPending(error?.message ?? 'process-termination-failed'); }
    const deadline = Date.now() + graceMs;
    while (addon.isPidAlive(ownership.pid)) { if (Date.now() >= deadline) return reapPending('process-still-live', true); await new Promise((resolve) => setTimeout(resolve, Math.min(25, Math.max(1, deadline - Date.now())))); }
    return reaped(true);
  }
  const processGroupId = Number.isSafeInteger(ownership.processGroupId) && ownership.processGroupId > 0 ? ownership.processGroupId : ownership.pid;
  const scope = persistedScope(ownership, { uid, processGroupId });
  if (!scope) return reapPending('process-group-scope-missing');
  const identity = await persistedIdentity(ownership, readIdentity ?? readProcessIdentity);
  // A live launcher whose creation token changed is PID reuse. Never touch it.
  // A missing launcher is expected: its still-live descendants can be proven by
  // exact pgid + session + uid enumeration before any signal is sent.
  if (!identity.matches && identity.reason !== 'process-identity-unavailable') return reapPending(identity.reason);
  const group = { platform, kill, execFile, uid: scope.uid, psTimeoutMs };
  try {
    let members = await enumeratePersistedOwnedGroup(processGroupId, scope, group);
    if (!members.length) return reaped(false);
    await signalPersistedMembers(members, 'SIGTERM', kill);
    if (await waitForPersistedGroupExit(processGroupId, scope, graceMs, group)) return reaped(false);
    members = await enumeratePersistedOwnedGroup(processGroupId, scope, group);
    if (!members.length) return reaped(false);
    await signalPersistedMembers(members, 'SIGKILL', kill);
    return await waitForPersistedGroupExit(processGroupId, scope, graceMs, group) ? reaped(true) : reapPending('process-group-still-live', true);
  } catch (error) {
    if (error?.code === 'RK_PROCESS_GROUP_ENUMERATION' || error?.code === 'RK_PROCESS_GROUP_OWNERSHIP') return reapPending(error.reason ?? 'process-group-enumeration-unavailable');
    if (error?.code === 'EPERM') return reapPending('process-group-signal-permission-denied');
    return Promise.reject(error);
  }
}

async function persistedIdentity(ownership, readIdentity) {
  if (typeof ownership.identity !== 'string' || !ownership.identity) return { matches: false, reason: 'process-identity-missing' };
  if (typeof readIdentity !== 'function') return { matches: false, reason: 'process-identity-reader-missing' };
  try {
    const observed = await readIdentity(ownership.pid, ownership);
    const value = identityValue(observed, false);
    if (!value) return { matches: false, reason: observed?.reason ?? 'process-identity-unavailable' };
    return value === ownership.identity ? { matches: true } : { matches: false, reason: 'process-identity-mismatch' };
  } catch { return { matches: false, reason: 'process-identity-unavailable' }; }
}
function persistedScope(ownership, { uid, processGroupId }) {
  const ownerUid = Number.isSafeInteger(ownership.uid) && ownership.uid >= 0 ? ownership.uid : uid;
  // Detached POSIX launchers lead both their initial process group & session.
  // Existing records predate explicit session persistence, so that invariant is
  // recovered only for their original `processGroupId`.
  const sessionId = Number.isSafeInteger(ownership.sessionId) && ownership.sessionId > 0 ? ownership.sessionId : processGroupId;
  return Number.isSafeInteger(ownerUid) && ownerUid >= 0 && Number.isSafeInteger(sessionId) && sessionId > 0 ? { uid: ownerUid, sessionId } : null;
}
async function enumeratePersistedOwnedGroup(processGroupId, scope, { execFile, psTimeoutMs }) {
  if (typeof execFile !== 'function') throw groupEnumerationError('process-group-enumeration-unavailable');
  try {
    const source = String(await boundedProcessGroup(runPersistedProcessList(execFile, psTimeoutMs), psTimeoutMs));
    const members = []; let groupMembers = 0; let parsedRows = 0;
    for (const line of source.split(/\r?\n/)) {
      const match = line.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+(\d+)$/); if (!match) continue;
      parsedRows += 1;
      const pid = Number(match[1]); const pgid = Number(match[2]); const sessionId = Number(match[3]); const ownerUid = Number(match[4]);
      if (!Number.isSafeInteger(pid) || pid < 1 || pgid !== processGroupId) continue;
      groupMembers += 1;
      if (sessionId === scope.sessionId && ownerUid === scope.uid) members.push(pid);
    }
    if (parsedRows === 0) throw groupEnumerationError('process-group-enumeration-malformed');
    if (groupMembers !== members.length) throw groupOwnershipError('process-group-ownership-ambiguous');
    return members;
  } catch (error) { if (error?.code === 'RK_PROCESS_GROUP_ENUMERATION' || error?.code === 'RK_PROCESS_GROUP_OWNERSHIP') throw error; throw groupEnumerationError('process-group-enumeration-unavailable'); }
}
function runPersistedProcessList(execFile, timeoutMs) {
  return new Promise((resolve, reject) => {
    execFile('ps', ['-axo', 'pid=,pgid=,sess=,uid='], { encoding: 'utf8', timeout: timeoutMs, maxBuffer: 1024 * 1024, windowsHide: true }, (error, stdout) => error ? reject(error) : resolve(stdout));
  });
}
function groupOwnershipError(reason) { return Object.assign(new Error(reason), { code: 'RK_PROCESS_GROUP_OWNERSHIP', reason }); }
async function signalPersistedMembers(members, signal, kill) {
  for (const pid of members) {
    try { kill(pid, signal); }
    catch (error) { if (error?.code !== 'ESRCH') throw error; }
  }
}
async function waitForPersistedGroupExit(processGroupId, scope, timeoutMs, options) {
  const deadline = Date.now() + timeoutMs;
  while ((await enumeratePersistedOwnedGroup(processGroupId, scope, options)).length) {
    if (Date.now() >= deadline) return false;
    await new Promise((resolve) => setTimeout(resolve, Math.min(25, Math.max(1, deadline - Date.now()))));
  }
  return true;
}
async function readLinuxIdentity(pid, { readFile = defaultReadFile, timeoutMs }) {
  try {
    const source = String(await bounded(readFile(`/proc/${pid}/stat`, 'utf8'), timeoutMs));
    if (source.length > 4_096) return unavailable('process-identity-malformed');
    const close = source.lastIndexOf(')'); const fields = close < 0 ? [] : source.slice(close + 1).trim().split(/\s+/);
    const starttime = fields[19];
    return /^\d+$/.test(starttime ?? '') ? available(`linux:${pid}:${starttime}`) : unavailable('process-identity-malformed');
  } catch (error) { return unavailable(error?.code === 'RK_PROCESS_IDENTITY_TIMEOUT' ? 'process-identity-timeout' : 'process-identity-unavailable'); }
}
/** CreationDate is a WMI-formatted timestamp string unique enough to detect
 * PID reuse the same way Linux's proc starttime & macOS's `ps lstart` do —
 * a reused pid gets a new creation time, so a stale identity never matches. */
async function readWindowsIdentity(pid, { execFile, timeoutMs, loadJobAddon }) {
  try {
    const addon = loadJobAddon?.();
    if (typeof addon?.getProcessCreationTime === 'function') {
      const created = String(addon.getProcessCreationTime(pid)).trim();
      return created ? available(`win32:${pid}:${created}`) : unavailable('process-identity-unavailable');
    }
  } catch { /* compatibility fallback for source runtimes without the new addon */ }
  if (typeof execFile !== 'function') return unavailable('process-identity-reader-missing');
  try {
    const source = String(await bounded(runPowerShellCreationDate(execFile, pid, timeoutMs), timeoutMs)).trim();
    return source ? available(`win32:${pid}:${source}`) : unavailable('process-identity-unavailable');
  } catch (error) { return unavailable(error?.code === 'RK_PROCESS_IDENTITY_TIMEOUT' ? 'process-identity-timeout' : 'process-identity-unavailable'); }
}
function runPowerShellCreationDate(execFile, pid, timeoutMs) {
  return new Promise((resolve, reject) => {
    execFile('powershell', ['-NoProfile', '-NonInteractive', '-Command', `Get-CimInstance Win32_Process -Filter "ProcessId=${Number(pid)}" | Select-Object -ExpandProperty CreationDate`], { encoding: 'utf8', timeout: timeoutMs, maxBuffer: 1_024, windowsHide: true }, (error, stdout) => error ? reject(error) : resolve(stdout));
  });
}
async function readMacosIdentity(pid, { execFile, timeoutMs }) {
  try {
    const source = String(await bounded(runPs(execFile, pid, timeoutMs), timeoutMs)).trim();
    return source ? available(`darwin:${pid}:${source}`) : unavailable('process-identity-unavailable');
  } catch (error) { return unavailable(error?.code === 'RK_PROCESS_IDENTITY_TIMEOUT' ? 'process-identity-timeout' : 'process-identity-unavailable'); }
}
function runPs(execFile, pid, timeoutMs) {
  if (typeof execFile !== 'function') throw new TypeError('execFile is required');
  return new Promise((resolve, reject) => {
    execFile('ps', ['-o', 'lstart=', '-p', String(pid)], { encoding: 'utf8', timeout: timeoutMs, maxBuffer: 1_024, windowsHide: true }, (error, stdout) => error ? reject(error) : resolve(stdout));
  });
}
function bounded(promise, timeoutMs) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(Object.assign(new Error('process identity timed out'), { code: 'RK_PROCESS_IDENTITY_TIMEOUT' })), timeoutMs); timer.unref?.(); })]).finally(() => clearTimeout(timer));
}
function identityValue(observed, required = true) {
  const value = typeof observed === 'string' ? observed : observed?.status === 'OK' ? observed.identity : null;
  if (typeof value === 'string' && value) return value;
  if (required) throw new TypeError('observed process-start identity is required');
  return null;
}
const available = (identity) => Object.freeze({ status: 'OK', identity });
const unavailable = (reason) => Object.freeze({ status: 'UNAVAILABLE', reason });
const defaultReadFile = async (...args) => (await import('node:fs/promises')).readFile(...args);
const reaped = (escalated) => Object.freeze({ status: 'REAPED', reaped: true, escalated });
const reapPending = (reason, escalated = false) => Object.freeze({ status: 'REAP_PENDING', reaped: false, escalated, reason });

function signalGroup(pid, signal, kill) { try { kill(-pid, signal); } catch (error) { if (error.code !== 'ESRCH') throw error; } }
async function signalOwnedGroup(processGroupId, signal, options) {
  try { signalGroup(processGroupId, signal, options.kill); }
  catch (error) {
    if (error?.code !== 'EPERM' || options.platform !== 'darwin') throw error;
    const members = await enumerateOwnedGroup(processGroupId, options);
    for (const pid of members) try { options.kill(pid, signal); } catch (memberError) { if (!['ESRCH', 'EPERM'].includes(memberError?.code)) throw memberError; }
  }
}
async function ownedGroupAlive(processGroupId, options) {
  try { return groupAlive(processGroupId, options.kill); }
  catch (error) {
    if (error?.code !== 'EPERM' || options.platform !== 'darwin') throw error;
    return (await enumerateOwnedGroup(processGroupId, options)).length > 0;
  }
}
async function enumerateOwnedGroup(processGroupId, { execFile, uid, psTimeoutMs }) {
  if (!Number.isSafeInteger(uid) || uid < 0) throw groupEnumerationError('process-group-uid-unavailable');
  try {
    const source = String(await boundedProcessGroup(runProcessList(execFile, psTimeoutMs), psTimeoutMs)); const members = []; let parsedRows = 0;
    for (const line of source.split(/\r?\n/)) {
      const match = line.trim().match(/^(\d+)\s+(\d+)\s+(\d+)$/); if (!match) continue;
      parsedRows += 1;
      const pid = Number(match[1]); const pgid = Number(match[2]); const owner = Number(match[3]);
      if (Number.isSafeInteger(pid) && pid > 0 && pgid === processGroupId && owner === uid) members.push(pid);
    }
    if (parsedRows === 0) throw groupEnumerationError('process-group-enumeration-malformed');
    return members;
  } catch (error) { if (error?.code === 'RK_PROCESS_GROUP_ENUMERATION') throw error; throw groupEnumerationError('process-group-enumeration-unavailable'); }
}
function runProcessList(execFile, timeoutMs) {
  if (typeof execFile !== 'function') throw groupEnumerationError('process-group-enumeration-unavailable');
  return new Promise((resolve, reject) => {
    execFile('ps', ['-axo', 'pid=,pgid=,uid='], { encoding: 'utf8', timeout: timeoutMs, maxBuffer: 1024 * 1024, windowsHide: true }, (error, stdout) => error ? reject(error) : resolve(stdout));
  });
}
function boundedProcessGroup(promise, timeoutMs) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(groupEnumerationError('process-group-enumeration-timeout')), timeoutMs); })]).finally(() => clearTimeout(timer));
}
function groupEnumerationError(reason) { return Object.assign(new Error(reason), { code: 'RK_PROCESS_GROUP_ENUMERATION', reason }); }
function alreadyExited(child) { return child.exitCode !== null && child.exitCode !== undefined || child.signalCode !== null && child.signalCode !== undefined; }
function waitForExit(child, timeoutMs) {
  observeClose(child); if (closedChildren.has(child)) return Promise.resolve(true);
  return new Promise((resolve) => {
    let timer; const done = () => { clearTimeout(timer); child.off?.('close', done); resolve(true); };
    child.once('close', done); timer = setTimeout(() => { child.off?.('close', done); resolve(false); }, timeoutMs);
  });
}
async function terminalGroupState(child, groupGone, graceMs, escalated = false) {
  const launcherClosed = await waitForExit(child, graceMs);
  return groupGone && launcherClosed ? reaped(escalated) : reapPending(groupGone ? 'launcher-close-unobserved' : 'process-group-still-live', escalated);
}
function observeClose(child) {
  if (!child || typeof child.once !== 'function') return;
  child.once('close', () => closedChildren.add(child));
}
function runTaskkill(pid, force, spawn) {
  return new Promise((resolve, reject) => {
    const child = spawn('taskkill.exe', ['/PID', String(pid), '/T', ...(force ? ['/F'] : [])], { windowsHide: true, stdio: 'ignore' });
    child.once('error', (error) => error.code === 'ENOENT' ? reject(error) : resolve()); child.once('close', () => resolve());
  });
}
async function waitForPidExit(pid, timeoutMs, kill) { const deadline = Date.now() + timeoutMs; while (pidAlive(pid, kill)) { if (Date.now() >= deadline) return false; await new Promise((resolve) => setTimeout(resolve, Math.min(25, Math.max(1, deadline - Date.now())))); } return true; }
function pidAlive(pid, kill) { try { kill(pid, 0); return true; } catch (error) { if (error.code === 'ESRCH') return false; throw error; } }
async function waitForOwnedGroupExit(processGroupId, timeoutMs, options) { const deadline = Date.now() + timeoutMs; while (await ownedGroupAlive(processGroupId, options)) { if (Date.now() >= deadline) return false; await new Promise((resolve) => setTimeout(resolve, Math.min(25, Math.max(1, deadline - Date.now())))); } return true; }
function groupAlive(processGroupId, kill) { try { kill(-processGroupId, 0); return true; } catch (error) { if (error.code === 'ESRCH') return false; throw error; } }
