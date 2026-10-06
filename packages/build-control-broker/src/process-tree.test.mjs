import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { WINDOWS_CONTAINMENT_CONTRACT, managedProcessOwnership, readManagedProcessIdentity, readManagedProcessResources, readProcessIdentity, reapPersistedProcessTree, spawnManagedProcess, startManagedProcess, terminateProcessTree } from './process-tree.mjs';

const child = () => Object.assign(new EventEmitter(), { pid: 41, exitCode: null, signalCode: null });

test('POSIX cancellation targets process group with TERM before KILL', async () => {
  const target = child(); const signals = []; let alive = true;
  await terminateProcessTree(target, { platform: 'darwin', graceMs: 1, kill: (pid, signal) => { if (signal !== 0) signals.push([pid, signal]); if (signal === 'SIGKILL') { alive = false; target.signalCode = signal; queueMicrotask(() => target.emit('close', null, signal)); } if (signal === 0 && !alive) throw Object.assign(new Error('gone'), { code: 'ESRCH' }); } });
  assert.deepEqual(signals, [[-41, 'SIGTERM'], [-41, 'SIGKILL']]);
});

test('POSIX graceful exit does not escalate', async () => {
  const target = child(); const signals = [];
  let alive = true; const result = await terminateProcessTree(target, { platform: 'linux', graceMs: 20, kill: (pid, signal) => { if (signal !== 0) signals.push([pid, signal]); if (signal === 'SIGTERM') { alive = false; target.signalCode = signal; queueMicrotask(() => target.emit('close', null, signal)); } if (signal === 0 && !alive) throw Object.assign(new Error('gone'), { code: 'ESRCH' }); } });
  assert.deepEqual(signals, [[-41, 'SIGTERM']]); assert.equal(result.escalated, false);
});

test('exited launcher with live POSIX group is signalled & reaped', async () => {
  const target = child(); target.exitCode = 0; const signals = []; let alive = true;
  queueMicrotask(() => target.emit('close', 0, null));
  const result = await terminateProcessTree(target, { platform: 'linux', graceMs: 1, kill: (pid, signal) => { if (signal !== 0) signals.push([pid, signal]); if (signal === 'SIGKILL') alive = false; if (signal === 0 && !alive) throw Object.assign(new Error('gone'), { code: 'ESRCH' }); } });
  assert.deepEqual(signals, [[-41, 'SIGTERM'], [-41, 'SIGKILL']]); assert.deepEqual(result, { status: 'REAPED', reaped: true, escalated: true });
});

test('macOS EPERM falls back to exact same-uid PGID members through bounded ps', async () => {
  const target = child(); const signals = []; const members = new Set([41, 42]); let psCalls = 0;
  const kill = (pid, signal) => {
    if (pid < 0) throw Object.assign(new Error('not permitted'), { code: 'EPERM' });
    signals.push([pid, signal]);
    if (signal === 'SIGTERM' && pid === 41) { members.delete(pid); target.signalCode = signal; queueMicrotask(() => target.emit('close', null, signal)); }
    if (signal === 'SIGKILL') members.delete(pid);
  };
  const execFile = (command, argv, options, callback) => { psCalls += 1; assert.equal(command, 'ps'); assert.deepEqual(argv, ['-axo', 'pid=,pgid=,uid=']); assert.equal(options.timeout, 10); const owned = [...members].map((pid) => `${pid} 41 501`); queueMicrotask(() => callback(null, [...owned, '77 9 501', '88 41 502'].join('\n'))); };
  const result = await terminateProcessTree(target, { platform: 'darwin', graceMs: 1, kill, execFile, uid: 501, psTimeoutMs: 10 });
  assert.ok(psCalls >= 3); assert.deepEqual(signals, [[41, 'SIGTERM'], [42, 'SIGTERM'], [42, 'SIGKILL']]); assert.deepEqual(result, { status: 'REAPED', reaped: true, escalated: true });
});

test('macOS EPERM enumeration timeout stays REAP_PENDING', async () => {
  const target = child(); const kill = (pid) => { if (pid < 0) throw Object.assign(new Error('not permitted'), { code: 'EPERM' }); };
  const result = await terminateProcessTree(target, { platform: 'darwin', graceMs: 1, kill, execFile: () => {}, uid: 501, psTimeoutMs: 5 });
  assert.deepEqual(result, { status: 'REAP_PENDING', reaped: false, escalated: false, reason: 'process-group-enumeration-timeout' });
});

