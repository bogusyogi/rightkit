import assert from 'node:assert/strict';
import { mkdtemp, mkdir, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { parseMacOSCutoverArgs, proveThinRequestsDrained, runMacOSCutover } from './runtime-cutover-macos-cli.mjs';

async function fixture() {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'rk-cutover-cli-')));
  const external = join(root, 'external'); const source = join(root, 'packages'); const health = join(root, 'health');
  await mkdir(external); await mkdir(source); await mkdir(health); await writeFile(join(health, 'Cargo.toml'), '[package]\nname = "rk-cutover-health"\nversion = "0.0.0"\nedition = "2021"\n');
  const config = join(external, 'config.json'); const plist = join(root, 'broker.plist'); const endpoint = join(external, 'broker.sock');
  await writeFile(config, JSON.stringify({ broker: { externalRoot: { path: external, volumeId: 'test', mounted: true, writable: true, internal: false }, stateFile: join(external, 'state.json'), receiptsDir: join(external, 'receipts'), activationFencePath: join(external, 'activation-fence.json'), capacityBytes: 1 }, unixSocket: endpoint }));
  await writeFile(plist, `<plist><dict><key>Label</key><string>dev.rightkit.broker</string><key>ProgramArguments</key><array><string>${process.execPath}</string><string>${join(external, 'runtime-current', 'build-control-broker', 'src', 'broker.mjs')}</string><string>${config}</string></array></dict></plist>`);
  return { root, external, source, health, config, plist, endpoint };
}

const input = (item) => ({ configPath: item.config, externalRoot: item.external, controlRoot: item.external, sourcePackagesRoot: item.source, sourceRevision: 'a'.repeat(40), sourceTreeDigest: 'b'.repeat(40), version: 'v2-test', launchdLabel: 'dev.rightkit.broker', uid: 501, endpoint: item.endpoint, plistPath: item.plist, healthWorkspace: item.health, timeoutMs: 100 });
const drainDependencies = { restartManaged: async () => {}, guiDomainAvailable: async () => true, beginDrain: async () => ({ path: 'test', token: 'test' }), endDrain: async () => {}, proveBrokerDrain: async () => ({ open: 0, inFlight: [] }) };

test('argument parser rejects unknown switches & requires exact activation identity', () => {
  assert.throws(() => parseMacOSCutoverArgs(['--config', '/a']), /--external-root is required/);
  assert.throws(() => parseMacOSCutoverArgs(['--nope', '/x']), /unknown/);
});

