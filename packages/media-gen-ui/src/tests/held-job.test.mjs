import test from 'node:test';
import assert from 'node:assert/strict';
import {
  RECOVERED_CODE,
  heldActionLabel,
  heldActions,
  heldConsequence,
  heldExplanation,
  heldLabel,
  isRecoveredCode,
  trainingAmount,
  trainingHeldActionLabel,
  trainingHeldActions,
  trainingHeldConfirmLabel,
  trainingHeldConsequence,
  trainingHeldExplanation,
  trainingHeldSince,
  trainingHeldTitle
} from '../lib/held-job.ts';
import { lineLabel, lineTone } from '../views/voices-logic.ts';

test('held jobs offer resubmit or abandon; a recovered result offers adopt, never resubmit', () => {
  assert.deepEqual(heldActions(false), ['resubmit', 'abandon']);
  assert.deepEqual(heldActions(true), ['adopt', 'abandon']);
  assert.equal(isRecoveredCode(RECOVERED_CODE), true);
  assert.equal(isRecoveredCode('outcome_unknown'), false);
  assert.equal(isRecoveredCode(null), false);
});

test('held wording names the provider and never shows a raw status', () => {
  assert.equal(heldLabel(false), 'Outcome unknown');
  assert.match(heldExplanation('ElevenLabs', false), /may already be running at ElevenLabs/);
  assert.match(heldExplanation('', false), /the provider/);
  assert.match(heldExplanation('ElevenLabs', true), /could not be stored/);
  for (const a of ['resubmit', 'abandon', 'adopt']) {
    assert.ok(heldActionLabel(a).length > 0 && !heldActionLabel(a).includes('_'));
    assert.ok(heldConsequence(a).endsWith('Continue?'));
  }
});

test('a held voice batch line has its own label and tone, not "Speaking…"', () => {
  assert.equal(lineLabel('outcome_unknown'), 'Outcome unknown');
  assert.equal(lineTone('outcome_unknown'), 'warn');
  assert.equal(lineLabel('polling'), 'Speaking…');
});

test('a held training offers release or abandon, each with its own label and confirmation', () => {
  assert.deepEqual(trainingHeldActions(), ['release', 'abandon']);
  assert.equal(trainingHeldActionLabel('release'), 'Nothing started: release the reservation');
  assert.equal(trainingHeldActionLabel('abandon'), 'It ran or may have: keep it counted as spent');
  for (const a of trainingHeldActions()) {
    assert.ok(trainingHeldTitle(a).endsWith('?'));
    assert.ok(trainingHeldConfirmLabel(a).length > 0 && !trainingHeldConfirmLabel(a).includes('_'));
    assert.ok(trainingHeldConsequence(a, 2).endsWith('Continue?'));
  }
  assert.equal(heldLabel(false), 'Outcome unknown');
});

test('held training wording names the reserved amount and says it may have run', () => {
  assert.match(trainingHeldExplanation('fal', 2), /may have accepted this training/);
  assert.match(trainingHeldExplanation('fal', 2), /\(\$2\.00\) stays reserved/);
  assert.match(trainingHeldExplanation('fal', null), /\(unknown\) stays reserved/);
  assert.match(trainingHeldConsequence('abandon', 1.5), /\$1\.50 counted as spent/);
  assert.match(trainingHeldConsequence('release', 1.5), /\$1\.50 reservation/);
  assert.equal(trainingAmount(undefined), 'unknown');
});

test('held training time is shown only for a readable held_at', () => {
  assert.equal(trainingHeldSince(undefined), '');
  assert.equal(trainingHeldSince('not a date'), '');
  assert.match(trainingHeldSince('2026-10-10T14:03:00Z'), /^Held since \d/);
});