test('Windows taskkill is best-effort & remains REAP_PENDING without a Job Object', async () => {
  const target = child(); const calls = [];
  const spawn = (_command, argv) => { calls.push(argv); const taskkill = new EventEmitter(); queueMicrotask(() => taskkill.emit('close', 0)); if (argv.includes('/F')) { target.exitCode = 1; queueMicrotask(() => target.emit('close', 1)); } return taskkill; };
  const result = await terminateProcessTree(target, { platform: 'win32', graceMs: 1, spawn });
  assert.equal(result.status, 'REAP_PENDING'); assert.equal(result.reason, 'windows-job-object-unsupported'); assert.deepEqual(calls, [['/PID', '41', '/T'], ['/PID', '41', '/T', '/F']]);
});

test('Windows heavy-job startup fails closed before launcher spawn', () => {
  let spawned = false;
  assert.throws(() => spawnManagedProcess('cargo', ['build'], { platform: 'win32', loadJobAddon: () => null }, () => { spawned = true; }), WINDOWS_CONTAINMENT_CONTRACT);
  assert.equal(spawned, false);
});

test('Windows ownership persistence builds a record with no process-group id, matching win32 having no pgid concept', () => {
  // Fail-closed containment is enforced once, at spawnManagedProcess — job
  // assignment already happened by the time ownership is ever recorded, so
  // this function has nothing left to reject.
  const record = managedProcessOwnership(child(), { platform: 'win32', identity: 'creation-time' });
  assert.deepEqual(record, { pid: 41, processGroupId: null, platform: 'win32', identity: 'creation-time' });
});

test('Windows START gate proceeds: IPC is Node’s own spawn() machinery, not native Job Object surface', () => {
  let started = false;
  const launcher = Object.assign(new EventEmitter(), { pid: 61, connected: true, send: (message, callback) => { started = message?.type === 'START'; callback?.(); } });
  const startup = startManagedProcess(launcher, { platform: 'win32', timeoutMs: 50 });
  launcher.emit('message', { type: 'STARTED', pid: 62 });
  return startup.then((result) => { assert.equal(started, true); assert.deepEqual(result, { pid: 62 }); });
});

test('restart recovery reaps persisted POSIX process group before closure', async () => {
  const calls = []; let members = [51];
  const kill = (pid, signal) => { calls.push([pid, signal]); if (pid === 51 && signal === 'SIGTERM') members = []; };
  const execFile = (_command, _argv, _options, callback) => queueMicrotask(() => callback(null, [...members.map((pid) => `${pid} 51 51 501`), '999 999 999 501'].join('\n')));
  const result = await reapPersistedProcessTree({ pid: 51, processGroupId: 51, platform: 'posix', identity: 'start-51', uid: 501, sessionId: 51 }, { graceMs: 10, kill, execFile, readIdentity: () => 'start-51' }); assert.equal(result.reaped, true); assert.equal(result.status, 'REAPED'); assert.deepEqual(calls, [[51, 'SIGTERM']]);
});

test('PID identity mismatch becomes REAP_PENDING without signalling a reused pid', async () => {
  const calls = [];
  const result = await reapPersistedProcessTree({ pid: 51, processGroupId: 51, platform: 'posix', identity: 'start-51', uid: 501, sessionId: 51 }, { platform: 'darwin', graceMs: 1, kill: (...args) => calls.push(args), readIdentity: () => 'replacement-51' });
  assert.deepEqual(result, { status: 'REAP_PENDING', reaped: false, escalated: false, reason: 'process-identity-mismatch' }); assert.deepEqual(calls, []);
});

test('restart recovery reaps live descendants after launcher has exited', async () => {
  const calls = []; let members = [62, 63];
  const kill = (pid, signal) => { calls.push([pid, signal]); if (signal === 'SIGTERM') members = members.filter((member) => member !== pid); };
  const execFile = (command, argv, _options, callback) => { assert.equal(command, 'ps'); assert.deepEqual(argv, ['-axo', 'pid=,pgid=,sess=,uid=']); queueMicrotask(() => callback(null, [...members.map((pid) => `${pid} 61 61 501`), '999 999 999 501'].join('\n'))); };
  const result = await reapPersistedProcessTree({ pid: 61, processGroupId: 61, platform: 'posix', identity: 'start-61', uid: 501, sessionId: 61 }, { graceMs: 10, kill, execFile, readIdentity: () => ({ status: 'UNAVAILABLE', reason: 'process-identity-unavailable' }) });
  assert.deepEqual(result, { status: 'REAPED', reaped: true, escalated: false });
  assert.deepEqual(calls, [[62, 'SIGTERM'], [63, 'SIGTERM']]);
});

