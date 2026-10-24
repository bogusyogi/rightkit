import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const recipeId = 'license-native-client-views-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const fixtureDir = path.dirname(fileURLToPath(import.meta.url));
const secret = 'fixture-private-licence-token-never-surfaces';

// Synthetic views only: nothing here is a real licence, key or device.
const view = (overrides = {}) => ({
  mode: 'configured', status: 'active', lifecycle: 'active', tier: 'pro', features: ['export', 'sync'], kind: 'term', stage: 'time_bound',
  validUntil: 1_900_000_000_000, confirmationDueAt: null, staleSince: null, licenseIdSuffix: 'AB12', busy: false, unresolved: false, revisionState: 'active', message: 'none',
  ...overrides,
});
const bridge = (handler) => {
  const calls = [];
  const listeners = new Map();
  return {
    calls, listeners,
    invoke: async (command, args) => { calls.push([command, args]); return handler(command, args); },
    listen: async (event, callback) => { listeners.set(event, callback); return () => { listeners.delete(event); }; },
  };
};

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/license');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/license', rootResolvedUrl: root, entries: { '@rightkit/license': root } });
  result.artifact = provenance.artifact;
  const license = await import(provenance.entries['@rightkit/license']);
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));
  const { createLicenseClient, LicenseClientError, LICENSE_MESSAGES, licenseMessage } = license;

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('export-surface-exact-and-testing-blocked-by-default', async () => {
    assert.deepEqual(Object.keys(license).sort(), ['LICENSE_MESSAGES', 'LicenseClientError', 'createLicenseClient', 'hasAllFeatures', 'hasAnyFeature', 'hasFeature', 'isLicensed', 'isPro', 'licenseMessage', 'lifecycleGrantsAccess', 'lifecycleNeedsOnlineRefresh', 'satisfiesRequirement', 'tier', 'tierAtLeast']);
    assert.deepEqual(Object.keys(manifest.exports), ['.', './testing']);
    assert.equal(manifest.exports['./testing'].default, null);
    assert.deepEqual(Object.keys(manifest.exports['./testing']), ['development', 'default']);
    let code = null;
    try { import.meta.resolve('@rightkit/license/testing'); } catch (error) { code = error.code; }
    assert.equal(code, 'ERR_PACKAGE_PATH_NOT_EXPORTED', 'the mock must not resolve without the development condition');
    assert.equal(Object.isFrozen(LICENSE_MESSAGES), true);
    return { rootExports: 14, testingResolvesByDefault: false };
  });

  await run('status-projects-safe-view-and-drops-unknown-fields', async () => {
    const b = bridge(async () => ({ ...view(), activationToken: secret, arbitrary: { nested: secret } }));
    const client = createLicenseClient(b);
    const projected = await client.status();
    assert.deepEqual(projected, view());
    assert.deepEqual(Object.keys(projected).sort(), Object.keys(view()).sort());
    assert(!JSON.stringify(projected).includes(secret));
    assert.deepEqual(b.calls, [['rightkit_license_status', undefined]]);
    for (const bad of [{ lifecycle: 'notNative' }, { lifecycle: null }, { validUntil: 1.5 }, { validUntil: Number.NaN }, { busy: 'no' }, { features: 'export' }, { licenseIdSuffix: 'TOOLONG' }, { tier: '' }]) {
      const broken = createLicenseClient(bridge(async () => ({ ...view(), ...bad })));
      await assert.rejects(broken.status(), error => error instanceof LicenseClientError && error.code === 'unknown' && error.message === 'Something went wrong.');
    }
    await assert.rejects(createLicenseClient(bridge(async () => null)).status(), LicenseClientError);
    return { fields: Object.keys(projected).length, malformedRefused: 9, secretLeaked: false };
  });

  await run('commands-use-frozen-names-and-validate-transient-inputs', async () => {
    const b = bridge(async command => {
      if (command === 'rightkit_license_refresh') return { view: view({ status: 'grace', lifecycle: 'grace' }), outcome: 'stale', extra: secret };
      if (command === 'rightkit_license_request_trial') return { accepted: true, message: 'trialRequested', extra: secret };
      return view();
    });
    const client = createLicenseClient(b);
    await client.status();
    await client.activate('fixture-key');
    const refreshed = await client.refresh();
    await client.deactivate();
    const trial = await client.requestTrial('person@example.invalid');
    assert.deepEqual(b.calls, [
      ['rightkit_license_status', undefined], ['rightkit_license_activate', { key: 'fixture-key' }], ['rightkit_license_refresh', undefined],
      ['rightkit_license_deactivate', undefined], ['rightkit_license_request_trial', { email: 'person@example.invalid' }],
    ]);
    assert.deepEqual(refreshed, { view: view({ status: 'grace', lifecycle: 'grace' }), outcome: 'stale' });
    assert.deepEqual(trial, { accepted: true, message: 'trialRequested' });
    const before = b.calls.length;
    for (const key of ['', 'a\nb', 'x'.repeat(4097)]) await assert.rejects(client.activate(key), LicenseClientError);
    await assert.rejects(client.requestTrial(''), LicenseClientError);
    assert.equal(b.calls.length, before, 'invalid transient inputs must be refused before any native call');
    const unknownOutcome = createLicenseClient(bridge(async () => ({ view: view(), outcome: 'surprise' })));
    await assert.rejects(unknownOutcome.refresh(), LicenseClientError);
    const unknownMessage = createLicenseClient(bridge(async () => ({ accepted: false, message: secret })));
    assert.deepEqual(await unknownMessage.requestTrial('person@example.invalid'), { accepted: false, message: 'unknown' });
    return { commands: 5, invalidInputsRefusedWithoutNativeCall: 4, outcomeValidated: true };
  });

  await run('native-errors-map-to-fixed-text-without-raw-detail', async () => {
    const reject = error => createLicenseClient(bridge(async () => { throw error; })).status();
    await assert.rejects(reject({ code: 'rejected', reason: 'deviceLimit', message: 'deviceLimit', detail: secret }), error => error instanceof LicenseClientError && error.code === 'rejected' && error.reason === 'deviceLimit' && error.message === LICENSE_MESSAGES.deviceLimit && error.name === 'LicenseClientError');
    await assert.rejects(reject({ code: 'network' }), error => error.code === 'network' && error.message === LICENSE_MESSAGES.networkUnavailable && error.reason === undefined);
    await assert.rejects(reject({ code: 'busy', message: 'busy' }), error => error.code === 'busy' && error.message === LICENSE_MESSAGES.busy);
    await assert.rejects(reject({ code: 'rejected' }), error => error.reason === undefined && error.message === LICENSE_MESSAGES.rejectedOther);
    for (const raw of [secret, new Error(secret), { code: secret, message: secret }, { code: 'network', message: secret, body: secret }, { code: 'rejected', reason: secret }]) {
      await assert.rejects(reject(raw), error => {
        assert(error instanceof LicenseClientError);
        assert(!String(error).includes(secret) && !JSON.stringify(error).includes(secret) && error.cause === undefined);
        return true;
      });
    }
    const constructed = new LicenseClientError('rejected', 'revoked', 'revoked');
    assert.deepEqual([constructed.code, constructed.reason, constructed.message], ['rejected', 'revoked', LICENSE_MESSAGES.revoked]);
    assert.equal(new LicenseClientError('busy', 'busy', 'revoked').reason, undefined, 'reason is kept only for rejected');
    assert.equal(String(new LicenseClientError()), 'LicenseClientError: Something went wrong.');
    return { mappedCodes: 4, redactedRawErrors: 5 };
  });

  await run('change-subscription-projects-events-and-stops-on-bad-payload', async () => {
    const b = bridge(async () => view());
    const client = createLicenseClient(b);
    const seen = [];
    const errors = [];
    const stop = await client.onChange(next => seen.push(next.lifecycle), error => errors.push(error));
    const deliver = b.listeners.get('rightkit://license-changed');
    assert.equal(typeof deliver, 'function');
    deliver({ payload: { view: { ...view({ lifecycle: 'lifetime', status: 'active' }), activationToken: secret } } });
    assert.deepEqual(seen, ['lifetime']);
    deliver({ payload: { view: { lifecycle: 'bogus' } } });
    assert.equal(errors.length, 1);
    assert(errors[0] instanceof LicenseClientError && errors[0].message === 'Something went wrong.');
    assert.equal(b.listeners.has('rightkit://license-changed'), false, 'a malformed event must release the native subscription');
    deliver({ payload: { view: view() } });
    assert.deepEqual(seen, ['lifetime'], 'no callback after the subscription stopped');
    stop(); stop();
    const failing = createLicenseClient({ invoke: async () => view(), listen: async () => { throw new Error(secret); } });
    await assert.rejects(failing.onChange(() => {}), error => error instanceof LicenseClientError && !String(error).includes(secret));
    const notFunction = createLicenseClient({ invoke: async () => view(), listen: async () => 'not-a-function' });
    await assert.rejects(notFunction.onChange(() => {}), LicenseClientError);
    return { eventsDelivered: 1, errorsReported: 1, idempotentStop: true };
  });

  await run('access-helpers-fail-closed', async () => {
    const pro = view();
    assert.equal(license.isLicensed(pro), true);
    assert.equal(license.isPro(pro), true);
    assert.equal(license.tier(pro), 'pro');
    assert.equal(license.tier(null), null);
    assert.equal(license.hasFeature(pro, 'export'), true);
    assert.equal(license.hasFeature(pro, 'missing'), false);
    assert.equal(license.hasAllFeatures(pro, ['export', 'sync']), true);
    assert.equal(license.hasAllFeatures(pro, ['export', 'missing']), false);
    assert.equal(license.hasAnyFeature(pro, ['missing', 'sync']), true);
    assert.equal(license.hasAnyFeature(pro, []), false);
    assert.equal(license.tierAtLeast(view({ tier: 'plus' }), 'plus'), true);
    assert.equal(license.tierAtLeast(view({ tier: 'plus' }), 'pro'), false);
    assert.equal(license.tierAtLeast(view({ tier: 'pro' }), 'free'), true);
    assert.equal(license.tierAtLeast(view({ tier: 'studio' }), 'studio'), true, 'custom tiers compare by equality');
    assert.equal(license.tierAtLeast(view({ tier: 'studio' }), 'pro'), false);
    assert.equal(license.satisfiesRequirement(pro, { tier: 'plus', feature: 'export', allFeatures: ['export', 'sync'], anyFeature: ['sync'] }), true);
    assert.equal(license.satisfiesRequirement(pro, { tier: 'plus', anyFeature: ['missing'] }), false);
    assert.equal(license.satisfiesRequirement(pro, {}), true);
    const closed = [null, view({ busy: true }), view({ unresolved: true }), view({ revisionState: 'tombstone' }), view({ revisionState: 'none' }), view({ mode: 'disabled' }), view({ status: 'expired', lifecycle: 'expired' }), view({ status: 'unverifiable', lifecycle: 'verification_required' }), view({ status: 'revoked', lifecycle: 'revoked' })];
    for (const candidate of closed) {
      assert.equal(license.isLicensed(candidate), false);
      assert.equal(license.isPro(candidate), false);
      assert.equal(license.hasFeature(candidate, 'export'), false);
      assert.equal(license.satisfiesRequirement(candidate, {}), false);
    }
    assert.equal(license.isLicensed(view({ status: 'grace', lifecycle: 'grace' })), true);
    const lifecycles = ['unlicensed', 'migration_required', 'trial', 'expired_trial', 'active', 'grace', 'expired', 'lifetime_provisional', 'verification_required', 'lifetime', 'clock_repair_required', 'revoked', 'invalid'];
    assert.deepEqual(lifecycles.filter(license.lifecycleGrantsAccess), ['trial', 'active', 'grace', 'lifetime_provisional', 'lifetime']);
    assert.deepEqual(lifecycles.filter(license.lifecycleNeedsOnlineRefresh), ['migration_required', 'verification_required', 'clock_repair_required']);
    return { closedStatesRefused: closed.length, lifecyclesGrantingAccess: 5, lifecyclesNeedingRefresh: 3 };
  });

  await run('message-catalogue-fixed-english', async () => {
    assert.deepEqual(Object.keys(LICENSE_MESSAGES), ['none', 'unconfigured', 'busy', 'unresolved', 'networkUnavailable', 'keyNotAccepted', 'revoked', 'expired', 'deviceLimit', 'rateLimited', 'rejectedOther', 'storeFailed', 'clockRollback', 'clockUnavailable', 'verifyFailed', 'invalidToken', 'issuedInTheFuture', 'invalidLifecycle', 'migrationRequired', 'clockRepairRequired', 'forbidden', 'unsupported', 'trialRequested', 'trialRefused', 'updateRefused', 'updateFailed', 'unknown']);
    assert.equal(licenseMessage('none'), '');
    assert.equal(licenseMessage('busy'), 'Another licence operation is in progress. Try again in a moment.');
    assert.equal(licenseMessage('keyNotAccepted'), 'That licence key was not accepted.');
    assert.equal(licenseMessage('toString'), LICENSE_MESSAGES.unknown, 'inherited names are not message codes');
    assert.equal(licenseMessage(secret), 'Something went wrong.');
    assert.equal(licenseMessage(undefined), 'Something went wrong.');
    assert.equal(licenseMessage(42), 'Something went wrong.');
    return { codes: 27 };
  });

  await run('testing-mock-resolves-only-under-development-condition', async () => {
    const script = `
      import assert from 'node:assert/strict';
      const testing = await import('@rightkit/license/testing');
      const { createLicenseMock, RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP } = testing;
      const keys = Object.keys(testing).sort();
      const mock = createLicenseMock();
      const lifecycles = [];
      const stop = await mock.client.onChange(next => lifecycles.push(next.lifecycle));
      const first = await mock.client.status();
      const activated = await mock.client.activate('fixture-key');
      const refreshed = await mock.client.refresh();
      const deactivated = await mock.client.deactivate();
      const accepted = await mock.client.requestTrial('person@example.invalid');
      const refused = await mock.client.requestTrial('no-at-sign');
      stop();
      await mock.client.activate('after-stop');
      let unsupported = null;
      try { await mock.bridge.invoke('rightkit_license_nope'); } catch (error) { unsupported = error.code; }
      let blankKey = null;
      try { await mock.bridge.invoke('rightkit_license_activate', { key: '' }); } catch (error) { blankKey = [error.code, error.reason]; }
      process.stdout.write(JSON.stringify({
        keys, sentinel: mock.sentinel === RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP, tagged: Object.getOwnPropertyDescriptor(mock.bridge.invoke, RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP)?.value === true,
        first: [first.lifecycle, first.revisionState], activated: [activated.lifecycle, activated.tier, activated.licenseIdSuffix], refreshed: refreshed.outcome,
        deactivated: [deactivated.lifecycle, deactivated.revisionState], accepted, refused, lifecycles, unsupported, blankKey,
      }));
    `;
    const child = spawnSync(process.execPath, ['--conditions=development', '--input-type=module', '-e', script], { cwd: fixtureDir, encoding: 'utf8', timeout: 30_000, env: { ...process.env, NODE_OPTIONS: '' } });
    assert.equal(child.status, 0, `development-condition child failed: ${child.stderr}`);
    const mock = JSON.parse(child.stdout.trim());
    assert.deepEqual(mock.keys, ['RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP', 'createLicenseMock']);
    assert.equal(mock.sentinel, true);
    assert.equal(mock.tagged, true);
    assert.deepEqual(mock.first, ['unlicensed', 'none']);
    assert.deepEqual(mock.activated, ['active', 'pro', 'MOCK']);
    assert.equal(mock.refreshed, 'refreshed');
    assert.deepEqual(mock.deactivated, ['unlicensed', 'tombstone']);
    assert.deepEqual(mock.accepted, { accepted: true, message: 'trialRequested' });
    assert.deepEqual(mock.refused, { accepted: false, message: 'trialRefused' });
    assert.deepEqual(mock.lifecycles, ['active', 'unlicensed'], 'change events stop after the unsubscribe');
    assert.equal(mock.unsupported, 'unsupported');
    assert.deepEqual(mock.blankKey, ['rejected', 'keyNotAccepted']);
    return { condition: 'development', mockCommands: 5, eventsBeforeStop: mock.lifecycles.length };
  });

  assert.equal(result.scenarios.length, 8);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
