import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import {
  managedDatabasePath, parseWindowsCutoverArgs, proveLegacyOwnershipAbsent, proveManagedRequestsDrained, proveV1Drained, runWindowsCutover,
} from './runtime-cutover-windows-cli.mjs';
import { createWindowsRuntimeCallbacks, ensureWindowsTaskRegistered, renderTaskDefinition, withTaskDefinitionFile } from './runtime-cutover-windows.mjs';

async function fixture() {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'rk-win-cutover-cli-')));
  const external = join(root, 'external'); const source = join(root, 'packages'); const health = join(root, 'health');
  await mkdir(external); await mkdir(source); await mkdir(health);
  await writeFile(join(health, 'Cargo.toml'), '[package]\nname = "rk-cutover-health"\nversion = "0.0.0"\nedition = "2021"\n');
  const config = join(root, 'broker.json'); const v1 = join(root, 'v1.json'); const endpoint = '\\\\.\\pipe\\rightkit-build-control-test';
  await writeFile(config, JSON.stringify({
    broker: { externalRoot: { path: external, volumeId: 'test', mounted: true, writable: true, internal: false }, stateFile: join(external, 'state.json'), receiptsDir: join(external, 'receipts'), activationFencePath: join(external, 'activation-fence.json'), capacityBytes: 1 },
    windowsPipe: endpoint,
  }));
  await writeFile(v1, JSON.stringify({ entries: [{ id: 'done', state: 'CANCELLED' }] }));
  return { root, external, source, health, config, v1, endpoint };
}

test('argument parser rejects unknown switches & requires exact activation identity', () => {
  assert.throws(() => parseWindowsCutoverArgs(['--config', 'C:\\a']), /--external-root is required/);
  assert.throws(() => parseWindowsCutoverArgs(['--nope', 'x']), /unknown/);
});

test('v1 drain proof rejects any non-terminal request', async () => {
  await assert.rejects(proveV1Drained('/state', { readFile: async () => JSON.stringify({ entries: [{ state: 'ACTIVE' }] }) }), /non-terminal/);
});

test('v1 drain proof strips a leading UTF-8 BOM instead of failing opaquely', async () => {
  const result = await proveV1Drained('/state', { readFile: async () => `﻿${JSON.stringify({ entries: [{ state: 'RELEASED' }] })}` });
  assert.equal(result.open, 0);
});

test('legacy ownership is proven absent by exact PID liveness alone', async () => {
  const missing = await proveLegacyOwnershipAbsent({ pid: 999_999 });
  assert.equal(missing.absent, true); assert.equal(missing.fallback, 'pid-esrch');
  const live = await proveLegacyOwnershipAbsent({ pid: process.pid });
  assert.equal(live.absent, false);
});

test('managed drain proof refuses activation while a v2 request is still running', () => {
  const running = { exists: () => true, openDatabase: () => ({ prepare: () => ({ all: () => [{ id: 'abc123', state: 'RUNNING' }] }), close() {} }) };
  assert.throws(() => proveManagedRequestsDrained('/present.sqlite', running), /in flight.*abc123=RUNNING/s);
});

test('managed drain proof reports an empty or absent store as drained', () => {
  assert.equal(proveManagedRequestsDrained('/absent.sqlite', { exists: () => false }).open, 0);
  const empty = proveManagedRequestsDrained('/present.sqlite', { exists: () => true, openDatabase: () => ({ prepare: () => ({ all: () => [] }), close() {} }) });
  assert.equal(empty.open, 0); assert.equal(empty.present, true);
});

test('managedDatabasePath derives from a Windows-style stateFile path', () => {
  const derived = managedDatabasePath({ broker: { stateFile: 'C:\\rightkit\\state\\broker.json' } });
  assert.match(derived, /broker\.json\.sqlite$/);
});