test('restart recovery rejects a reused launcher PID before group enumeration', async () => {
  const calls = []; let listed = false;
  const result = await reapPersistedProcessTree({ pid: 71, processGroupId: 71, platform: 'posix', identity: 'start-71', uid: 501, sessionId: 71 }, {
    graceMs: 1,
    kill: (...args) => calls.push(args),
    readIdentity: () => 'replacement-71',
    execFile: () => { listed = true; },
  });
  assert.deepEqual(result, { status: 'REAP_PENDING', reaped: false, escalated: false, reason: 'process-identity-mismatch' }); assert.deepEqual(calls, []); assert.equal(listed, false);
});

test('restart recovery rejects foreign uid or session members in matching PGID', async () => {
  for (const row of ['82 81 81 502', '82 81 80 501']) {
    const calls = [];
    const result = await reapPersistedProcessTree({ pid: 81, processGroupId: 81, platform: 'posix', identity: 'start-81', uid: 501, sessionId: 81 }, {
      graceMs: 1,
      kill: (...args) => calls.push(args),
      readIdentity: () => ({ status: 'UNAVAILABLE', reason: 'process-identity-unavailable' }),
      execFile: (_command, _argv, _options, callback) => queueMicrotask(() => callback(null, row)),
    });
    assert.deepEqual(result, { status: 'REAP_PENDING', reaped: false, escalated: false, reason: 'process-group-ownership-ambiguous' }); assert.deepEqual(calls, []);
  }
});

test('restart recovery marks an absent scoped group reaped when launcher is gone', async () => {
  const calls = [];
  const result = await reapPersistedProcessTree({ pid: 91, processGroupId: 91, platform: 'posix', identity: 'start-91', uid: 501, sessionId: 91 }, {
    graceMs: 1,
    kill: (...args) => calls.push(args),
    readIdentity: () => ({ status: 'UNAVAILABLE', reason: 'process-identity-unavailable' }),
    execFile: (_command, _argv, _options, callback) => queueMicrotask(() => callback(null, '99 99 99 501')),
  });
  assert.deepEqual(result, { status: 'REAPED', reaped: true, escalated: false }); assert.deepEqual(calls, []);
});

test('restart recovery fails closed when exact descendant signalling is denied', async () => {
  const result = await reapPersistedProcessTree({ pid: 101, processGroupId: 101, platform: 'posix', identity: 'start-101', uid: 501, sessionId: 101 }, {
    graceMs: 1,
    kill: () => { throw Object.assign(new Error('denied'), { code: 'EPERM' }); },
    readIdentity: () => ({ status: 'UNAVAILABLE', reason: 'process-identity-unavailable' }),
    execFile: (_command, _argv, _options, callback) => queueMicrotask(() => callback(null, '102 101 101 501')),
  });
  assert.deepEqual(result, { status: 'REAP_PENDING', reaped: false, escalated: false, reason: 'process-group-signal-permission-denied' });
});

test('managed launcher resolves STARTED only after durable ownership is captured', async () => {
  const messages = []; const launched = Object.assign(new EventEmitter(), { pid: 61, connected: true, send: (message) => messages.push(message) });
  const childProcess = spawnManagedProcess('/managed/cargo', ['build'], { platform: 'darwin' }, (command, argv, options) => { assert.equal(command, process.execPath); assert.equal(options.stdio.at(-1), 'ipc'); assert.equal(typeof argv.at(-1), 'string'); return launched; });
  assert.deepEqual(messages, []); const ownership = managedProcessOwnership(childProcess, { identity: { status: 'OK', identity: 'test-start-token' } });
  assert.equal(ownership.pid, childProcess.pid); const started = startManagedProcess(childProcess, { timeoutMs: 20 }); assert.deepEqual(messages, [{ type: 'START' }]); launched.emit('message', { type: 'STARTED', pid: 62 }); assert.deepEqual(await started, { pid: 62 });
});

