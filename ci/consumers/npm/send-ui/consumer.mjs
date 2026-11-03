import { validateInstalledArtifact, fixtureEntry } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const recipeId = 'send-ui-wire-and-markup-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/send-ui');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/send-ui', rootResolvedUrl: root, entries: { '@rightkit/send-ui': root, '@rightkit/send-ui/send-ui.css': import.meta.resolve('@rightkit/send-ui/send-ui.css') } });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));
  const React = await import(fixtureEntry(import.meta.url, import.meta.resolve('react')));
  const { renderToStaticMarkup } = await import(fixtureEntry(import.meta.url, import.meta.resolve('react-dom/server')));
  const ui = await import(provenance.entries['@rightkit/send-ui']);
  const css = readFileSync(fileURLToPath(provenance.entries['@rightkit/send-ui/send-ui.css']), 'utf8');
  const h = React.createElement;

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  // Synthetic data only: no device, address or file is real, and no network is used.
  const CLIENT_METHODS = ['cancel', 'dismiss', 'refresh', 'respond', 'send', 'setSaveDir', 'snapshot', 'trustDefault', 'trustForget', 'trustLabel', 'trustList', 'trustSet', 'trustSetDefault'];

  await run('export-surface-exact-and-stylesheet-shipped', async () => {
    assert.deepEqual(Object.keys(ui).sort(), ['SendPanel', 'createSendClient', 'defaultWords', 'isSendError', 'useSend']);
    assert.deepEqual(Object.keys(manifest.exports), ['.', './send-ui.css']);
    assert.deepEqual(manifest.sideEffects, ['*.css']);
    assert.deepEqual(Object.keys(manifest.peerDependencies), ['react']);
    assert.equal(manifest.dependencies, undefined);
    for (const selector of ['.rk-send', '.rk-send__title', '.rk-send__list', '.rk-send__row', '.rk-send__card', '.rk-send__name', '.rk-send__actions', '.rk-send__muted', '.rk-send__warn', '.rk-send__message']) assert(css.includes(selector), selector);
    return { exports: Object.keys(ui).length, stylesheetBytes: css.length };
  });

  await run('default-words-and-typed-error-wording', async () => {
    const words = ui.defaultWords;
    const strings = ['accept', 'allow', 'ask', 'cancel', 'decline', 'deny', 'dismiss', 'forget', 'incoming', 'labelPlaceholder', 'localNetworkBlocked', 'message', 'nearby', 'newDevices', 'noTransfers', 'nobodyNearby', 'refresh', 'saveLabel', 'scanning', 'sendFiles', 'sendText', 'textPlaceholder', 'transfers', 'trust', 'unverified'];
    assert.deepEqual(Object.keys(words).sort(), [...strings, 'error', 'files', 'states'].sort());
    for (const key of strings) assert(typeof words[key] === 'string' && words[key].length > 0, key);
    assert.deepEqual(Object.keys(words.states).sort(), ['active', 'cancelled', 'declined', 'done', 'failed', 'waiting']);
    assert.equal(words.files(1), '1 file');
    assert.equal(words.files(0), '0 files');
    assert.equal(words.files(3), '3 files');
    assert.equal(words.error({ code: 'device_nearby' }), 'device nearby', 'a typed error is worded by its code with underscores as spaces');
    assert.equal(words.error({ code: 'save_dir_missing', path: '/synthetic' }), 'save dir missing');
    assert.equal(words.error(new Error('boom')), 'Something went wrong.');
    assert.equal(words.error(null), 'Something went wrong.');
    return { strings: strings.length, states: Object.keys(words.states).length };
  });

  await run('typed-error-guard', async () => {
    assert.equal(ui.isSendError({ code: 'device_nearby' }), true);
    assert.equal(ui.isSendError({ code: 'x', detail: 1 }), true);
    for (const value of [null, undefined, 'device_nearby', 42, {}, { code: 7 }, new Error('plain')]) assert.equal(ui.isSendError(value), false, String(value));
    return { guarded: 7 };
  });

  await run('client-maps-every-call-to-the-one-json-command', async () => {
    const calls = [];
    const snapshot = { product: 'FixtureRight', devices: [], incoming: [], transfers: [] };
    const replies = { snapshot, send: 'transfer-1', respond: true, cancel: false, trustList: [], trustDefault: 'deny' };
    const client = ui.createSendClient(async (command, args) => { calls.push({ command, args }); return replies[command]; });
    assert.deepEqual(Object.keys(client).sort(), CLIENT_METHODS);
    assert.equal(await client.snapshot(), snapshot, 'the reply is returned as given');
    assert.equal(await client.refresh(), undefined);
    const request = { target: 'fixture-fingerprint', paths: ['/synthetic/a.txt'], texts: ['hello'], clipboard: true };
    assert.equal(await client.send(request), 'transfer-1');
    assert.equal(await client.respond('incoming-1', true), true);
    assert.equal(await client.cancel('transfer-1'), false);
    assert.equal(await client.dismiss('transfer-1'), undefined);
    assert.equal(await client.setSaveDir('/synthetic/inbox'), undefined);
    assert.deepEqual(await client.trustList(), []);
    assert.equal(await client.trustDefault(), 'deny');
    assert.equal(await client.trustSet('fixture-fingerprint', 'allow'), undefined);
    assert.equal(await client.trustLabel('fixture-fingerprint', 'Desk'), undefined);
    assert.equal(await client.trustForget('fixture-fingerprint'), undefined);
    assert.equal(await client.trustSetDefault('ask'), undefined);
    assert.deepEqual(calls, [
      { command: 'snapshot', args: undefined },
      { command: 'refresh', args: undefined },
      { command: 'send', args: request },
      { command: 'respond', args: { id: 'incoming-1', accept: true } },
      { command: 'cancel', args: { id: 'transfer-1' } },
      { command: 'dismiss', args: { id: 'transfer-1' } },
      { command: 'setSaveDir', args: { path: '/synthetic/inbox' } },
      { command: 'trustList', args: undefined },
      { command: 'trustDefault', args: undefined },
      { command: 'trustSet', args: { fingerprint: 'fixture-fingerprint', state: 'allow' } },
      { command: 'trustLabel', args: { fingerprint: 'fixture-fingerprint', label: 'Desk' } },
      { command: 'trustForget', args: { fingerprint: 'fixture-fingerprint' } },
      { command: 'trustSetDefault', args: { state: 'ask' } },
    ]);
    assert.notEqual(calls[2].args, request, 'the request is copied, not passed by reference');
    const failing = ui.createSendClient(async () => { throw { code: 'device_nearby', fingerprint: 'fixture-fingerprint' }; });
    await assert.rejects(failing.trustForget('fixture-fingerprint'), error => ui.isSendError(error) && error.code === 'device_nearby', 'a typed error from the host reaches the caller unchanged');
    return { calls: calls.length };
  });

  await run('panel-server-markup-is-loading-state-and-issues-no-command', async () => {
    let commands = 0;
    const client = ui.createSendClient(async () => { commands += 1; });
    const render = props => renderToStaticMarkup(h(ui.SendPanel, props));
    assert.equal(render({ client }), '<div class="rk-send" aria-busy="true"></div>');
    assert.equal(render({ client, className: 'fixture-send' }), '<div class="fixture-send" aria-busy="true"></div>');
    assert.equal(render({ client, pickPaths: async () => [], words: { nearby: 'Fixture nearby' }, subscribe: () => () => {}, pollMs: 10 }), '<div class="rk-send" aria-busy="true"></div>', 'options do not change the loading markup');
    assert.equal(commands, 0, 'effects do not run on the server, so no command is issued before a snapshot is requested by a mounted tree');
    return { markup: 3 };
  });

  assert.equal(result.scenarios.length, 5);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
