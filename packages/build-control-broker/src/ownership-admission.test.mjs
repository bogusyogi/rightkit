import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { OwnershipAdmission, classifyOwnershipReport, defaultScannerRoot, ownershipScanRoot } from './ownership-admission.mjs';

const violation = { fingerprint: 'browser.npm.playwright', capability: 'browser-automation', package: 'rightkit-browser', file: 'package.json', line: 0 };
const fake = (report, calls = []) => async ({ scanRoot }) => { calls.push(scanRoot); return typeof report === 'function' ? report() : report; };

test('classification separates real violations from enrollment-pending backlog', () => {
  assert.equal(classifyOwnershipReport({ violations: [], problems: [] }).outcome, 'clean');
  assert.equal(classifyOwnershipReport({ violations: [violation], enrollmentPending: true }).outcome, 'pending-enrollment');
  assert.equal(classifyOwnershipReport({ violations: [violation], enrollmentPending: false }).outcome, 'violations');
  assert.equal(classifyOwnershipReport({ violations: [], problems: ['exception #0: expired'] }).outcome, 'problems');
  assert.deepEqual(classifyOwnershipReport({ violations: [violation] }).violations[0], { capability: 'browser-automation', owner: 'rightkit-browser', fingerprint: 'browser.npm.playwright', file: 'package.json', line: 0 });
});

test('report mode records but never refuses; enforce refuses violations and unverifiable scans', async () => {
  const dirty = { report: { violations: [violation] } };
  const report = new OwnershipAdmission({ mode: 'report', scannerRoot: 'scanner', runScan: fake(dirty) });
  report.assertAdmitted(await report.evaluate({ repositoryRoot: join(tmpdir(), 'app-a') }));
  const enforce = new OwnershipAdmission({ mode: 'enforce', scannerRoot: 'scanner', runScan: fake(dirty) });
  assert.throws(() => enforce.assertAdmitted({ outcome: 'violations', violations: [{ ...violation, owner: 'rightkit-browser' }] }), { code: 'RK_OWNERSHIP_VIOLATION', message: /browser-automation .* owned by rightkit-browser/ });
  await assert.rejects(async () => enforce.assertAdmitted(await enforce.evaluate({ repositoryRoot: join(tmpdir(), 'app-a') })), { code: 'RK_OWNERSHIP_VIOLATION' });
  const broken = new OwnershipAdmission({ mode: 'enforce', scannerRoot: 'scanner', runScan: fake({ error: 'scan exceeded 30000 ms' }) });
  await assert.rejects(async () => broken.assertAdmitted(await broken.evaluate({ repositoryRoot: join(tmpdir(), 'app-b') })), { code: 'RK_OWNERSHIP_UNVERIFIED' });
  const pending = new OwnershipAdmission({ mode: 'enforce', scannerRoot: 'scanner', runScan: fake({ report: { violations: [violation], enrollmentPending: true } }) });
  pending.assertAdmitted(await pending.evaluate({ repositoryRoot: join(tmpdir(), 'app-c') }));
  const missing = new OwnershipAdmission({ mode: 'enforce', scannerRoot: null });
  assert.equal((await missing.evaluate({ repositoryRoot: join(tmpdir(), 'app-d') })).outcome, 'unscanned');
});

test('scans are cached per root, deduplicated in flight, and failures are retried', async () => {
  let clock = 0; const calls = [];
  const admission = new OwnershipAdmission({ scannerRoot: 'scanner', ttlMs: 1_000, now: () => clock, runScan: fake({ report: { violations: [] } }, calls) });
  const root = join(tmpdir(), 'app-cache');
  const [a, b] = await Promise.all([admission.evaluate({ repositoryRoot: root }), admission.evaluate({ repositoryRoot: root })]);
  assert.equal(calls.length, 1); assert.equal(a, b);
  assert.equal((await admission.evaluate({ repositoryRoot: root })).cached, true); assert.equal(calls.length, 1);
  clock = 2_000; await admission.evaluate({ repositoryRoot: root }); assert.equal(calls.length, 2);
  const failing = []; const flaky = new OwnershipAdmission({ scannerRoot: 'scanner', runScan: fake({ error: 'boom' }, failing) });
  await flaky.evaluate({ repositoryRoot: root }); await flaky.evaluate({ repositoryRoot: root });
  assert.equal(failing.length, 2);
  assert.equal((await new OwnershipAdmission({ mode: 'off', runScan: () => assert.fail('off mode never scans') }).evaluate({ repositoryRoot: root })).outcome, 'off');
});

test('RightKit host monorepo: RightKit crates are exempt, other workspaces scan alone', async () => {
  const host = await mkdtemp(join(tmpdir(), 'rk-host-')); try {
    await mkdir(join(host, 'tools', 'rightkit', 'packages', 'release'), { recursive: true });
    await writeFile(join(host, 'tools', 'rightkit', 'packages', 'release', 'rightkit-ownership.json'), '{}');
    assert.deepEqual(ownershipScanRoot({ repositoryRoot: host, workspaceRoot: join(host, 'tools', 'rightkit') }), { exempt: 'rightkit-source' });
    assert.deepEqual(ownershipScanRoot({ repositoryRoot: host, workspaceRoot: join(host, 'tools', 'other') }), { root: join(host, 'tools', 'other') });
    assert.deepEqual(ownershipScanRoot({ repositoryRoot: join(host, 'app'), workspaceRoot: join(host, 'app', 'src-tauri') }), { root: join(host, 'app') });
  } finally { await rm(host, { recursive: true, force: true }); }
});

test('the real scanner flags an app that depends on Playwright directly', { timeout: 60_000 }, async () => {
  const scannerRoot = defaultScannerRoot(); assert.ok(scannerRoot, 'scanner resolvable from the source checkout');
  const app = await mkdtemp(join(tmpdir(), 'rk-own-app-')); try {
    await writeFile(join(app, 'package.json'), JSON.stringify({ name: 'dup-app', version: '0.0.0', dependencies: { playwright: '1.50.0' } }));
    const git = (...args) => execFileSync('git', ['-C', app, ...args], { stdio: 'ignore' });
    git('init'); git('config', 'user.email', 'rk@example.invalid'); git('config', 'user.name', 'RK'); git('add', '.'); git('commit', '-m', 'app');
    const result = await new OwnershipAdmission({ mode: 'enforce', scannerRoot }).evaluate({ repositoryRoot: app });
    assert.ok(['violations', 'pending-enrollment'].includes(result.outcome), `${result.outcome}: ${result.error ?? ''}`);
    assert.ok(result.violations.some((v) => v.fingerprint === 'browser.npm.playwright' && v.owner === 'rightkit-browser'));
  } finally { await rm(app, { recursive: true, force: true }); }
});