test('managed launcher start rejects on close, error & timeout', async () => {
  const launcher = () => Object.assign(new EventEmitter(), { pid: 61, connected: true, send() {} });
  const closed = launcher(); const closing = startManagedProcess(closed, { timeoutMs: 20 }); closed.emit('close', 125, null); await assert.rejects(closing, { code: 'RK_LAUNCHER_START_CLOSED' });
  const failed = launcher(); const erroring = startManagedProcess(failed, { timeoutMs: 20 }); failed.emit('error', new Error('spawn failed')); await assert.rejects(erroring, { code: 'RK_LAUNCHER_START_ERROR' });
  const hung = launcher(); await assert.rejects(startManagedProcess(hung, { timeoutMs: 5 }), { code: 'RK_LAUNCHER_START_TIMEOUT' });
});

test('Linux process identity reads bounded proc starttime through an injected seam', async () => {
  const fields = ['S', ...Array(18).fill('0'), '987654'];
  const result = await readProcessIdentity(51, null, { platform: 'linux', timeoutMs: 10, readFile: async (path, encoding) => { assert.equal(path, '/proc/51/stat'); assert.equal(encoding, 'utf8'); return `51 (node worker) ${fields.join(' ')}`; } });
  assert.deepEqual(result, { status: 'OK', identity: 'linux:51:987654' });
});

test('macOS process identity uses bounded ps lstart through an injected seam', async () => {
  const result = await readProcessIdentity(52, null, { platform: 'darwin', timeoutMs: 10, execFile: (command, argv, options, callback) => { assert.equal(command, 'ps'); assert.deepEqual(argv, ['-o', 'lstart=', '-p', '52']); assert.equal(options.timeout, 10); queueMicrotask(() => callback(null, 'Thu Aug 14 20:00:00 2026\n')); } });
  assert.deepEqual(result, { status: 'OK', identity: 'darwin:52:Thu Aug 14 20:00:00 2026' });
});

test('Windows process identity uses native creation time without PowerShell', async () => {
  const result = await readProcessIdentity(52, null, {
    platform: 'win32',
    timeoutMs: 10,
    loadJobAddon: () => ({ getProcessCreationTime: (pid) => { assert.equal(pid, 52); return '134157836012345678'; } }),
    execFile: () => assert.fail('PowerShell fallback must not run'),
  });
  assert.deepEqual(result, { status: 'OK', identity: 'win32:52:134157836012345678' });
});

test('identity probe timeout is typed unavailable & requires no live process', async () => {
  const result = await readProcessIdentity(53, null, { platform: 'linux', timeoutMs: 5, readFile: () => new Promise(() => {}) });
  assert.deepEqual(result, { status: 'UNAVAILABLE', reason: 'process-identity-timeout' });
});

test('managed launcher identity retries transient bounded probe failures', async () => {
  const timeouts = [];
  const result = await readManagedProcessIdentity(53, {
    attempts: 3,
    timeoutMs: 1_000,
    readIdentity: async (_pid, _ownership, options) => {
      timeouts.push(options.timeoutMs);
      return timeouts.length < 3 ? { status: 'UNAVAILABLE', reason: 'process-identity-timeout' } : { status: 'OK', identity: 'darwin:53:start' };
    },
  });
  assert.deepEqual(result, { status: 'OK', identity: 'darwin:53:start' });
  assert.deepEqual(timeouts, [1_000, 1_000, 1_000]);
});

test('persisted Windows tree is REAP_PENDING because Job Object recovery is unsupported', async () => {
  const result = await reapPersistedProcessTree({ pid: 54, platform: 'win32', identity: 'creation-time' }, { loadJobAddon: () => null });
  assert.deepEqual(result, { status: 'REAP_PENDING', reaped: false, escalated: false, reason: 'windows-job-object-unsupported' });
});

test('Windows assigns Job Object before IPC START can create payload', async () => {
  const calls = [];
  const fakeAddon = {
    assignRunningProcessToNewJob: (pid) => { calls.push(['assign', pid]); return { processHandle: 'ph', jobHandle: 'jh' }; },
    closeProcessHandle: (handle) => { calls.push(['closeProcessHandle', handle]); },
  };
  const spawnedChild = Object.assign(new EventEmitter(), {
    pid: 777, exitCode: null, signalCode: null, connected: true,
    send(message, callback) { calls.push(['send', message.type]); callback?.(); }
  });
  const spawn = () => { calls.push(['spawn']); return spawnedChild; };
  const result = spawnManagedProcess('binary', ['build'], { platform: 'win32', loadJobAddon: () => fakeAddon }, spawn);
  assert.equal(result, spawnedChild);
  assert.deepEqual(calls, [['spawn'], ['assign', 777]]);
  const started = startManagedProcess(result, { platform: 'win32', timeoutMs: 50 });
  assert.deepEqual(calls, [['spawn'], ['assign', 777], ['send', 'START']]);
  spawnedChild.emit('message', { type: 'STARTED', pid: 778 });
  assert.deepEqual(await started, { pid: 778 });
  // The processHandle is released once the child actually closes, not before.
  spawnedChild.emit('close', 0, null);
  assert.deepEqual(calls.at(-1), ['closeProcessHandle', 'ph']);
});

