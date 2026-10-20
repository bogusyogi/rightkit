import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const result = { recipeId: 'app-update-controller-native-dto-consumer', schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false, nativeSignatureQualified: false };
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
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/app-update', rootResolvedUrl: import.meta.resolve('@rightkit/app-update'), entries: { '@rightkit/app-update': import.meta.resolve('@rightkit/app-update'), '@rightkit/app-update/native': import.meta.resolve('@rightkit/app-update/native') } });
  result.artifact = provenance.artifact;
  const api = await import(provenance.entries['@rightkit/app-update']);
  const native = await import(provenance.entries['@rightkit/app-update/native']);
  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }
  function preparedUpdate(options = {}) {
    const calls = { download: 0, install: 0, combined: 0, artifactHeaders: [] };
    const update = {
      version: '9.8.7', target: 'fixture-target', rawJson: { tier: 'patch', digest: 'a'.repeat(64), signature: 'fixture-signature' },
      downloadAndInstall: async () => { calls.combined++; throw new Error('prepared adapter must not use combined path'); },
      download: async (onEvent, settings) => {
        calls.download++; calls.artifactHeaders.push(settings?.headers);
        onEvent?.({ event: 'Started', data: { contentLength: 10 } });
        onEvent?.({ event: 'Progress', data: { chunkLength: 10 } });
        if (options.failDownload) throw new Error('fixture download interrupted');
      },
      install: async () => { calls.install++; if (options.failInstall) throw new Error('fixture install interrupted'); },
    };
    return { update, calls };
  }
  await run('prepared-check-download-install-single-consumption-header-isolation', async () => {
    const fixture = preparedUpdate(), statuses = [], checks = [];
    const client = api.createAutoUpdateClient({ check: async options => { checks.push(options); return fixture.update; }, getHeaders: () => ({ Authorization: 'fixture-check-only' }), onStatus: status => statuses.push(status) });
    assert.equal((await client.checkOnce()).kind, 'available');
    const staged = await client.downloadAvailable();
    assert(staged.ok);
    assert.equal(staged.handle.prepared, true);
    assert.equal(staged.handle.target, 'fixture-target');
    assert.equal(staged.handle.digest, 'a'.repeat(64));
    assert.equal(fixture.calls.install, 0);
    assert.deepEqual(checks, [{ headers: { Authorization: 'fixture-check-only' } }]);
    assert.deepEqual(fixture.calls.artifactHeaders, [{}]);
    assert.equal(api.deriveUpdateOverlay(staged.status).actions.install, true);
    assert.equal((await client.installStaged(staged.handle)).kind, 'ready');
    assert.equal((await client.installStaged(staged.handle)).kind, 'error');
    assert.equal(fixture.calls.download, 1);
    assert.equal(fixture.calls.install, 1);
    assert.equal(fixture.calls.combined, 0);
    assert(statuses.some(status => status.kind === 'downloading' && status.pct === 100));
    return { statuses, calls: fixture.calls, prepared: staged.handle.prepared };
  });
  await run('discard-cancels-staged-handle-new-check-fences-old', async () => {
    const fixture = preparedUpdate();
    const client = api.createAutoUpdateClient({ check: async () => fixture.update });
    await client.checkOnce();
    const cancelled = await client.downloadAvailable(); assert(cancelled.ok);
    assert.equal(client.discardStaged(cancelled.handle), true);
    assert.equal(client.discardStaged(cancelled.handle), false);
    assert.equal((await client.installStaged(cancelled.handle)).kind, 'error');
    await client.checkOnce();
    const stale = await client.downloadAvailable(); assert(stale.ok);
    await client.checkOnce();
    assert.equal((await client.installStaged(stale.handle)).kind, 'error');
    assert.equal(fixture.calls.install, 0);
    return { cancelled: true, staleRefused: true, installCalls: fixture.calls.install };
  });
  await run('eligibility-refusal-deferral-download-install-failure', async () => {
    const fixture = preparedUpdate();
    const refused = api.createAutoUpdateClient({ check: async () => ({ ...fixture.update, rawJson: { tier: 'update' } }) });
    const refusedStatus = await refused.checkOnce();
    assert.equal(refusedStatus.kind, 'available'); assert.equal(refusedStatus.eligible, false);
    assert.equal(api.deriveUpdateOverlay(refusedStatus).actions.download, false);
    assert.equal((await refused.downloadAvailable()).ok, false);
    assert.equal(fixture.calls.download, 0);
    const deferred = api.createAutoUpdateClient({ check: async () => fixture.update, canInstall: () => false, deferReason: () => 'fixture_host_busy' });
    await deferred.checkOnce();
    const deferredStatus = await deferred.installAvailable();
    assert.equal(deferredStatus.kind, 'deferred'); assert.equal(deferredStatus.reason, 'fixture_host_busy');
    assert.equal(fixture.calls.download, 0); assert.equal(fixture.calls.install, 0);
    const badDownload = preparedUpdate({ failDownload: true });
    const downloader = api.createAutoUpdateClient({ check: async () => badDownload.update });
    await downloader.checkOnce();
    const failedDownload = await downloader.downloadAvailable();
    assert.equal(failedDownload.ok, false); assert.equal(failedDownload.status.kind, 'error');
    assert.equal(badDownload.calls.install, 0);
    const badInstall = preparedUpdate({ failInstall: true });
    const installer = api.createAutoUpdateClient({ check: async () => badInstall.update });
    await installer.checkOnce();
    const failedInstall = await installer.installAvailable(); assert.equal(failedInstall.kind, 'error');
    const empty = api.createAutoUpdateClient({ check: async () => null });
    assert.equal((await empty.checkOnce()).kind, 'up_to_date');
    return { refused: refusedStatus, deferred: deferredStatus, failedDownload: failedDownload.status, failedInstall, noUpdate: true };
  });
  await run('consent-deny-download-never-approves-install', async () => {
    const fixture = preparedUpdate();
    const client = api.createAutoUpdateClient({ check: async () => fixture.update });
    const consent = api.createUpdateConsent(), runner = api.createUpdateConsentRunner(client);
    await client.checkOnce();
    consent.request('download'); consent.deny(); await consent.allowOnce(runner);
    assert.equal(fixture.calls.download, 0);
    consent.request('download'); await consent.allowOnce(runner);
    assert.equal(fixture.calls.download, 1); assert.equal(fixture.calls.install, 0);
    await consent.allowOnce(runner); assert.equal(fixture.calls.install, 0);
    consent.request('install'); await consent.allowOnce(runner);
    assert.equal(fixture.calls.install, 1); assert.equal(consent.state().error, null);
    return { calls: fixture.calls, consent: consent.state() };
  });
  await run('renderer-native-bridge-command-state-cancel-refusal-projection', async () => {
    // Deliberately an in-memory transport, not an implementation/qualification of native updater.
    const privateMaterial = { headers: { Authorization: 'private-fixture' }, url: 'private-fixture', signature: 'private-fixture', path: 'private-fixture', token: 'private-fixture' };
    const check = { available: true, eligible: true, kind: 'patch', version: '9.8.7', tier: 'free', refusal: 'none', handleId: 'fixture_1', checkedAt: 1, fresh: true };
    let state = 'available', progressCallback, disposed = 0;
    const commands = [], progress = [];
    const client = native.createNativeUpdateClient({
      invoke: async (command, args) => {
        commands.push({ command, args });
        if (command === 'rightkit_update_check') return { ...check, ...privateMaterial };
        if (command === 'rightkit_update_status') return { state, handleId: 'fixture_1', message: 'none', ...privateMaterial };
        assert.deepEqual(args, { handleId: 'fixture_1' });
        if (command === 'rightkit_update_download') { state = 'downloaded'; return { handleId: 'fixture_1', state: 'downloaded', message: 'none', ...privateMaterial }; }
        if (command === 'rightkit_update_cancel') { state = 'idle'; return { cancelled: true, ...privateMaterial }; }
        if (command === 'rightkit_update_install') return { state: 'refused', reason: 'stale', message: 'updateRefused', ...privateMaterial };
        throw new Error('unexpected fixture command');
      },
      listen: async (event, callback) => { assert.equal(event, 'rightkit://update-progress'); progressCallback = callback; return () => { disposed++; }; },
    });
    try {
      assert.deepEqual(await client.check(), check);
      assert.deepEqual(await client.status(), { state: 'available', handleId: 'fixture_1', message: 'none' });
      const stop = await client.onProgress(value => progress.push(value));
      progressCallback({ payload: { handleId: 'fixture_1', received: 5, total: 10, ...privateMaterial } });
      progressCallback({ payload: { handleId: 'fixture_1', received: 11, total: 10 } });
      assert.deepEqual(progress, [{ handleId: 'fixture_1', received: 5, total: 10 }]);
      assert.equal((await client.status()).state, 'available', 'events cannot synthesize status');
      assert.deepEqual(await client.download('fixture_1'), { handleId: 'fixture_1', state: 'downloaded', message: 'none' });
      assert.equal((await client.status()).state, 'downloaded');
      assert.deepEqual(await client.cancel('fixture_1'), { cancelled: true });
      assert.equal((await client.status()).state, 'idle');
      assert.deepEqual(await client.install('fixture_1'), { state: 'refused', reason: 'stale', message: 'updateRefused' });
      const before = commands.length;
      await assert.rejects(client.download('../not-a-handle'), error => error instanceof native.NativeUpdateClientError);
      assert.equal(commands.length, before);
      stop(); stop(); client.dispose();
      progressCallback({ payload: { handleId: 'fixture_1', received: 6 } });
      assert.equal(progress.length, 1); assert.equal(disposed, 1);
      await assert.rejects(client.status(), error => error.code === 'unsupported');
      return { commands, progress, unlistenCalls: disposed, projectionRemovedPrivateFields: true, refusal: 'stale' };
    } finally { client.dispose(); }
  });
  await run('malformed-native-dto-private-errors-fail-closed', async () => {
    let response = { available: true, eligible: true, refusal: 'none', checkedAt: 1, fresh: false };
    let rejection;
    const client = native.createNativeUpdateClient({ invoke: async () => { if (rejection) throw rejection; return response; }, listen: async () => () => {} });
    try {
      await assert.rejects(client.check(), error => error instanceof native.NativeUpdateClientError && error.messageCode === 'unknown');
      response = { state: 'failed', message: 'private-fixture' };
      assert.deepEqual(await client.status(), { state: 'failed', message: 'unknown' });
      rejection = { code: 'forbidden', message: 'forbidden', cause: 'private-fixture', headers: 'private-fixture' };
      await assert.rejects(client.check(), error => error.code === 'forbidden' && error.message === native.UPDATE_MESSAGES.forbidden && !('cause' in error) && !JSON.stringify(error).includes('private-fixture'));
      rejection = new Error('private-fixture');
      await assert.rejects(client.status(), error => error.message === native.UPDATE_MESSAGES.unknown && !('cause' in error));
      return { malformedRejected: true, unknownMessageProjected: true, errorTextFixed: true, privateCauseRemoved: true };
    } finally { client.dispose(); }
  });
  assert.equal(result.scenarios.length, 6);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error)); process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
