import { validateInstalledArtifact, fixtureEntry } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const CANDIDATE = { version: '0.6.3', sha256: '9db84e3f619ab01cc932e5a9323ee77022287c82e752604a339fc3ddff8047a3' };
const result = { recipeId: 'onboarding-react-memory-mount', schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error), stack: error?.stack ?? null });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
let dom, cleanup;
try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(version, CANDIDATE.version);
  if (resolution === 'packed') assert.equal(sha256, CANDIDATE.sha256, 'wrong R2(a) candidate archive');
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/onboarding', rootResolvedUrl: import.meta.resolve('@rightkit/onboarding'), entries: { '@rightkit/onboarding': import.meta.resolve('@rightkit/onboarding') } });
  result.artifact = provenance.artifact;
  const { JSDOM } = await import(fixtureEntry(import.meta.url, import.meta.resolve('jsdom')));
  dom = new JSDOM('<!doctype html><html><body></body></html>', { url: 'https://consumer.invalid/', pretendToBeVisual: true });
  for (const key of ['window', 'document', 'navigator', 'HTMLElement', 'Element', 'Node', 'MutationObserver', 'getComputedStyle']) {
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value: key === 'getComputedStyle' ? dom.window.getComputedStyle.bind(dom.window) : dom.window[key] });
  }
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  const React = await import(fixtureEntry(import.meta.url, import.meta.resolve('react')));
  const testing = await import(fixtureEntry(import.meta.url, import.meta.resolve('@testing-library/react')));
  const { render, screen, waitFor } = testing;
  cleanup = testing.cleanup;
  const userEvent = (await import(fixtureEntry(import.meta.url, import.meta.resolve('@testing-library/user-event')))).default;
  const { Onboarding, onboardingStateKey } = await import(provenance.entries['@rightkit/onboarding']);
  const KEY = typeof onboardingStateKey === 'function' ? onboardingStateKey() : onboardingStateKey;
  assert.equal(KEY, 'onboarding.state');
  const theme = { bg: '#fff', surface: '#fff', border: '#ddd', text: '#000', muted: '#666', accent: '#70c', accentContrast: '#fff', fontDisplay: 'system-ui', fontBody: 'system-ui' };
  const legal = { appName: 'ConsumerRight', trustLine: 'Consumer fixture.', documents: [{ title: 'Terms', url: 'https://consumer.invalid/terms' }], agreementStatement: 'I agree.', theme };
  const documents = [{ id: 'terms', role: 'eula', version: '1', sha256: 'a'.repeat(64), treatment: 'agree', material: true }];
  const record = hash => ({ version: 'v1', documentsHash: hash, acceptedAt: '2026-10-20T08:30:00.000Z', eligibilityBasis: 'individual' });
  function memory(initial = {}) {
    const values = structuredClone(initial);
    let revision = 1, failNext = false;
    return { values, failNext: () => { failNext = true; }, backend: {
      load: async () => ({ values: structuredClone(values), revision }),
      patch: async (patch, expectedRevision) => {
        if (failNext) { failNext = false; throw new Error('fixture settings save failed'); }
        if (expectedRevision !== undefined) assert.equal(expectedRevision, revision);
        Object.assign(values, structuredClone(patch));
        return { values: structuredClone(values), revision: ++revision };
      },
    } };
  }
  function receipt(initial = null, failFirst = false) {
    let stored = initial;
    const requests = [];
    return { requests, get stored() { return stored; }, port: {
      read: async () => stored,
      create: async request => {
        requests.push(structuredClone(request));
        assert(!('acceptedAt' in request), 'renderer must not supply receipt time');
        assert.equal(request.surface, 'first_run');
        assert.deepEqual(request.documents, documents);
        if (failFirst && requests.length === 1) throw new Error('fixture assent save failed');
        stored = { ...record(request.documentsHash), version: request.version, eligibilityBasis: request.eligibilityBasis };
        return stored;
      },
      write: async () => { throw new Error('create must take precedence over legacy write'); },
    } };
  }
  const step = (id, extra = {}) => ({ id, label: id.toUpperCase(), title: `Step ${id}`, render: () => React.createElement('p', null, `${id} body`), ...extra });
  function mount(layout, settings, extra = {}) {
    const completed = [], skipped = [];
    render(React.createElement(Onboarding, { layout, settings: settings.backend, legal, requiredVersion: 'v1', requiredDocumentsHash: 'sha256:new', receiptDocuments: documents, steps: [step('a'), step('b', { skippable: true }), step('c')], onComplete: value => completed.push(value), onSkip: () => skipped.push(true), ...extra }));
    return { completed, skipped };
  }
  async function agree(user) {
    await user.click((await screen.findAllByRole('radio'))[0]);
    await user.click(screen.getByRole('checkbox'));
    await user.click(screen.getByRole('button', { name: /agree/i }));
  }
  async function run(name, layout, operation) {
    const entry = { name, layout, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(userEvent.setup()); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
    finally { cleanup(); }
  }
  for (const layout of ['window', 'contained']) {
    await run('first-run-failed-assent-settings-retry-navigation-finish', layout, async user => {
      const settings = memory(), native = receipt(null, true);
      const callbacks = mount(layout, settings, { legalReceipt: native.port });
      const gate = await screen.findByTestId('onboarding-legal');
      assert.equal(gate.dataset.variant, 'first-run');
      assert.equal(gate.classList.contains('rk-onboarding--contained'), layout === 'contained');
      await agree(user);
      await screen.findByRole('alert');
      assert.equal(screen.queryByText('a body'), null);
      assert.equal(settings.values[KEY], undefined);
      assert.equal(native.stored, null);
      assert.equal(callbacks.completed.length, 0);
      // Next retry saves receipt but settings rejects; gate must remain closed again.
      settings.failNext();
      await user.click(screen.getByRole('button', { name: /agree/i }));
      await screen.findByRole('alert');
      assert.equal(screen.queryByText('a body'), null);
      assert.equal(settings.values[KEY], undefined);
      await user.click(screen.getByRole('button', { name: /agree/i }));
      await screen.findByText('a body');
      assert.equal(native.requests.length, 2, 'settings-only retry must reuse created receipt');
      assert.deepEqual(settings.values[KEY].legalAcceptance, record('sha256:new'));
      assert.equal(screen.getByTestId('onboarding').classList.contains('rk-onboarding--contained'), layout === 'contained');
      await user.click(screen.getByRole('button', { name: 'Continue' }));
      await screen.findByText('b body');
      await user.click(screen.getByRole('button', { name: 'Back' }));
      await screen.findByText('a body');
      assert.equal(settings.values[KEY].currentStep, 1);
      await user.click(screen.getByRole('button', { name: 'Continue' }));
      await screen.findByText('b body');
      await user.click(screen.getByRole('button', { name: 'Skip', exact: true }));
      await screen.findByText('c body');
      await user.click(screen.getByRole('button', { name: 'Finish setup' }));
      await waitFor(() => assert.equal(callbacks.completed.length, 1));
      assert.deepEqual(callbacks.completed[0], { completed: ['legal', 'a', 'c'], skipped: ['b'], steps: {} });
      assert.equal(callbacks.skipped.length, 0);
      return { receiptCreateCalls: native.requests.length, declaration: native.requests.at(-1), state: settings.values[KEY], completion: callbacks.completed[0] };
    });
    await run('bare-legacy-flag-refused', layout, async () => {
      const settings = memory({ [KEY]: { version: 1, currentStep: 1, completed: ['legal'], skipped: false, legalAccepted: true } });
      const callbacks = mount(layout, settings);
      assert.equal((await screen.findByTestId('onboarding-legal')).dataset.variant, 're-acceptance');
      assert.equal(screen.queryByText('a body'), null);
      assert.equal(callbacks.completed.length, 0);
      return { gate: 're-acceptance', advanced: false };
    });
    await run('digest-change-reacceptance', layout, async user => {
      const settings = memory({ [KEY]: { version: 1, currentStep: 1, completed: ['legal'], skipped: false, legalAccepted: true, legalAcceptance: record('sha256:old') } });
      const native = receipt(record('sha256:old'));
      mount(layout, settings, { legalReceipt: native.port });
      assert.equal((await screen.findByTestId('onboarding-legal')).dataset.variant, 're-acceptance');
      assert.equal(screen.queryByText('a body'), null);
      await agree(user);
      await screen.findByText('a body');
      assert.equal(native.requests.length, 1);
      assert.deepEqual(settings.values[KEY].legalAcceptance, record('sha256:new'));
      return { oldDigest: 'sha256:old', accepted: native.stored, declaration: native.requests[0] };
    });
    await run('set-up-later-persists-before-callback', layout, async user => {
      const settings = memory({ [KEY]: { version: 1, currentStep: 1, completed: ['legal'], skipped: false, legalAccepted: true, legalAcceptance: record('sha256:new') } });
      let skips = 0, completes = 0;
      mount(layout, settings, { onSkip: () => { assert.equal(settings.values[KEY].skipped, true); skips++; }, onComplete: () => { completes++; } });
      await user.click(await screen.findByRole('button', { name: 'Set up later' }));
      await waitFor(() => assert.equal(skips, 1));
      assert.equal(completes, 0);
      return { skips, completes, state: settings.values[KEY] };
    });
  }
  assert.equal(result.scenarios.length, 8);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
} finally {
  cleanup?.();
  dom?.window.close();
}
process.stdout.write(`${JSON.stringify(result)}\n`);