test('Windows terminate uses the job handle for a certain whole-tree kill when the addon assigned one at spawn', async () => {
  const calls = [];
  const fakeAddon = { assignRunningProcessToNewJob: (pid) => ({ processHandle: 'ph', jobHandle: `jh-${pid}` }), closeProcessHandle: () => {}, terminateJob: (handle) => { calls.push(['terminateJob', handle]); } };
  const target = Object.assign(new EventEmitter(), { pid: 888, exitCode: null, signalCode: null });
  const spawned = spawnManagedProcess('binary', ['build'], { platform: 'win32', loadJobAddon: () => fakeAddon }, () => target);
  queueMicrotask(() => { spawned.exitCode = 0; spawned.emit('close', 0, null); });
  const result = await terminateProcessTree(spawned, { platform: 'win32', graceMs: 50, loadJobAddon: () => fakeAddon });
  assert.deepEqual(calls, [['terminateJob', 'jh-888']]);
  assert.equal(result.status, 'REAPED');
});

test('Windows cancellation queries final accounting before closing native Job handle', async () => {
  let jobOpen = true; let queriedWhileOpen = false;
  const usage = { cpuUserMs: 8, cpuKernelMs: 2, cpuTotalMs: 10, peakCommittedBytes: 8192, totalProcesses: 2, activeProcesses: 0 };
  const fakeAddon = {
    assignRunningProcessToNewJob: (pid) => ({ processHandle: 'ph', jobHandle: `jh-${pid}` }),
    terminateJob: (_handle, keepOpen) => { assert.equal(keepOpen, true); target.exitCode = 1; queueMicrotask(() => target.emit('close', 1, null)); },
    queryJobResourceUsage: () => { queriedWhileOpen = jobOpen; return usage; },
    closeJobHandle: () => { jobOpen = false; },
  };
  const target = Object.assign(new EventEmitter(), { pid: 889, exitCode: null, signalCode: null });
  spawnManagedProcess('binary', [], { platform: 'win32', loadJobAddon: () => fakeAddon }, () => target);
  const result = await terminateProcessTree(target, { platform: 'win32', graceMs: 50, loadJobAddon: () => fakeAddon });
  assert.equal(result.status, 'REAPED'); assert.equal(queriedWhileOpen, true); assert.equal(jobOpen, false);
  assert.deepEqual(readManagedProcessResources(target, { platform: 'win32', loadJobAddon: () => { throw new Error('post-reap addon call'); } }), { status: 'OK', ...usage });
});

test('Windows resource query returns cumulative Job Object CPU including exited descendants', () => {
  const usage = { cpuUserMs: 17, cpuKernelMs: 5, cpuTotalMs: 22, peakCommittedBytes: 123456, totalProcesses: 3, activeProcesses: 1 };
  const fakeAddon = { assignRunningProcessToNewJob: () => ({ processHandle: 'ph', jobHandle: 'jh' }), queryJobResourceUsage: (handle) => { assert.equal(handle, 'jh'); return usage; } };
  const target = spawnManagedProcess('binary', [], { platform: 'win32', loadJobAddon: () => fakeAddon }, () => Object.assign(new EventEmitter(), { pid: 901 }));
  assert.deepEqual(readManagedProcessResources(target, { platform: 'win32', loadJobAddon: () => fakeAddon }), { status: 'OK', ...usage });
});

