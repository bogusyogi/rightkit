import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { parseWindowsCutoverArgs, proveThinRequestsDrained, runWindowsCutover, stopManagedTask } from './runtime-cutover-windows-cli.mjs';
import { createWindowsRuntimeCallbacks, ensureWindowsTaskRegistered, renderBrokerLauncherVbs, renderTaskDefinition, selectWindowsNodePid, selectWindowsNodePidResolved, withTaskDefinitionFile } from './runtime-cutover-windows.mjs';

async function fixture() {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'rk-win-cutover-cli-')));
  const external = join(root, 'external'); const source = join(root, 'packages'); const health = join(root, 'health');
  await mkdir(external); await mkdir(source); await mkdir(health); await writeFile(join(health, 'Cargo.toml'), '[package]\nname = "rk-cutover-health"\nversion = "0.0.0"\nedition = "2021"\n');
  const config = join(root, 'broker.json'); const endpoint = '\\\\.\\pipe\\rightkit-build-control-test';
  await writeFile(config, JSON.stringify({ broker: { externalRoot: { path: external, volumeId: 'test', mounted: true, writable: true, internal: false }, stateFile: join(external, 'state.json'), receiptsDir: join(external, 'receipts'), activationFencePath: join(external, 'activation-fence.json'), capacityBytes: 1 }, windowsPipe: endpoint }));
  return { root, external, source, health, config, endpoint };
}

const input = (item, endpoint = item.endpoint) => ({ configPath: item.config, externalRoot: item.external, sourcePackagesRoot: item.source, sourceRevision: 'a'.repeat(40), sourceTreeDigest: 'b'.repeat(40), version: 'v2-test', taskName: 'RightKitBuildControl', endpoint, healthWorkspace: item.health, timeoutMs: 100 });
const drainDependencies = { beginDrain: async () => ({ path: 'test', token: 'test' }), endDrain: async () => {}, proveBrokerDrain: async () => ({ open: 0, inFlight: [] }) };

test('stopManagedTask polls until named pipe releases', async () => {
  let probes = 0;
  const netModule = { createConnection: () => { const open = probes++ < 2; const handlers = {}; queueMicrotask(() => (open ? handlers.connect?.() : handlers.error?.())); return { once: (event, fn) => { handlers[event] = fn; }, destroy() {} }; } };
  const result = await stopManagedTask({ taskName: 'T', endpoint: '\\\\.\\pipe\\x', execFile: async () => {}, netModule, pipeReleaseTimeoutMs: 5_000, pipePollIntervalMs: 1, sleep: async () => {} });
  assert.deepEqual(result, { stopped: true, target: 'T', listener: false }); assert.ok(probes >= 3);
});

test('stopManagedTask fails closed when named pipe remains live', async () => {
  const netModule = { createConnection: () => { const handlers = {}; queueMicrotask(() => handlers.connect?.()); return { once: (event, fn) => { handlers[event] = fn; }, destroy() {} }; } };
  await assert.rejects(stopManagedTask({ taskName: 'T', endpoint: '\\\\.\\pipe\\x', execFile: async () => {}, netModule, pipeReleaseTimeoutMs: 30, pipePollIntervalMs: 1, sleep: async () => {} }), /remains reachable/);
});

test('argument parser rejects unknown switches & requires exact activation identity', () => {
  assert.throws(() => parseWindowsCutoverArgs(['--config', 'C:\\a']), /--external-root is required/);
  assert.throws(() => parseWindowsCutoverArgs(['--nope', 'x']), /unknown/);
});

