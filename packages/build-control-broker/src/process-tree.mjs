import { execFile as nodeExecFile, spawn as nodeSpawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const launcherPath = fileURLToPath(new URL('./managed-launcher.mjs', import.meta.url));
const closedChildren = new WeakSet();
const startupPromises = new WeakMap();

/** Windows startup stays fail-closed until a native Job Object owner exists. */
export const WINDOWS_CONTAINMENT_CONTRACT = Object.freeze({
  code: 'RK_WINDOWS_CONTAINMENT_UNAVAILABLE',
  reason: 'windows-job-object-unsupported',
  required: 'native Job Object owner: CREATE_SUSPENDED + AssignProcessToJobObject + KILL_ON_JOB_CLOSE before ResumeThread',
});
function windowsContainmentError() { return Object.assign(new Error(WINDOWS_CONTAINMENT_CONTRACT.required), WINDOWS_CONTAINMENT_CONTRACT); }

export function spawnManagedProcess(command, argv, options = {}, spawn = nodeSpawn) {
  if (typeof command !== 'string' || !command || !Array.isArray(argv) || argv.some((value) => typeof value !== 'string')) throw new TypeError('managed command & argv are required');
  const { platform = process.platform, ...spawnOptions } = options;
  if (platform === 'win32') throw windowsContainmentError();
  const payload = Buffer.from(JSON.stringify({ command, argv }), 'utf8').toString('base64url');
  const child = spawn(process.execPath, [launcherPath, payload], { ...spawnOptions, stdio: ['ignore', spawnOptions.stdio?.[1] ?? 'pipe', spawnOptions.stdio?.[2] ?? 'pipe', 'ipc'], detached: platform !== 'win32' });
  observeClose(child); return child;
}

/** Send START only after ownership persistence; resolve after payload spawn. */
export function startManagedProcess(child, { timeoutMs = 5_000, platform = process.platform } = {}) {
  if (platform === 'win32') throw windowsContainmentError();
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
  if (platform === 'win32') throw windowsContainmentError();
  const observedIdentity = identityValue(identity);
  if (platform !== 'win32' && (!Number.isSafeInteger(uid) || uid < 0 || !Number.isSafeInteger(sessionId) || sessionId < 1)) throw new TypeError('POSIX ownership requires uid & session id');
  return Object.freeze({ pid: child.pid, processGroupId: platform === 'win32' ? null : child.pid, platform: platform === 'win32' ? 'win32' : 'posix', identity: observedIdentity, ...(platform === 'win32' ? {} : { uid, sessionId }) });
}

/**
 * Read a portable, opaque process-start token without relying on a live daemon.
 * Linux uses proc starttime; macOS uses `ps lstart`. Windows Job Object identity
 * is intentionally unsupported until a Job Object owner exists.
 */
export async function readProcessIdentity(pid, ownership = null, { platform = process.platform, readFile, execFile = nodeExecFile, timeoutMs = 250 } = {}) {
  if (!Number.isSafeInteger(pid) || pid < 1) throw new TypeError('positive process pid is required');
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) throw new TypeError('positive process identity timeout is required');
  if (ownership?.platform === 'win32' || platform === 'win32') return unavailable('windows-job-object-unsupported');
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

export async function terminateProcessTree(child, { platform = process.platform, graceMs = 5_000, kill = process.kill, spawn = nodeSpawn, execFile = nodeExecFile, uid = process.getuid?.(), psTimeoutMs = 250 } = {}) {
  if (!child || !Number.isSafeInteger(child.pid) || child.pid < 1) throw new TypeError('spawned child with pid is required');
  if (!Number.isSafeInteger(graceMs) || graceMs < 0) throw new TypeError('graceMs must be non-negative');
  if (!Number.isSafeInteger(psTimeoutMs) || psTimeoutMs < 1) throw new TypeError('positive process enumeration timeout is required');
  observeClose(child);
  if (platform === 'win32') {
    // taskkill is only best-effort. Without a Job Object it cannot certify all
    // descendants, so callers must treat this as REAP_PENDING.
    if (!alreadyExited(child)) { await runTaskkill(child.pid, false, spawn); if (!await waitForExit(child, graceMs)) { await runTaskkill(child.pid, true, spawn); await waitForExit(child, graceMs); } }
    return reapPending('windows-job-object-unsupported', !alreadyExited(child));
  }
  // A launcher may exit while descendants still hold its process group. Probe
  // and reap that owned group even after the group leader has exited.
  const group = { platform, kill, execFile, uid, psTimeoutMs };
  let escalated = false;
  try {
    if (!await ownedGroupAlive(child.pid, group)) return await terminalGroupState(child, true, graceMs);
    await signalOwnedGroup(child.pid, 'SIGTERM', group);
    if (await waitForOwnedGroupExit(child.pid, graceMs, group)) return await terminalGroupState(child, true, graceMs);
    escalated = true; await signalOwnedGroup(child.pid, 'SIGKILL', group); const groupGone = await waitForOwnedGroupExit(child.pid, graceMs, group);
    return await terminalGroupState(child, groupGone, graceMs, true);
  } catch (error) {
    if (error?.code === 'RK_PROCESS_GROUP_ENUMERATION') return reapPending(error.reason ?? 'process-group-enumeration-unavailable', escalated);
    throw error;
  }
}

/**
 * `readIdentity(pid, ownership)` is deliberately injected. Platform-specific
 * start-time/creation-token readers belong outside this portable module.
 */
export async function reapPersistedProcessTree(ownership, { graceMs = 5_000, kill = process.kill, spawn = nodeSpawn, readIdentity, platform = process.platform, execFile = nodeExecFile, uid = process.getuid?.(), psTimeoutMs = 250 } = {}) {
  if (!ownership || !Number.isSafeInteger(ownership.pid) || ownership.pid < 1 || !['win32', 'posix'].includes(ownership.platform)) throw new TypeError('persisted process ownership is required');
  if (ownership.platform === 'win32') return reapPending('windows-job-object-unsupported');
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