test('Windows short-lived final resource query is retained after process-handle close', async () => {
  const usage = { cpuUserMs: 2, cpuKernelMs: 1, cpuTotalMs: 3, peakCommittedBytes: 4096, totalProcesses: 2, activeProcesses: 0 };
  let closed = false;
  const fakeAddon = {
    assignRunningProcessToNewJob: () => ({ processHandle: 'ph', jobHandle: 'jh' }),
    queryJobResourceUsage: () => usage,
    closeProcessHandle: () => { closed = true; },
  };
  const target = spawnManagedProcess('binary', [], { platform: 'win32', loadJobAddon: () => fakeAddon }, () => Object.assign(new EventEmitter(), { pid: 902, exitCode: 0 }));
  target.emit('close', 0, null);
  assert.equal(closed, true);
  await terminateProcessTree(target, { platform: 'win32', graceMs: 1, loadJobAddon: () => fakeAddon });
  assert.deepEqual(readManagedProcessResources(target, { platform: 'win32', loadJobAddon: () => { throw new Error('addon reloaded'); } }), { status: 'OK', ...usage });
});

test('Windows resource samples stay live then replace close snapshot with final termination sample', async () => {
  const samples = [10, 20, 30, 40]; let queries = 0; let closed = false;
  const usage = (cpuTotalMs) => ({ cpuUserMs: cpuTotalMs, cpuKernelMs: 0, cpuTotalMs, peakCommittedBytes: 1, totalProcesses: 1, activeProcesses: 0 });
  const fakeAddon = {
    assignRunningProcessToNewJob: () => ({ processHandle: 'ph', jobHandle: 'jh' }),
    queryJobResourceUsage: () => usage(samples[queries++]),
    closeProcessHandle: () => { closed = true; },
  };
  const target = spawnManagedProcess('binary', [], { platform: 'win32', loadJobAddon: () => fakeAddon }, () => Object.assign(new EventEmitter(), { pid: 905, exitCode: 0 }));
  assert.equal(readManagedProcessResources(target, { platform: 'win32', loadJobAddon: () => fakeAddon }).cpuTotalMs, 10);
  assert.equal(readManagedProcessResources(target, { platform: 'win32', loadJobAddon: () => fakeAddon }).cpuTotalMs, 20);
  target.emit('close', 0, null);
  assert.equal(closed, true);
  await terminateProcessTree(target, { platform: 'win32', graceMs: 1, loadJobAddon: () => fakeAddon });
  assert.equal(readManagedProcessResources(target, { platform: 'win32', loadJobAddon: () => { throw new Error('post-reap addon call'); } }).cpuTotalMs, 40);
  assert.equal(queries, 4);
});

test('Windows resource query types unavailable platform and addon', () => {
  const target = child();
  assert.deepEqual(readManagedProcessResources(target, { platform: 'linux' }), { status: 'UNAVAILABLE', reason: 'process-resources-platform-unsupported' });
  const assigned = spawnManagedProcess('binary', [], { platform: 'win32', loadJobAddon: () => ({ assignRunningProcessToNewJob: () => ({ processHandle: 'ph', jobHandle: 'jh' }) }) }, () => Object.assign(new EventEmitter(), { pid: 904 }));
  assert.deepEqual(readManagedProcessResources(assigned, { platform: 'win32', loadJobAddon: () => null }), { status: 'UNAVAILABLE', reason: 'process-resources-addon-unavailable' });
});

test('Windows resource query reports native query errors', () => {
  const fakeAddon = { assignRunningProcessToNewJob: () => ({ processHandle: 'ph', jobHandle: 'jh' }), queryJobResourceUsage: () => { throw new Error('query failed'); } };
  const target = spawnManagedProcess('binary', [], { platform: 'win32', loadJobAddon: () => fakeAddon }, () => Object.assign(new EventEmitter(), { pid: 903 }));
  assert.deepEqual(readManagedProcessResources(target, { platform: 'win32', loadJobAddon: () => fakeAddon }), { status: 'ERROR', reason: 'process-resources-query-failed', error: 'query failed' });
});

test('macOS resource query reports native CPU with physical-footprint memory semantics', () => {
  const usage = { cpuUserMs: 17, cpuKernelMs: 5, cpuTotalMs: 22, peakMemoryBytes: 123456, totalProcesses: 3, activeProcesses: 1 };
  const addon = { queryProcessTreeResourceUsage: (pid) => { assert.equal(pid, 910); return usage; } };
  const target = spawnManagedProcess('binary', [], { platform: 'darwin', loadResourceAddon: () => addon }, () => Object.assign(new EventEmitter(), { pid: 910 }));
  assert.deepEqual(readManagedProcessResources(target, { platform: 'darwin', loadResourceAddon: () => addon }), { status: 'OK', ...usage, memoryKind: 'physical-footprint' });
});

