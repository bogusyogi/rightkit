import test from 'node:test';
import assert from 'node:assert/strict';
import { SETUP_STEPS, SETUP_STEP_IDS, setupStepDef } from '../lib/setup-steps.ts';

const FACT_KEYS = ['purpose', 'cost', 'skipCost', 'resume'];

test('setup steps keep their order and labels (hidden-browser UI expectations)', () => {
  assert.deepEqual(SETUP_STEPS.map((s) => s.id), ['library', 'computer', 'installed', 'keys', 'models']);
  assert.deepEqual(SETUP_STEPS.map((s) => s.label), ['Library', 'This computer', 'Already installed', 'API keys', 'Local models']);
  assert.deepEqual(SETUP_STEP_IDS, SETUP_STEPS.map((s) => s.id));
});

test('every setup step carries all four non-empty facts: purpose, cost, skip cost, resume', () => {
  for (const step of SETUP_STEPS) {
    assert.ok(step.facts && typeof step.facts === 'object', `${step.id} has no facts`);
    for (const key of FACT_KEYS) {
      const value = step.facts[key];
      assert.equal(typeof value, 'string', `${step.id}.facts.${key} must be a string`);
      assert.ok(value.trim().length > 0, `${step.id}.facts.${key} must not be empty`);
    }
  }
});

test('every step says where to resume setup later', () => {
  for (const step of SETUP_STEPS) {
    assert.match(step.facts.resume, /Settings/, `${step.id} resume must name Settings`);
  }
});

test('setupStepDef returns the definition for a known id and throws for an unknown one', () => {
  assert.equal(setupStepDef('keys').label, 'API keys');
  assert.throws(() => setupStepDef('ffmpeg'), /No setup step definition/);
});
