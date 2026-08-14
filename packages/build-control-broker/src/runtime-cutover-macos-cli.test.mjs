import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { parseMacOSCutoverArgs, proveLegacyOwnershipAbsent, proveV1Drained, runMacOSCutover } from './runtime-cutover-macos-cli.mjs';

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'rk-cutover-cli-'));
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