test('macOS final RESOURCE snapshot survives launcher close & addon loss', () => {
  const usage = { cpuUserMs: 2, cpuKernelMs: 1, cpuTotalMs: 3, peakMemoryBytes: 4096, totalProcesses: 2, activeProcesses: 0 };
  const target = spawnManagedProcess('binary', [], { platform: 'darwin', loadResourceAddon: () => ({ queryProcessTreeResourceUsage: () => { throw new Error('query after final'); } }) }, () => Object.assign(new EventEmitter(), { pid: 911, exitCode: 0 }));
  target.emit('message', { type: 'RESOURCE_FINAL', snapshot: usage });
  target.emit('close', 0, null);
  assert.deepEqual(readManagedProcessResources(target, { platform: 'darwin', loadResourceAddon: () => null }), { status: 'OK', ...usage, memoryKind: 'physical-footprint' });
});

test('macOS forced close without final RESOURCE stays explicitly unavailable', () => {
  const target = spawnManagedProcess('binary', [], { platform: 'darwin', loadResourceAddon: () => null }, () => Object.assign(new EventEmitter(), { pid: 912, exitCode: 137 }));
  target.emit('message', { type: 'RESOURCE', snapshot: { cpuUserMs: 8, cpuKernelMs: 1, cpuTotalMs: 9, peakMemoryBytes: 8192, totalProcesses: 1, activeProcesses: 1 } });
  target.emit('close', null, 'SIGKILL');
  assert.deepEqual(readManagedProcessResources(target, { platform: 'darwin', loadResourceAddon: () => null }), { status: 'UNAVAILABLE', reason: 'process-resources-final-sample-unavailable' });
});

test('Windows terminate falls back to taskkill for a child never spawned through spawnManagedProcess, even with the addon present', async () => {
  const target = child();
  const calls = [];
  const spawn = (_command, argv) => { calls.push(argv); const taskkill = new EventEmitter(); queueMicrotask(() => taskkill.emit('close', 0)); if (argv.includes('/F')) { target.exitCode = 1; queueMicrotask(() => target.emit('close', 1)); } return taskkill; };
  const fakeAddon = { terminateJob: () => { throw new Error('must not be called: this child has no recorded job handle'); } };
  const result = await terminateProcessTree(target, { platform: 'win32', graceMs: 1, spawn, loadJobAddon: () => fakeAddon });
  assert.equal(result.status, 'REAP_PENDING'); assert.equal(result.reason, 'windows-job-object-unsupported');
});

test('Windows persisted reap verifies creation-time identity before touching an unrelated reused pid', async () => {
  const fakeAddon = { isPidAlive: () => true, assignRunningProcessToNewJob: () => { throw new Error('must not terminate: identity does not match'); } };
  const result = await reapPersistedProcessTree(
    { pid: 999, platform: 'win32', identity: 'win32:999:20260101000000.000000' },
    { platform: 'win32', loadJobAddon: () => fakeAddon, readIdentity: async () => ({ status: 'OK', identity: 'win32:999:20260102000000.000000' }) },
  );
  assert.equal(result.status, 'REAP_PENDING'); assert.equal(result.reason, 'process-identity-mismatch');
});

test('Windows persisted reap is a no-op when the OS already tore the job down on the prior broker’s exit', async () => {
  const fakeAddon = { isPidAlive: () => false, assignRunningProcessToNewJob: () => { throw new Error('must not be called: already dead'); } };
  const result = await reapPersistedProcessTree(
    { pid: 1000, platform: 'win32', identity: 'win32:1000:t' },
    { platform: 'win32', loadJobAddon: () => fakeAddon, readIdentity: async () => { throw new Error('dead PID must not require an identity subprocess'); } },
  );
  assert.deepEqual(result, { status: 'REAPED', reaped: true, escalated: false });
});

