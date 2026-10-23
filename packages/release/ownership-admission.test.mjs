import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { OwnershipAdmission, classifyOwnershipReport, defaultScannerRoot, ownershipScanRoot } from './ownership-admission.mjs';
import { CATALOG_DIGEST_DRIFT_WARNING } from './ownership-approval.mjs';

const digest = (n) => n.toString(16).padStart(64, '0');
const identity = 'https://github.com/test/repo';
const trust = { schema: 1, revokedKeyIds: [], verifiedTrackingEvidence: [], minimumScannerVersion: '2.0.0', minimumCatalogVersion: '1', repositories: [{ repositoryIdentity: identity, roots: ['.'], visibility: 'private' }] };
const inputs = () => ({ contentDigest: digest(1), catalogDigest: digest(2), trustDigest: digest(3), approvalsDigest: digest(4), scannerPolicyDigest: digest(5), protectedFloors: { minimumScannerVersion: '2.0.0', minimumCatalogVersion: '1', minimumCatalogDigest: null } });
const violation = { fingerprint: 'browser.npm.playwright', capability: 'browser-automation', package: 'rightkit-browser', file: 'package.json', line: 0 };
const report = (patch = {}) => ({ ok: true, enrollmentPending: false, violations: [], problems: [], coverage: [], metadata: { repositoryIdentity: identity, scannerVersion: '2.4.0', catalogVersion: '1', catalogDigest: digest(2), coverage: [], roots: [{ root: '.' }] }, ...patch });
function admission(mode, value, options = {}) {
  return new OwnershipAdmission({ mode, scannerRoot: defaultScannerRoot(), runInputs: async () => ({ inputs: inputs(), trust, complete: true }), runScan: async () => ({ report: value, trust, inputs: inputs() }), ...options });
}
const root = join(tmpdir(), 'ownership-verdict-app');
const cases = [
  ['ok false', report({ ok: false }), 'RK_OWNERSHIP_REPORT_FAILED'],
  ['pending enrollment is only a warning: ok:false still refuses as report-failed', report({ ok: false, enrollmentPending: true }), 'RK_OWNERSHIP_REPORT_FAILED'],
  ['pending enrollment never hides a violation', report({ ok: false, enrollmentPending: true, violations: [violation] }), 'RK_OWNERSHIP_VIOLATION'],
  ['incomplete coverage', report({ coverage: [{ path: 'source.bin', status: 'unscanned', reason: 'unsupported-input' }] }), 'RK_OWNERSHIP_INCOMPLETE_COVERAGE'],
  ['invalid registration', report({ metadata: { ...report().metadata, repositoryIdentity: 'https://github.com/test/unregistered' } }), 'RK_OWNERSHIP_INVALID_REGISTRATION'],
  ['scanner downgrade', report({ metadata: { ...report().metadata, scannerVersion: '1.0.0' } }), 'RK_OWNERSHIP_POLICY_DOWNGRADE'],
  ['catalog downgrade', report({ metadata: { ...report().metadata, catalogVersion: '0' } }), 'RK_OWNERSHIP_POLICY_DOWNGRADE'],
  ['violation', report({ ok: false, violations: [violation] }), 'RK_OWNERSHIP_VIOLATION'],
  ['problems', report({ ok: false, problems: ['exception expired'] }), 'RK_OWNERSHIP_UNVERIFIED'],
];
for (const [name, value, code] of cases) test('enforce refuses ' + name + '; report records identical typed verdict', async () => {
  const enforce = admission('enforce', value), advisory = admission('report', value);
  const result = await enforce.evaluate({ repositoryRoot: root });
  assert.equal(result.admit, false); assert.equal(result.code, code); assert.ok(result.problems.length);
  assert.throws(() => enforce.assertAdmitted(result), { code });
  const recorded = await advisory.evaluate({ repositoryRoot: root });
  assert.equal(recorded.code, code); assert.equal(recorded.admit, false);
  assert.doesNotThrow(() => advisory.assertAdmitted(recorded));
});

test('missing, extra, wrong-typed & non-object reports fail closed', async () => {
  const malformed = [null, undefined, [], 'clean', 1, { ...report(), extra: true }];
  for (const key of ['ok', 'enrollmentPending', 'violations', 'problems', 'coverage', 'metadata']) {
    const missing = report(); delete missing[key]; malformed.push(missing);
    malformed.push(report({ [key]: key === 'ok' || key === 'enrollmentPending' ? 'false' : 1 }));
  }
  malformed.push(report({ metadata: { ...report().metadata, extra: true } }), report({ coverage: [null] }), report({ violations: [null] }), report({ problems: [1] }),
    report({ violations: [{ ...violation, extra: true }] }), report({ rows: [{ capability: 1 }] }),
    report({ metadata: { ...report().metadata, contentDigests: [{ path: 'source.rs', digest: 1 }] } }));
  for (const value of malformed) {
    const gate = admission('enforce', value), result = await gate.evaluate({ repositoryRoot: root });
    assert.equal(result.code, 'RK_OWNERSHIP_MALFORMED_REPORT', JSON.stringify(value));
    assert.throws(() => gate.assertAdmitted(result), { code: 'RK_OWNERSHIP_MALFORMED_REPORT' });
  }
});