test('an in-flight managed build blocks cutover before the scheduled task is ever stopped', async () => {
  const item = await fixture();
  try {
    let stopped = false; let installed = false;
    await assert.rejects(runWindowsCutover(
      { configPath: item.config, externalRoot: item.external, sourcePackagesRoot: item.source, sourceRevision: 'a'.repeat(40), sourceTreeDigest: 'b'.repeat(40), version: 'v2-test', taskName: 'RightKitBuildControl', endpoint: item.endpoint, v1StatePath: item.v1, healthWorkspace: item.health, timeoutMs: 100 },
      {
        platform: 'win32',
        proveManagedDrain: () => { throw new Error('cannot activate while 1 managed request(s) are in flight: abc123=RUNNING'); },
        stopLegacy: async () => { stopped = true; return { stopped: true }; },
        install: async () => { installed = true; },
      },
    ), /in flight/);
    // The whole point of the fix: the running build is never signalled.
    assert.equal(stopped, false);
    assert.equal(installed, false);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('cutover CLI proves exact input selection & rechecks v1 drain at activation', async () => {
  const item = await fixture();
  try {
    let options;
    const receipt = await runWindowsCutover(
      { configPath: item.config, externalRoot: item.external, sourcePackagesRoot: item.source, sourceRevision: 'a'.repeat(40), sourceTreeDigest: 'b'.repeat(40), version: 'v2-test', taskName: 'RightKitBuildControl', endpoint: item.endpoint, v1StatePath: item.v1, healthWorkspace: item.health, timeoutMs: 100 },
      {
        platform: 'win32',
        stopLegacy: async () => ({ stopped: true }),
        install: async (candidate) => { options = candidate; assert.equal((await candidate.drainProbe()).open, 0); return { taskName: 'RightKitBuildControl' }; },
      },
    );
    assert.equal(receipt.v1Drain.open, 0); assert.equal(options.externalRoot, item.external); assert.equal(options.configPath, item.config);
    assert.equal(options.taskXmlPath.endsWith('.xml'), true);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('cutover CLI refuses config or v1-state identity drift before activation', async () => {
  const item = await fixture();
  try {
    await writeFile(item.v1, JSON.stringify({ entries: [{ id: 'live', state: 'ACTIVE' }] }));
    let installed = false;
    await assert.rejects(runWindowsCutover(
      { configPath: item.config, externalRoot: item.external, sourcePackagesRoot: item.source, sourceRevision: 'a'.repeat(40), sourceTreeDigest: 'b'.repeat(40), version: 'v2-test', taskName: 'RightKitBuildControl', endpoint: item.endpoint, v1StatePath: item.v1, healthWorkspace: item.health, timeoutMs: 100 },
      { platform: 'win32', stopLegacy: async () => ({ stopped: true }), install: async () => { installed = true; } },
    ), /no verifiable pid.*ownership|non-terminal/);
    assert.equal(installed, false);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('cutover CLI refuses a windowsPipe mismatch before activation', async () => {
  const item = await fixture();
  try {
    await assert.rejects(runWindowsCutover(
      { configPath: item.config, externalRoot: item.external, sourcePackagesRoot: item.source, sourceRevision: 'a'.repeat(40), sourceTreeDigest: 'b'.repeat(40), version: 'v2-test', taskName: 'RightKitBuildControl', endpoint: '\\\\.\\pipe\\some-other-name', v1StatePath: item.v1, healthWorkspace: item.health, timeoutMs: 100 },
      { platform: 'win32', stopLegacy: async () => ({ stopped: true }), install: async () => {} },
    ), /windowsPipe differs/);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('renderTaskDefinition sets non-interactive logon, auto-restart, & hidden — the crash/window fixes', () => {
  const xml = renderTaskDefinition({ taskName: 'RightKitBuildControl', nodePath: 'C:\\node\\node.exe', brokerScript: 'C:\\rightkit\\broker.mjs', configPath: 'C:\\rightkit\\config.json' });
  // S4U, not InteractiveToken: node.exe is a console-subsystem binary & would
  // otherwise flash a visible window in the user's session on every restart.
  assert.match(xml, /<LogonType>S4U<\/LogonType>/);
  assert.match(xml, /<Hidden>true<\/Hidden>/);
  assert.match(xml, /<RestartOnFailure>[\s\S]*<Count>999<\/Count>[\s\S]*<\/RestartOnFailure>/);
  assert.match(xml, /<BootTrigger>/); assert.match(xml, /<LogonTrigger>/);
  assert.match(xml, /<Command>C:\\node\\node\.exe<\/Command>/);
});

test('renderTaskDefinition escapes reserved XML characters', () => {
  const xml = renderTaskDefinition({ taskName: 'x', nodePath: 'C:\\a & b\\node.exe', brokerScript: 'C:\\x<y', configPath: 'C:\\c.json' });
  assert.ok(!xml.includes('C:\\a & b\\node.exe'));
  assert.match(xml, /C:\\a &amp; b\\node\.exe/);
});

test('Task Scheduler XML is written as BOM-prefixed UTF-16LE', async () => {
  let bytes;
  await withTaskDefinitionFile('<Task/>', async (file) => { bytes = await readFile(file); });
  assert.deepEqual(bytes.subarray(0, 2), Buffer.from([0xff, 0xfe]));
  assert.equal(bytes.subarray(2).toString('utf16le'), '<Task/>');
});

test('fresh Windows cutover registers task before use & rollback handles absent or registered candidate', async () => {
  const calls = [];
  const execFile = async (command, args) => {
    calls.push([command, args]);
    if (args[0] === '/Query') throw new Error('missing task');
    return { stdout: '' };
  };
  await ensureWindowsTaskRegistered({ taskName: 'RightKitTest', taskXmlPath: '/tmp/task.xml', execFile });
  assert.deepEqual(calls.map(([, args]) => args[0]), ['/Query', '/Create']);
  const callbacks = createWindowsRuntimeCallbacks({ taskName: 'RightKitTest', endpoint: '\\\\.\\pipe\\rightkit-test', drainProbe: async () => ({ open: 0 }), execFile });
  await callbacks.rollbackService({ prior: null });
  assert.equal(calls.at(-1)[1][0], '/Query');
  const registeredCalls = [];
  const registeredExec = async (command, args) => { registeredCalls.push([command, args]); return { stdout: '' }; };
  const registeredCallbacks = createWindowsRuntimeCallbacks({ taskName: 'RightKitTest', endpoint: '\\\\.\\pipe\\rightkit-test', drainProbe: async () => ({ open: 0 }), execFile: registeredExec });
  await registeredCallbacks.rollbackService({ prior: null });
  assert.equal(registeredCalls.at(-1)[1][0], '/End');
});
