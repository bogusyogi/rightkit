import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

const recipeId = 'tauri-host-adapter-command-contract-consumer';
const result = {
  schemaVersion: 1, recipeId, ok: false, nativeHostQualified: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, scenarios: [], errors: [],
};
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
// Fixture failures report fixed scenario names only: never serialize an untrusted host error.
let stage = 'artifact-provenance';
try {
  const archive = required('RIGHTKIT_CONSUMER_ARTIFACT');
  assert(archive.startsWith('/') || /^[A-Za-z]:[\\/]/.test(archive), 'artifact must be absolute');
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/tauri');
  const dialog = import.meta.resolve('@rightkit/tauri/dialog');
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/tauri', rootResolvedUrl: root,
    entries: { '@rightkit/tauri': root, '@rightkit/tauri/dialog': dialog },
  });
  result.artifact = provenance.artifact;
  const tauri = await import(provenance.entries['@rightkit/tauri']);
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };
  // Fixture invoke: records every command and returns a value derived from the command name only.
  const fixtureHost = () => {
    const calls = [];
    const invoke = async (command, args) => { calls.push({ command, args }); return `ok:${command}`; };
    return { calls, invoke };
  };

  await run('standard-command-contract-matches-logs-package', async () => {
    assert.deepEqual(tauri.standardRightKitTauriCommands, {
      logs: { write: 'rightkit_logs_write', collect: 'rightkit_logs_collect', clear: 'rightkit_logs_clear' },
      diagnostics: { collect: 'collect_diagnostics', clear: 'clear_diagnostic_logs' },
    });
    assert.equal(tauri.standardRightKitDeviceGetOrCreate.command, 'secure_device_id_get_or_create');
    return { logsCommands: 3, diagnosticsCommands: 2, deviceCommand: true };
  });

  await run('logs-write-uses-named-argument', async () => {
    const host = fixtureHost();
    const client = tauri.createRightKitTauriClient({ invoke: host.invoke });
    const events = [{ level: 'info', message: 'fixture-event' }];
    await client.logs.write(events);
    assert.deepEqual(host.calls, [{ command: 'rightkit_logs_write', args: { events } }]);
    return { writes: host.calls.length, argumentName: 'events' };
  });

  await run('collect-and-clear-route-to-standard-commands', async () => {
    const host = fixtureHost();
    const client = tauri.createRightKitTauriClient({ invoke: host.invoke });
    assert.equal(await client.logs.collect(), 'ok:rightkit_logs_collect');
    assert.equal(await client.logs.clear(), 'ok:rightkit_logs_clear');
    assert.equal(await client.diagnostics.collect(), 'ok:collect_diagnostics');
    assert.equal(await client.diagnostics.clear(), 'ok:clear_diagnostic_logs');
    assert.deepEqual(host.calls.map(call => call.command), ['rightkit_logs_collect', 'rightkit_logs_clear', 'collect_diagnostics', 'clear_diagnostic_logs']);
    return { commands: host.calls.length };
  });

  await run('device-identity-unsupported-until-native-command', async () => {
    const unconfigured = fixtureHost();
    await assert.rejects(tauri.createRightKitTauriClient({ invoke: unconfigured.invoke }).device.getOrCreate(), /unsupported/);
    assert.equal(unconfigured.calls.length, 0);
    const configured = fixtureHost();
    const client = tauri.createRightKitTauriClient({ invoke: configured.invoke, deviceGetOrCreate: tauri.standardRightKitDeviceGetOrCreate });
    assert.equal(await client.device.getOrCreate(), 'ok:secure_device_id_get_or_create');
    assert.equal(configured.calls.at(-1).command, 'secure_device_id_get_or_create');
    return { unconfiguredInvokes: 0, configuredInvokes: configured.calls.length };
  });

  await run('invalid-config-refused-and-updates-passed-through', async () => {
    const host = fixtureHost();
    assert.throws(() => tauri.createRightKitTauriClient({ invoke: host.invoke, argumentNames: { logsWrite: 'bad-name' } }), /argument name/);
    assert.throws(() => tauri.createRightKitTauriClient({ invoke: host.invoke, commands: { logs: { write: '', collect: 'c', clear: 'd' }, diagnostics: { collect: 'e', clear: 'f' } } }));
    assert.equal(host.calls.length, 0);
    const updates = Object.freeze({ fixture: true });
    assert.equal(tauri.createRightKitTauriClient({ invoke: host.invoke, updates }).updates, updates);
    return { refusedConfigs: 2, invokesOnRefusal: 0, updatesPassThrough: true };
  });

  await run('dialog-subpath-bound-to-archive-without-plugin-load', async () => {
    const dialogFile = provenance.entries['@rightkit/tauri/dialog'];
    assert.match(dialogFile, /\/dist\/dialog\.js$/);
    assert.ok(readFileSync(new URL(dialogFile)).length > 0);
    return { dialogExportBound: true, pluginDialogLoaded: false };
  });

  stage = 'qualification-complete';
  result.ok = true;
} catch {
  result.errors.push({ stage, code: 'consumer-assertion-failed' });
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