test('shared classification accepts complete protected report & preserves violation details', () => {
  assert.equal(classifyOwnershipReport(report(), trust).admit, true);
  const dirty = classifyOwnershipReport(report({ violations: [violation] }), trust);
  assert.equal(dirty.code, 'RK_OWNERSHIP_VIOLATION');
  assert.deepEqual(dirty.violations[0], { capability: 'browser-automation', owner: 'rightkit-browser', fingerprint: 'browser.npm.playwright', file: 'package.json', line: 0 });
  const pendingNoTrust = classifyOwnershipReport(report({ enrollmentPending: true }), null);
  assert.equal(pendingNoTrust.outcome, 'clean');
  assert.equal(pendingNoTrust.admit, true);
  assert.match(pendingNoTrust.warnings.join(' '), /enrollment is pending/);
  assert.equal(classifyOwnershipReport(report({ enrollmentPending: true }), trust).warnings.length, 1);
  assert.equal(classifyOwnershipReport(report(), trust).warnings, undefined);
  assert.equal(classifyOwnershipReport(report(), null).code, 'RK_OWNERSHIP_INVALID_REGISTRATION');
});

test('catalog paths never confer owner exemption & missing roots refuse', async () => {
  const host = await mkdtemp(join(tmpdir(), 'rk-false-owner-'));
  try {
    await mkdir(join(host, 'tools', 'rightkit', 'packages', 'release'), { recursive: true });
    await writeFile(join(host, 'tools', 'rightkit', 'packages', 'release', 'rightkit-ownership.json'), '{}');
    await writeFile(join(host, 'rightkit-ownership.json'), '{}');
    assert.deepEqual(ownershipScanRoot({ repositoryRoot: host, workspaceRoot: join(host, 'tools', 'rightkit') }), { root: host });
    assert.deepEqual(ownershipScanRoot({ repositoryRoot: host, workspaceRoot: join(host, 'tools', 'other') }), { root: host });
    const gate = admission('enforce', report({ violations: [violation] }));
    const result = await gate.evaluate({ repositoryRoot: host });
    assert.throws(() => gate.assertAdmitted(result), { code: 'RK_OWNERSHIP_VIOLATION' });
    const missing = await gate.evaluate({});
    assert.throws(() => gate.assertAdmitted(missing), { code: 'RK_OWNERSHIP_INVALID_REGISTRATION' });
  } finally { await rm(host, { recursive: true, force: true }); }
});

for (const key of Object.keys(inputs())) test('cache invalidates changed ' + key + ' without TTL reuse', async () => {
  let current = inputs(), calls = 0;
  const gate = admission('enforce', report(), { now: () => 0, runInputs: async () => ({ inputs: structuredClone(current), trust, complete: true }), runScan: async () => { calls++; return { inputs: structuredClone(current), trust, report: report(calls > 1 ? { ok: false, violations: [violation] } : {}) }; } });
  assert.equal((await gate.evaluate({ repositoryRoot: root })).admit, true);
  current[key] = key === 'protectedFloors' ? { ...current[key], minimumScannerVersion: '2.1.0' } : digest(99);
  const next = await gate.evaluate({ repositoryRoot: root });
  assert.equal(calls, 2); assert.equal(next.cached, false); assert.equal(next.admit, false);
});

test('identical digests share cache & in-flight scan regardless elapsed time', async () => {
  let clock = 0, calls = 0, probes = 0;
  const gate = admission('enforce', report(), { now: () => clock, runInputs: async () => { probes++; return { inputs: inputs(), trust, complete: true }; }, runScan: async () => { calls++; await new Promise((resolve) => setImmediate(resolve)); return { report: report(), trust, inputs: inputs() }; } });
  const [a, b] = await Promise.all([gate.evaluate({ repositoryRoot: root }), gate.evaluate({ repositoryRoot: root })]);
  assert.equal(calls, 1); assert.equal(a.admit, true); assert.equal(b.admit, true);
  clock = 120_000;
  assert.equal((await gate.evaluate({ repositoryRoot: root })).cached, true); assert.equal(calls, 1); assert.ok(probes >= 3);
});

test('incomplete inventory never inherits cached or in-flight clean verdict', async () => {
  let complete = true;
  const gate = admission('enforce', report(), { runInputs: async () => ({ inputs: inputs(), trust, complete }) });
  assert.equal((await gate.evaluate({ repositoryRoot: root })).admit, true);
  complete = false;
  const refused = await gate.evaluate({ repositoryRoot: root });
  assert.equal(refused.code, 'RK_OWNERSHIP_INCOMPLETE_COVERAGE'); assert.equal(refused.admit, false); assert.equal(refused.cached, false);
});

