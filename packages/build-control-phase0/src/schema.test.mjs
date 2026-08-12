import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import Ajv2020 from 'ajv/dist/2020.js';

const sourceDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(sourceDirectory, '../../../../../');
const readJson = (path) => JSON.parse(readFileSync(path, 'utf8'));
const validate = (schemaPath, documentPath) => {
  const validator = new Ajv2020({ allErrors: true, strict: false }).compile(readJson(schemaPath));
  const valid = validator(readJson(documentPath));
  assert.equal(valid, true, `${documentPath}: ${JSON.stringify(validator.errors)}`);
};

test('canonical Phase-0 report validates against its JSON Schema', () => {
  validate(resolve(sourceDirectory, '../schemas/phase0-report.schema.json'), resolve(repositoryRoot, 'tools/rightkit/docs/plans/evidence/rightkit-build-control-plane-v5/shared/phase0-report.json'));
});

test('sealed implementation contract validates against its JSON Schema', () => {
  const contractPath = resolve(repositoryRoot, 'tools/rightkit/docs/plans/evidence/rightkit-build-control-plane-v5/shared/implementation-contract.json');
  const contract = readJson(contractPath);
  const schemaPath = resolve(dirname(contractPath), contract.$schema);
  assert.equal(schemaPath, resolve(sourceDirectory, '../schemas/implementation-contract.schema.json'));
  validate(schemaPath, contractPath);
});

test('canonical managed-agent verification validates against its JSON Schema', () => {
  validate(resolve(sourceDirectory, '../schemas/managed-verification-report.schema.json'), resolve(repositoryRoot, 'tools/rightkit/docs/plans/evidence/rightkit-build-control-managed-v1/verification.json'));
});

test('macOS revision-2 managed policy evidence validates against its schema', () => {
  validate(resolve(sourceDirectory, '../schemas/managed-macos-policy-revision-2.schema.json'), resolve(repositoryRoot, 'tools/rightkit/docs/plans/evidence/rightkit-build-control-managed-v1/macos/managed-policy-revision-2.json'));
});

test('Windows revision-2 managed policy evidence validates against its schema', () => {
  validate(resolve(sourceDirectory, '../schemas/managed-windows-policy-rollout.schema.json'), resolve(repositoryRoot, 'tools/rightkit/docs/plans/evidence/rightkit-build-control-plane-v5/windows/managed-agent-policy-rollout-windows.json'));
});

test('revision-3 runtime and cleanup evidence is typed & complete', () => {
  const runtime = readJson(resolve(repositoryRoot, 'tools/rightkit/docs/plans/evidence/rightkit-build-control-managed-v1/macos/runtime-durability-2026-08-13.json'));
  assert.equal(runtime.schema, 'rightkit.build-control.macos.runtime-durability.v1');
  assert.equal(runtime.status, 'PASS');
  assert.ok(runtime.gc.reclaimedBytes > 0);
  assert.equal(runtime.restartRecovery.reservedBytesAfterRestart, 0);
  assert.match(runtime.managedCargo.externalTargetRoot, /^\/Volumes\/D\/\.rightkit-managed\//);
  assert.equal(runtime.managedCargo.workspaceLocalTargetAbsent, true);
  const cleanup = readJson(resolve(repositoryRoot, 'tools/rightkit/docs/plans/evidence/rightkit-build-control-managed-v1/macos/tmp-cleanup-2026-08-13.json'));
  assert.equal(cleanup.schema, 'rightkit-build-control-cleanup-evidence-v2');
  assert.ok(cleanup.bytesReclaimed > 0);
  assert.equal(cleanup.verification.result, 'clean');
  assert.deepEqual(cleanup.verification.remainingNamedDisposableCandidates, []);
  const policy = readJson(resolve(repositoryRoot, 'tools/rightkit/docs/plans/evidence/rightkit-build-control-plane-v5/windows/managed-agent-policy-validation-2026-08-13.json'));
  assert.equal(policy.schema, 'rightkit.build-control.windows.policy-validation.v1');
  assert.equal(policy.source.result, 'PASS');
  assert.equal(policy.source.tests, 28);
  for (const value of Object.values(policy.adapters.macosLocal)) assert.match(value, /^PASS:/);
  assert.match(policy.windows.codex, /^PASS:/);
  assert.match(policy.windows.claude, /^PASS:/);
  assert.equal(policy.windows.broker.state, 'Running');
  assert.equal(policy.windows.rightkit.result, 'PASS');
  assert.equal(policy.windows.childCargo.localTargetCreated, false);
});

test('managed-agent PASS cannot contain pending checks or acceptance', () => {
  const schema = readJson(resolve(sourceDirectory, '../schemas/managed-verification-report.schema.json'));
  const report = readJson(resolve(repositoryRoot, 'tools/rightkit/docs/plans/evidence/rightkit-build-control-managed-v1/verification.json'));
  const validator = new Ajv2020({ allErrors: true, strict: false }).compile(schema);
  const invalid = structuredClone(report);
  invalid.status = 'PASS';
  invalid.checks[0].status = 'PENDING';
  assert.equal(validator(invalid), false);
  const invalidAcceptance = structuredClone(report);
  invalidAcceptance.status = 'PASS';
  invalidAcceptance.acceptance.durableQuotaAndReceipts = 'PENDING';
  assert.equal(validator(invalidAcceptance), false);
  const invalidRevisionTwoGate = structuredClone(report);
  invalidRevisionTwoGate.status = 'PASS';
  invalidRevisionTwoGate.acceptance.macosManagedMatrix = 'PENDING';
  assert.equal(validator(invalidRevisionTwoGate), false);
  const invalidRevisionThreeGate = structuredClone(report);
  invalidRevisionThreeGate.status = 'PASS';
  invalidRevisionThreeGate.acceptance.releasedNamespaceGc = 'PENDING';
  assert.equal(validator(invalidRevisionThreeGate), false);
  const invalidRevisionFourGate = structuredClone(report);
  invalidRevisionFourGate.status = 'PASS';
  invalidRevisionFourGate.acceptance.remotePublication = 'PENDING';
  assert.equal(validator(invalidRevisionFourGate), false);
});

test('sealed managed-agent implementation contract validates against its JSON Schema', () => {
  const contractPath = resolve(repositoryRoot, 'tools/rightkit/docs/plans/evidence/rightkit-build-control-managed-v1/implementation-contract.json');
  const contract = readJson(contractPath);
  const schemaPath = resolve(dirname(contractPath), contract.$schema);
  assert.equal(schemaPath, resolve(sourceDirectory, '../schemas/implementation-contract.schema.json'));
  validate(schemaPath, contractPath);
});

test('contract schema rejects malformed producer, ID, & platform status', () => {
  const schema = readJson(resolve(sourceDirectory, '../schemas/implementation-contract.schema.json'));
  const contract = readJson(resolve(repositoryRoot, 'tools/rightkit/docs/plans/evidence/rightkit-build-control-plane-v5/shared/implementation-contract.json'));
  const validator = new Ajv2020({ allErrors: true, strict: false }).compile(schema);
  const invalid = structuredClone(contract);
  invalid.contractId = 'EC-0';
  invalid.evidenceProducers[0] = { id: 'Bad_Producer', path: '', produces: '' };
  invalid.closedPhase1Slice.currentPlatformDisposition.shared = 'PENDING';
  assert.equal(validator(invalid), false);
});
