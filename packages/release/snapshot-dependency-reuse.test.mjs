import test from 'node:test';
import os from "node:os";
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { dependencyDigest, canonicalConfigFingerprint, preparationInputComponents } from './snapshot-dependency-reuse.mjs';

test('dependency proof binds bytes, metadata and modes', (t) => {
  const scratch = ((root) => (fs.existsSync(root) ? root : os.tmpdir()))(process.platform === 'win32' ? 'D:\\scratch' : '/Volumes/D/scratch');
  const root = fs.mkdtempSync(path.join(scratch, 'rk-deps-proof-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const modules = path.join(root, 'node_modules'); fs.mkdirSync(modules);
  const pkg = path.join(modules, 'pkg.js'); fs.writeFileSync(pkg, 'one');
  const before = dependencyDigest(root, ['node_modules']);
  fs.writeFileSync(pkg, 'two'); assert.notEqual(dependencyDigest(root, ['node_modules']), before);
  fs.writeFileSync(pkg, 'one'); fs.writeFileSync(path.join(modules, '.pnpm-workspace-state-v1.json'), '{}');
  assert.notEqual(dependencyDigest(root, ['node_modules']), before);
  fs.rmSync(path.join(modules, '.pnpm-workspace-state-v1.json'));
  if (process.platform !== 'win32') {
    fs.chmodSync(pkg, 0o755); assert.notEqual(dependencyDigest(root, ['node_modules']), before);
    fs.chmodSync(pkg, 0o644);
  }
  fs.rmSync(pkg); assert.notEqual(dependencyDigest(root, ['node_modules']), before);
});

test('preparation inputs ignore only per-run ownership and approved scheduling controls', () => {
  const input = { source: 'tree', toolchain: 'tool', config: 'config', store: 'store', env: { RIGHTKIT_BUILD_OWNERSHIP: 'first', RIGHTKIT_BUILD_OWNERSHIP_FILE: '/owned', NODE_OPTIONS: 'same', RK_GATE_JOBS: '4' } };
  const fingerprint = (env) => preparationInputComponents({ ...input, env });
  const before = fingerprint(input.env);
  assert.deepEqual(fingerprint({ ...input.env, RUSTRIGHT_GATE_RUN: 'dell-probe-first-cancel' }), before);
  assert.deepEqual(fingerprint({ ...input.env, RUSTRIGHT_GATE_RUN: 'dell-probe-second-cancel' }), before);
  assert.deepEqual(fingerprint({ ...input.env, RIGHTKIT_BUILD_OWNERSHIP: 'second', RK_GATE_JOBS: '8' }), before);
  assert.notEqual(fingerprint({ ...input.env, NODE_OPTIONS: 'changed' }).environment, before.environment);
  assert.notEqual(fingerprint({ ...input.env, RIGHTKIT_BUILD_OWNERSHIP_FILE: '/other' }).environment, before.environment);
  assert.notEqual(preparationInputComponents({ ...input, config: 'changed' }).config, before.config);
});

test('NN12: what changes between two gate launches of the same check does not change the preparation key (Dell 2026-11-18)', () => {
  // The contract test's two warm launches (S1:check, S2:check) differ in exactly these gate variables (gate-acceptance.mjs doRun, contractRunEnv).
  const input = { source: 'tree', toolchain: 'tool', config: 'config', store: 'store' };
  const base = { Path: 'C:\\bin', USERPROFILE: 'C:\\Users\\u', NPM_CONFIG_REGISTRY: 'https://r', NODE_OPTIONS: '--x', RIGHTKIT_BUILD_OWNERSHIP_FILE: '/owned' };
  const first = preparationInputComponents({ ...input, env: { ...base, RK_GATE_SPAWN_LEDGER: 'D:\\out\\S1_check.ledger.jsonl', RK_GATE_CONTRACT_STEP: 'S1:check', RK_GATE_CONTRACT_RUN_ID: 'a'.repeat(64), RK_GATE_CONTRACT_JOBS: 'D:\\out\\jobs-1', RUSTRIGHT_ATTEST_NONCE: 'n1' } });
  const second = preparationInputComponents({ ...input, env: { ...base, RK_GATE_SPAWN_LEDGER: 'D:\\out\\S2_check.ledger.jsonl', RK_GATE_CONTRACT_STEP: 'S2:check', RK_GATE_CONTRACT_RUN_ID: 'b'.repeat(64), RK_GATE_CONTRACT_JOBS: 'D:\\out\\jobs-2', RUSTRIGHT_ATTEST_NONCE: 'n2' } });
  assert.deepEqual(second, first);
  // Anything else keys it, including variables nobody has shown an install ignores (astra62 review 2): a new session's TMPDIR prepares again.
  assert.notEqual(preparationInputComponents({ ...input, env: { ...base, TMPDIR: 'C:\\t2' } }).environment, first.environment);
  // What pnpm reads still keys it, whatever its case.
  assert.notEqual(preparationInputComponents({ ...input, env: { ...base, npm_config_registry: 'https://other' } }).environment, first.environment);
  assert.notEqual(preparationInputComponents({ ...input, env: { ...base, PNPM_CONFIG_NODE_LINKER: 'hoisted' } }).environment, first.environment);
});

test('config identity ignores object order but binds values, array order and file hashes', () => {
  const files = [['user', 'hash-a'], ['global', 'hash-b']];
  const fingerprint = (config, ids = files) => canonicalConfigFingerprint(config, ids);
  const before = fingerprint({ a: [1, 2], b: { x: 1, y: 2 } });
  assert.equal(fingerprint({ b: { y: 2, x: 1 }, a: [1, 2] }), before);
  assert.notEqual(fingerprint({ b: { y: 2, x: 1 }, a: [2, 1] }), before);
  assert.notEqual(fingerprint({ b: { y: 3, x: 1 }, a: [1, 2] }), before);
  assert.notEqual(fingerprint({ a: [1, 2], b: { x: 1, y: 2 } }, [['user', 'changed'], ['global', 'hash-b']]), before);
});