test('Windows persisted reap terminates the verified survivor via a fresh job assignment', async () => {
  const calls = []; let alive = true;
  const fakeAddon = {
    isPidAlive: (pid) => { calls.push(['isPidAlive', pid]); return alive; },
    assignRunningProcessToNewJob: (pid) => { calls.push(['assign', pid]); return { jobHandle: 'jh' }; },
    terminateJob: (handle) => { calls.push(['terminateJob', handle]); alive = false; },
  };
  const result = await reapPersistedProcessTree(
    { pid: 1001, platform: 'win32', identity: 'win32:1001:t' },
    { platform: 'win32', graceMs: 100, loadJobAddon: () => fakeAddon, readIdentity: async () => ({ status: 'OK', identity: 'win32:1001:t' }) },
  );
  assert.deepEqual(result, { status: 'REAPED', reaped: true, escalated: true });
  assert.deepEqual(calls[0], ['isPidAlive', 1001]);
  assert.deepEqual(calls[1], ['assign', 1001]);
  assert.deepEqual(calls[2], ['terminateJob', 'jh']);
});

const capturePayload = (options) => {
  let captured = null;
  spawnManagedProcess('/managed/sccache', [], options, (_command, argv) => { captured = JSON.parse(Buffer.from(argv.at(-1), 'base64url').toString('utf8')); return Object.assign(new EventEmitter(), { pid: 41, once() { return this; } }); });
  return captured;
};

test('an explicit umask reaches the launcher payload & stays out of spawn options', () => {
  let spawnOptions = null;
  spawnManagedProcess('/managed/sccache', [], { platform: 'darwin', umask: 0o177, env: { A: '1' } }, (_command, argv, options) => {
    spawnOptions = options;
    assert.equal(JSON.parse(Buffer.from(argv.at(-1), 'base64url').toString('utf8')).umask, 0o177);
    return Object.assign(new EventEmitter(), { pid: 41, once() { return this; } });
  });
  assert.equal(spawnOptions.umask, undefined);
  assert.deepEqual(spawnOptions.env, { A: '1' });
});

test('an absent umask leaves the launcher payload byte-identical to today', () => {
  assert.deepEqual(capturePayload({ platform: 'darwin' }), { command: '/managed/sccache', argv: [] });
  assert.deepEqual(capturePayload({ platform: 'darwin', umask: undefined }), { command: '/managed/sccache', argv: [] });
  assert.deepEqual(capturePayload({ platform: 'darwin', umask: 'not-an-integer' }), { command: '/managed/sccache', argv: [] });
});

test('kill-on-disconnect is explicit launcher payload state, not a spawn option', () => {
  let spawnOptions = null;
  const payload = capturePayload({ platform: 'darwin', killOnDisconnect: true, env: { A: '1' } });
  spawnManagedProcess('/managed/sccache', [], { platform: 'darwin', killOnDisconnect: true }, (_command, _argv, options) => { spawnOptions = options; return Object.assign(new EventEmitter(), { pid: 41, once() { return this; } }); });
  assert.equal(payload.killOnDisconnect, true);
  assert.equal(spawnOptions.killOnDisconnect, undefined);
});

test('Windows job root gets a windowless console that the whole job tree inherits', () => {
  let options;
  const launcher = Object.assign(new EventEmitter(), { pid: 71 });
  spawnManagedProcess('cargo', ['test'], { platform: 'win32', stdio: ['ignore', 'pipe', 'pipe'], loadJobAddon: () => ({ assignRunningProcessToNewJob: () => ({ processHandle: 1 }), closeProcessHandle: () => {} }) }, (_command, _argv, spawnOptions) => { options = spawnOptions; return launcher; });
  assert.equal(options.windowsHide, true); assert.equal(options.detached, false);
  assert.equal(options.stdio.some((entry) => entry === 'inherit' || Number.isInteger(entry)), false); // inherited stdio would suppress CREATE_NO_WINDOW
});

test('managed launcher stdin is the null device unless a job pipes a credential', () => {
  const seen = [];
  const addon = () => ({ assignRunningProcessToNewJob: () => ({ processHandle: 1 }), closeProcessHandle: () => {} });
  const capture = (_command, _argv, spawnOptions) => { seen.push(spawnOptions.stdio[0]); return Object.assign(new EventEmitter(), { pid: 72 }); };
  spawnManagedProcess('cargo', ['build'], { platform: 'win32', stdio: ['ignore', 'pipe', 'pipe'], loadJobAddon: addon }, capture);
  spawnManagedProcess('cargo', ['login'], { platform: 'win32', stdio: ['pipe', 'pipe', 'pipe'], loadJobAddon: addon }, capture);
  spawnManagedProcess('cargo', ['build'], { platform: 'win32', stdio: ['inherit', 'pipe', 'pipe'], loadJobAddon: addon }, capture);
  assert.deepEqual(seen, ['ignore', 'pipe', 'ignore']);
});