test('protected pinned catalog, root coverage & malformed registry fail closed', () => {
  // A digest that differs from the pin is drift: admitted with a warning (trust-sync is retired); version floors stay hard.
  const drift = classifyOwnershipReport(report(), { ...trust, minimumCatalogDigest: digest(99) });
  assert.equal(drift.admit, true); assert.deepEqual(drift.warnings, [CATALOG_DIGEST_DRIFT_WARNING]);
  assert.equal(classifyOwnershipReport(report(), { ...trust, minimumCatalogDigest: report().metadata.catalogDigest }).warnings, undefined);
  assert.equal(classifyOwnershipReport(report(), { ...trust, minimumCatalogVersion: '99', minimumCatalogDigest: digest(99) }).code, 'RK_OWNERSHIP_POLICY_DOWNGRADE');
  assert.equal(classifyOwnershipReport(report(), { ...trust, repositories: [{ ...trust.repositories[0], roots: ['engine', 'app'] }] }).code, 'RK_OWNERSHIP_INVALID_REGISTRATION');
  assert.equal(classifyOwnershipReport(report(), { ...trust, repositories: [null] }).code, 'RK_OWNERSHIP_INVALID_REGISTRATION');
  assert.equal(classifyOwnershipReport(report({ metadata: { ...report().metadata, coverage: [{ path: 'source.rs', status: 'incomplete', reason: 'missing' }] } }), trust).code, 'RK_OWNERSHIP_INCOMPLETE_COVERAGE');
});

test('missing digests, changed scan bindings & unavailable scanners refuse; failures retry', async () => {
  for (const key of Object.keys(inputs())) {
    const incomplete = inputs(); delete incomplete[key];
    const gate = admission('enforce', report(), { runInputs: async () => ({ inputs: incomplete, trust, complete: true }) });
    const result = await gate.evaluate({ repositoryRoot: root });
    assert.throws(() => gate.assertAdmitted(result), { code: 'RK_OWNERSHIP_INVALID_INPUTS' });
  }
  const gate = admission('enforce', report(), { runScan: async () => ({ report: report(), trust, inputs: { ...inputs(), contentDigest: digest(99) } }) });
  const changed = await gate.evaluate({ repositoryRoot: root });
  assert.throws(() => gate.assertAdmitted(changed), { code: 'RK_OWNERSHIP_INPUTS_CHANGED' });
  for (const complete of [undefined, null, 'true']) {
    const malformed = admission('enforce', report(), { runInputs: async () => ({ inputs: inputs(), trust, complete }) });
    assert.equal((await malformed.evaluate({ repositoryRoot: root })).code, 'RK_OWNERSHIP_INVALID_INPUTS');
  }
  let calls = 0;
  const broken = admission('enforce', report(), { runScan: async () => { calls++; return { error: 'scan timeout' }; } });
  for (let i = 0; i < 2; i++) {
    const result = await broken.evaluate({ repositoryRoot: root });
    assert.throws(() => broken.assertAdmitted(result), { code: 'RK_OWNERSHIP_UNVERIFIED' });
  }
  assert.equal(calls, 2);
  const missing = await new OwnershipAdmission({ mode: 'enforce', scannerRoot: null }).evaluate({ repositoryRoot: root });
  assert.equal(missing.outcome, 'unscanned');
  assert.equal((await new OwnershipAdmission({ mode: 'off', runScan: () => assert.fail('off scans') }).evaluate({ repositoryRoot: root })).outcome, 'off');
});

test('only block-tier or banned violations refuse; advise violations admit with an advisory verdict', async () => {
  const { ownershipVerdict } = await import('./ownership-verdict.mjs');
  const advise = { ...violation, tier: 'advise' };
  const advisory = ownershipVerdict(report({ violations: [advise] }), trust);
  assert.equal(advisory.admit, true);
  assert.equal(advisory.code, 'RK_OWNERSHIP_ADVISORY');
  assert.match(advisory.problems[0], /\[advise\]/);
  assert.equal(ownershipVerdict(report({ ok: false, violations: [{ ...violation, tier: 'block' }] }), trust).code, 'RK_OWNERSHIP_VIOLATION');
  assert.equal(ownershipVerdict(report({ ok: false, violations: [{ ...advise, policy: 'banned' }] }), trust).code, 'RK_OWNERSHIP_VIOLATION');
  assert.equal(ownershipVerdict(report({ ok: false, violations: [violation] }), trust).code, 'RK_OWNERSHIP_VIOLATION', 'missing tier fails closed as block');
  assert.equal(ownershipVerdict(report({ violations: [{ ...violation, tier: 'warn' }] }), trust).code, 'RK_OWNERSHIP_MALFORMED_REPORT');
});