test('thin drain proof reports absent state & rejects active markers', async () => {
  const item = await fixture();
  try {
    assert.equal((await proveThinRequestsDrained(item.external)).open, 0);
    const active = join(item.external, 'rightkit-build-control', 'global', 'active'); await mkdir(active, { recursive: true }); await writeFile(join(active, 'job-1.json'), JSON.stringify({ id: 'job-1', workspaceKey: 'w' }));
    await assert.rejects(proveThinRequestsDrained(item.external), /in flight: job-1/);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('cutover proves exact input selection & rechecks thin drain at activation', async () => {
  const item = await fixture();
  try {
    let options;
    const receipt = await runMacOSCutover(input(item), { ...drainDependencies, platform: 'darwin', stopManaged: async () => ({ stopped: true }), install: async (candidate) => { options = candidate; assert.equal((await candidate.drainProbe()).open, 0); return { runtime: join(item.external, 'runtime-test') }; } });
    assert.equal(receipt.thinDrain.open, 0); assert.equal(options.externalRoot, item.external); assert.equal(options.plistPath, item.plist);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('in-flight thin build blocks cutover before launchd is stopped', async () => {
  const item = await fixture();
  try {
    let stopped = false; let installed = false;
    await assert.rejects(runMacOSCutover(input(item), { ...drainDependencies, platform: 'darwin', proveThinDrain: async () => { throw new Error('cannot activate while 1 thin request(s) are in flight: job-1'); }, stopManaged: async () => { stopped = true; }, install: async () => { installed = true; } }), /in flight/);
    assert.equal(stopped, false); assert.equal(installed, false);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('cutover fence waits boundedly for existing requests to drain', async () => {
  const item = await fixture();
  try {
    let attempts = 0; let sleeps = 0; let stopped = false;
    const receipt = await runMacOSCutover({ ...input(item), drainTimeoutMs: 5_000 }, {
      ...drainDependencies, platform: 'darwin', now: () => attempts * 100,
      sleep: async () => { sleeps += 1; },
      proveBrokerDrain: async () => {
        attempts += 1;
        if (attempts === 1) throw new Error('cannot activate while 1 broker request(s) are in flight: job-1');
        return { open: 0, inFlight: [] };
      },
      proveThinDrain: async () => ({ open: 0, inFlight: [] }),
      stopManaged: async () => { stopped = true; return { stopped: true }; },
      install: async () => ({ runtime: join(item.external, 'runtime-test') }),
    });
    assert.equal(sleeps, 1); assert.equal(stopped, true); assert.equal(receipt.brokerDrain.open, 0);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('cutover refuses plist identity drift before activation', async () => {
  const item = await fixture();
  try {
    await writeFile(item.plist, '<plist><dict><key>Label</key><string>wrong</string></dict></plist>');
    await assert.rejects(runMacOSCutover(input(item), { ...drainDependencies, platform: 'darwin', stopManaged: async () => {}, install: async () => {} }), /Label differs/);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('macOS cutover refuses before stopping the broker when no GUI login session exists', async () => {
  const item = await fixture();
  try {
    let stopped = false;
    await assert.rejects(runMacOSCutover(input(item), { ...drainDependencies, platform: 'darwin', guiDomainAvailable: async () => false, stopManaged: async () => { stopped = true; }, install: async () => {} }), /no GUI login session/);
    assert.equal(stopped, false);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('macOS cutover restarts the stopped LaunchAgent when install fails before the runtime switch', async () => {
  const item = await fixture();
  try {
    const calls = [];
    const error = await runMacOSCutover(input(item), { ...drainDependencies, platform: 'darwin', stopManaged: async () => { calls.push('stop'); return { stopped: true }; }, restartManaged: async () => { calls.push('restart'); }, install: async () => { throw new Error('runtime source packages root is dirty'); } }).catch((caught) => caught);
    assert.match(error.message, /dirty/); assert.equal(error.brokerRestored, true); assert.deepEqual(calls, ['stop', 'restart']);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});

test('restartManagedLaunchd enables, loads and kickstarts the LaunchAgent', async () => {
  const { restartManagedLaunchd } = await import('./runtime-cutover-macos-cli.mjs');
  const calls = [];
  await restartManagedLaunchd({ launchdLabel: 'L', uid: 501, plistPath: '/p.plist', execFile: async (file, args) => { calls.push([file, ...args]); } });
  assert.deepEqual(calls, [['launchctl', 'enable', 'gui/501/L'], ['launchctl', 'bootstrap', 'gui/501', '/p.plist'], ['launchctl', 'kickstart', 'gui/501/L']]);
});

test('split-root cutover accepts spaces & stages internally while draining external data', async () => {
  const item = await fixture(); const control = join(item.root, 'Application Support', 'RightKit');
  await mkdir(control, { recursive: true });
  const config = JSON.parse(await (await import('node:fs/promises')).readFile(item.config, 'utf8'));
  config.nodePath = process.execPath; config.broker.controlRoot = control;
  config.broker.stateFile = join(control, 'state.json'); config.broker.activationFencePath = join(control, 'activation-fence.json');
  config.unixSocket = join(control, 'broker.sock');
  item.config = join(control, 'config.json');
  await writeFile(item.config, JSON.stringify(config));
  await writeFile(item.plist, `<plist><dict><key>Label</key><string>dev.rightkit.broker</string><key>ProgramArguments</key><array><string>${process.execPath}</string><string>${join(control, 'runtime-current', 'build-control-broker', 'src', 'broker.mjs')}</string><string>${item.config}</string></array></dict></plist>`);
  try {
    let installed; let fenced; let drained;
    await runMacOSCutover({ ...input(item), controlRoot: control, endpoint: config.unixSocket }, {
      ...drainDependencies, platform: 'darwin', beginDrain: async (root) => { fenced = root; return {}; },
      proveBrokerDrain: async (root) => { assert.equal(root, control); return { open: 0 }; },
      proveThinDrain: async (root) => { drained = root; return { open: 0 }; },
      stopManaged: async () => ({ stopped: true }), install: async (options) => { installed = options; await options.drainProbe(); return {}; },
    });
    assert.equal(installed.externalRoot, control); assert.equal(installed.config.broker.externalRoot.path, item.external);
    assert.equal(fenced, control); assert.equal(drained, item.external);
  } finally { await rm(item.root, { recursive: true, force: true }); }
});