test('thin drain proof rejects active markers', async () => {
  const item = await fixture();
  try {
    assert.equal((await proveThinRequestsDrained(item.external)).open, 0);
    const active = join(item.external, 'rightkit-build-control', 'global', 'active'); await mkdir(active, { recursive: true }); await writeFile(join(active, 'job-1.json'), JSON.stringify({ id: 'job-1' }));
    await assert.rejects(proveThinRequestsDrained(item.external), /in flight: job-1/);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('in-flight thin build blocks cutover before scheduled task stops', async () => {
  const item = await fixture();
  try {
    let stopped = false; let installed = false;
    await assert.rejects(runWindowsCutover(input(item), { ...drainDependencies, platform: 'win32', proveThinDrain: async () => { throw new Error('cannot activate while 1 thin request(s) are in flight: job-1'); }, stopManaged: async () => { stopped = true; }, install: async () => { installed = true; } }), /in flight/);
    assert.equal(stopped, false); assert.equal(installed, false);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('cutover proves input selection & rechecks thin drain', async () => {
  const item = await fixture();
  try {
    let options;
    const receipt = await runWindowsCutover(input(item), { ...drainDependencies, platform: 'win32', stopManaged: async () => ({ stopped: true }), install: async (candidate) => { options = candidate; assert.equal((await candidate.drainProbe()).open, 0); assert.match(await readFile(candidate.taskXmlPath, 'utf16le'), /wscript\.exe/); assert.match(await readFile(join(item.external, 'broker-launch.vbs'), 'utf8'), new RegExp(process.execPath.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))); return { taskName: 'RightKitBuildControl' }; } });
    assert.equal(receipt.thinDrain.open, 0); assert.equal(options.externalRoot, item.external); assert.equal(options.nodePath, process.execPath); assert.equal(options.taskXmlPath.endsWith('.xml'), true);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('cutover refuses named-pipe drift before activation', async () => {
  const item = await fixture();
  try { await assert.rejects(runWindowsCutover(input(item, '\\\\.\\pipe\\wrong'), { ...drainDependencies, platform: 'win32', stopManaged: async () => {}, install: async () => {} }), /windowsPipe differs/); }
  finally { await rm(item.root, { recursive: true, force: true }); }
});

test('task definition uses current-user interactive token & logon restart policy', () => {
  const xml = renderTaskDefinition({ taskName: 'RightKitBuildControl', nodePath: 'C:\\node\\node.exe', brokerScript: 'C:\\rightkit\\broker.mjs', configPath: 'C:\\rightkit\\config.json' });
  assert.match(xml, /<LogonType>InteractiveToken<\/LogonType>/); assert.doesNotMatch(xml, /<LogonType>S4U<\/LogonType>/); assert.match(xml, /<UserId>[^<]+<\/UserId>/);
  // Launcher mode: the task must exec wscript with the hidden-window VBS, and
  // the rendered VBS must run node with window style 0.
  const launched = renderTaskDefinition({ taskName: 'RightKitBuildControl', nodePath: 'C:/node/node.exe', brokerScript: 'C:/rightkit/broker.mjs', configPath: 'C:/rightkit/config.json', launcherScript: 'D:/managed/broker-launch.vbs' });
  assert.match(launched, /wscript\.exe<\/Command>/);
  assert.match(launched, /broker-launch\.vbs/);
  assert.doesNotMatch(launched, /node\.exe<\/Command>/);
  const vbs = renderBrokerLauncherVbs({ nodePath: 'C:/node/node.exe', brokerScript: 'C:/rightkit/broker.mjs', configPath: 'C:/rightkit/config.json' });
  assert.match(vbs, /, 0, False/);
  assert.match(vbs, /node\.exe/); assert.match(xml, /<Hidden>true<\/Hidden>/); assert.match(xml, /<RestartOnFailure>[\s\S]*<Count>999<\/Count>/); assert.doesNotMatch(xml, /<BootTrigger>/); assert.match(xml, /<LogonTrigger>/);
});

test('task definition escapes XML & temporary file uses UTF-16LE BOM', async () => {
  assert.match(renderTaskDefinition({ taskName: 'x', nodePath: 'C:\\a & b\\node.exe', brokerScript: 'C:\\x<y', configPath: 'C:\\c.json' }), /C:\\a &amp; b\\node\.exe/);
  let bytes; await withTaskDefinitionFile('<Task/>', async (file) => { bytes = await readFile(file); });
  assert.deepEqual(bytes.subarray(0, 2), Buffer.from([0xff, 0xfe])); assert.equal(bytes.subarray(2).toString('utf16le'), '<Task/>');
});

test('Windows PID selection rejects shell/self matches', () => {
  const processes = [{ ProcessId: 712, Name: 'powershell.exe', CommandLine: 'config.json' }, { ProcessId: 913, Name: 'NODE.EXE', ExecutablePath: 'c:/PROGRAM FILES/nodejs/NODE.EXE', CommandLine: 'node broker.mjs config.json' }];
  assert.equal(selectWindowsNodePid(processes, 'C:\\Program Files\\nodejs\\node.exe'), 913); assert.equal(selectWindowsNodePid(processes[0], 'C:\\Program Files\\nodejs\\node.exe'), null);
});

test('Windows process proof excludes cutover CLI sharing node executable & config', async () => {
  const externalRoot = join(tmpdir(), 'rightkit-pid-test');
  const configPath = join(externalRoot, 'config.json');
  const runtime = join(externalRoot, 'runtime-candidate');
  const brokerScript = join(externalRoot, 'runtime-current', 'build-control-broker', 'src', 'broker.mjs');
  const brokerCommand = `"${process.execPath}" "${brokerScript}" "${configPath}"`;
  const processes = [
    { ProcessId: 38192, Name: 'node.exe', ExecutablePath: process.execPath, CommandLine: `"${process.execPath}" runtime-cutover-windows-cli.mjs --config "${configPath}"` },
    { ProcessId: 15392, Name: 'node.exe', ExecutablePath: process.execPath, CommandLine: brokerCommand },
  ];
  const callbacks = createWindowsRuntimeCallbacks({
    taskName: 'RightKitTest', externalRoot, configPath, endpoint: '\\\\.\\pipe\\rightkit-test', drainProbe: async () => ({ open: 0 }),
    execFile: async (_command, args) => ({ stdout: args.at(-1).includes('-ExpandProperty CommandLine') ? brokerCommand : JSON.stringify(processes) }),
  });
  assert.deepEqual(await callbacks.processProbe({ pid: 15392, runtime }), { alive: true, runtimePath: runtime, scriptPath: join(runtime, 'build-control-broker', 'src', 'broker.mjs') });
  assert.deepEqual(await callbacks.processProbe({ pid: 38192, runtime }), { alive: false });
});

test('Windows PID selection resolves an NVM junction before exact executable comparison', async () => {
  const physical = [{ ProcessId: 913, Name: 'node.exe', ExecutablePath: 'C:\\Users\\adrds\\AppData\\Local\\nvm\\v26.8.1\\node.exe' }];
  const junction = [{ ProcessId: 914, Name: 'node.exe', ExecutablePath: 'C:\\nvm4w\\nodejs\\node.exe' }];
  const resolvePath = async () => 'C:\\Users\\adrds\\AppData\\Local\\nvm\\v26.8.1\\node.exe';
  assert.equal(await selectWindowsNodePidResolved(physical, 'C:\\nvm4w\\nodejs\\node.exe', resolvePath), 913);
  assert.equal(await selectWindowsNodePidResolved(junction, 'C:\\nvm4w\\nodejs\\node.exe', resolvePath), 914);
  // Inverse direction: cutover launched with the physical versioned node while
  // the broker task spawns through the junction. The observed ExecutablePath
  // must be resolved too, or a healthy new broker is rejected & rolled back.
  const resolveJunctionOnly = async (value) => value.startsWith('C:\\nvm4w') ? 'C:\\Users\\adrds\\AppData\\Local\\nvm\\v26.8.1\\node.exe' : value;
  assert.equal(await selectWindowsNodePidResolved(junction, 'C:\\Users\\adrds\\AppData\\Local\\nvm\\v26.8.1\\node.exe', resolveJunctionOnly), 914);
});

test('fresh Windows cutover registers task & rollback ends registered candidate', async () => {
  const calls = []; const execFile = async (command, args) => { calls.push([command, args]); if (args[0] === '/Query') throw new Error('missing'); return { stdout: '' }; };
  assert.deepEqual(await ensureWindowsTaskRegistered({ taskName: 'RightKitTest', taskXmlPath: '/tmp/task.xml', execFile }), { registered: true, reused: false });
  assert.deepEqual(calls.map(([, args]) => args[0]), ['/Query', '/Create']);
  const registeredCalls = []; const callbacks = createWindowsRuntimeCallbacks({ taskName: 'RightKitTest', endpoint: '\\\\.\\pipe\\rightkit-test', drainProbe: async () => ({ open: 0 }), execFile: async (command, args) => { registeredCalls.push([command, args]); return { stdout: '' }; } });
  await callbacks.rollbackService({ prior: null }); assert.equal(registeredCalls.at(-1)[1][0], '/End');
});

test('existing Windows cutover re-enables task without elevation-only replacement', async () => {
  const calls = [];
  const result = await ensureWindowsTaskRegistered({
    taskName: 'RightKitTest',
    taskXmlPath: '/tmp/task.xml',
    execFile: async (command, args) => { calls.push([command, args]); return { stdout: '' }; },
  });
  assert.deepEqual(result, { registered: true, reused: true });
  assert.deepEqual(calls.map(([, args]) => args[0]), ['/Query', '/Change']);
  assert.deepEqual(calls[1][1].slice(-1), ['/ENABLE']);
});

test('Windows cutover waits (bounded) for in-flight work to drain, then activates', async () => {
  const item = await fixture();
  try {
    let clock = 0; let probes = 0; let stopped = false;
    const busy = async () => { probes += 1; if (probes < 3) throw new Error('cannot activate while 1 thin request(s) are in flight: job-1'); return { open: 0 }; };
    const receipt = await runWindowsCutover({ ...input(item), drainTimeoutMs: 10_000 }, { ...drainDependencies, platform: 'win32', now: () => clock, sleep: async (ms) => { clock += ms; }, proveThinDrain: busy, stopManaged: async () => { stopped = true; return { stopped: true }; }, install: async () => ({}) }).catch((error) => error);
    assert.equal(probes >= 3, true); assert.equal(stopped, true); assert.equal(clock, 2_000); void receipt;
    clock = 0; probes = 0; stopped = false;
    await assert.rejects(runWindowsCutover({ ...input(item), drainTimeoutMs: 1_500 }, { ...drainDependencies, platform: 'win32', now: () => clock, sleep: async (ms) => { clock += ms; }, proveThinDrain: async () => { throw new Error('cannot activate while 1 thin request(s) are in flight: job-1'); }, stopManaged: async () => { stopped = true; }, install: async () => {} }), /in flight/);
    assert.equal(stopped, false);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});
