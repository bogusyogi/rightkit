import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { parseMacOSCutoverArgs, proveLegacyOwnershipAbsent, proveManagedRequestsDrained, proveV1Drained, runMacOSCutover } from './runtime-cutover-macos-cli.mjs';

async function fixture() {
  // realpath so the fixture matches the canonicalized paths the CLI compares against.
  const root = await realpath(await mkdtemp(join(tmpdir(), 'rk-cutover-cli-')));
  const external = join(root, 'external'); const source = join(root, 'packages'); const health = join(root, 'health');
  await mkdir(external); await mkdir(source); await mkdir(health); await writeFile(join(health, 'Cargo.toml'), '[package]\nname = "rk-cutover-health"\nversion = "0.0.0"\nedition = "2021"\n');
  const config = join(root, 'broker.json'); const plist = join(root, 'broker.plist'); const v1 = join(root, 'v1.json'); const endpoint = join(external, 'broker.sock');
  await writeFile(config, JSON.stringify({ broker: { externalRoot: { path: external, volumeId: 'test', mounted: true, writable: true, internal: false }, stateFile: join(external, 'state.json'), receiptsDir: join(external, 'receipts'), activationFencePath: join(external, 'activation-fence.json'), capacityBytes: 1 }, unixSocket: endpoint }));
  await writeFile(plist, `<plist><dict><key>Label</key><string>dev.rightkit.broker</string><key>ProgramArguments</key><array><string>${process.execPath}</string><string>${join(external, 'runtime-current', 'build-control-broker', 'src', 'broker.mjs')}</string><string>${config}</string></array></dict></plist>`);
  await writeFile(v1, JSON.stringify({ entries: [{ id: 'done', state: 'CANCELLED' }] }));
  return { root, external, source, health, config, plist, v1, endpoint };
}

test('argument parser rejects unknown switches & requires exact activation identity', () => {
  assert.throws(() => parseMacOSCutoverArgs(['--config', '/a']), /--external-root is required/);
  assert.throws(() => parseMacOSCutoverArgs(['--nope', '/x']), /unknown/);
});

test('v1 drain proof rejects any non-terminal request', async () => {
  await assert.rejects(proveV1Drained('/state', { readFile: async () => JSON.stringify({ entries: [{ state: 'ACTIVE' }] }) }), /non-terminal/);
});

test('legacy drain accepts only exact missing PID plus process group when identity is unavailable', async () => {
  const missing = (target, signal) => { assert.equal(signal, 0); throw Object.assign(new Error(`missing ${target}`), { code: 'ESRCH' }); };
  const proof = await proveLegacyOwnershipAbsent({ pid: 41, processGroupId: 41, platform: 'posix' }, { reapOwnership: async () => ({ reaped: false, reason: 'process-identity-missing' }), processKill: missing });
  assert.equal(proof.absent, true); assert.equal(proof.fallback, 'pid-pgid-esrch');
  const live = await proveLegacyOwnershipAbsent({ pid: 41, processGroupId: 41, platform: 'posix' }, { reapOwnership: async () => ({ reaped: false }), processKill: () => {} });
  assert.equal(live.absent, false);
});

test('cutover CLI proves exact input selection & rechecks v1 drain at activation', async () => {
  const item = await fixture();
  try {
    let options;
    const receipt = await runMacOSCutover({ configPath: item.config, externalRoot: item.external, sourcePackagesRoot: item.source, sourceRevision: 'a'.repeat(40), sourceTreeDigest: 'b'.repeat(40), version: 'v2-test', launchdLabel: 'dev.rightkit.broker', uid: 501, endpoint: item.endpoint, plistPath: item.plist, v1StatePath: item.v1, healthWorkspace: item.health, timeoutMs: 100 }, {
      platform: 'darwin',
      stopLegacy: async () => ({ stopped: true }),
      install: async (candidate) => { options = candidate; assert.equal((await candidate.drainProbe()).open, 0); return { runtime: join(item.external, 'runtime-test') }; },
    });
    assert.equal(receipt.v1Drain.open, 0); assert.equal(options.externalRoot, item.external); assert.equal(options.plistPath, item.plist);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('cutover CLI refuses config, plist, or v1-state identity drift before activation', async () => {
  const item = await fixture();
  try {
    await writeFile(item.v1, JSON.stringify({ entries: [{ id: 'live', state: 'ACTIVE' }] }));
    let installed = false;
    await assert.rejects(runMacOSCutover({ configPath: item.config, externalRoot: item.external, sourcePackagesRoot: item.source, sourceRevision: 'a'.repeat(40), sourceTreeDigest: 'b'.repeat(40), version: 'v2-test', launchdLabel: 'dev.rightkit.broker', uid: 501, endpoint: item.endpoint, plistPath: item.plist, v1StatePath: item.v1, healthWorkspace: item.health, timeoutMs: 100 }, { platform: 'darwin', stopLegacy: async () => ({ stopped: true }), install: async () => { installed = true; } }), /ownership/);
    assert.equal(installed, false);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('managed drain proof reports an empty or absent store as drained', () => {
  assert.deepEqual(proveManagedRequestsDrained('/absent.sqlite', { exists: () => false }).open, 0);
  const empty = proveManagedRequestsDrained('/present.sqlite', { exists: () => true, openDatabase: () => ({ prepare: () => ({ all: () => [] }), close() {} }) });
  assert.equal(empty.open, 0); assert.equal(empty.present, true);
});

test('managed drain proof refuses activation while a v2 request is still running', () => {
  const running = { exists: () => true, openDatabase: () => ({ prepare: () => ({ all: () => [{ id: 'dbf3486b', state: 'RUNNING' }] }), close() {} }) };
  assert.throws(() => proveManagedRequestsDrained('/present.sqlite', running), /in flight.*dbf3486b=RUNNING/s);
});

test('an in-flight managed build blocks cutover before launchd is ever stopped', async () => {
  const item = await fixture();
  try {
    let stopped = false; let installed = false;
    await assert.rejects(runMacOSCutover(
      { configPath: item.config, externalRoot: item.external, sourcePackagesRoot: item.source, sourceRevision: 'a'.repeat(40), sourceTreeDigest: 'b'.repeat(40), version: 'v2-test', launchdLabel: 'dev.rightkit.broker', uid: 501, endpoint: item.endpoint, plistPath: item.plist, v1StatePath: item.v1, healthWorkspace: item.health, timeoutMs: 100 },
      {
        platform: 'darwin',
        proveManagedDrain: () => { throw new Error('cannot activate while 1 managed request(s) are in flight: dbf3486b=RUNNING'); },
        stopLegacy: async () => { stopped = true; return { stopped: true }; },
        install: async () => { installed = true; }
      }
    ), /in flight/);
    // The whole point of the fix: the running build is never signalled.
    assert.equal(stopped, false);
    assert.equal(installed, false);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});
