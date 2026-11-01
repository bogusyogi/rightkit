import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

// The HeyGen bump (video model form contracts 1.0.0 -> 1.1.0) must leave every existing model's form alone.
// Both contract versions are frozen as fixtures in this repository, copied from the workspace tool that owns the
// live contract (tools/recipes/video/model-form-contracts.json; commits dc863dea4e and 49a9799caf of the workspace
// repository). They sit in fixtures/model-form-contracts/, outside the no-provider-imports scan roots, because the
// contract's _meta.source names provider scripts. The test reads no path outside the repository and runs no git
// command, so it gives the same answer in an isolated checkout, in CI and in the workspace. A later bump adds its
// own fixture and its own comparison.
const readContract = (version) => JSON.parse(readFileSync(fileURLToPath(new URL(`../../fixtures/model-form-contracts/${version}.json`, import.meta.url)), 'utf8'));

const HEYGEN_MODELS = ['heygen-avatar-video', 'heygen-lipsync-speed', 'heygen-lipsync-precision'];

function stableModelShape(model) {
  return {
    fields: model.fields,
    generation_param_fields: model.generation_param_fields,
    omit_common_fields: model.omit_common_fields,
    cost_per_second_usd: model.cost_per_second_usd,
    cost_per_second_with_sound_usd: model.cost_per_second_with_sound_usd,
    cost_per_second_by_resolution: model.cost_per_second_by_resolution,
    cost_per_second_by_resolution_audio: model.cost_per_second_by_resolution_audio
  };
}

test('HeyGen contract bump is additive for existing models', () => {
  const before = readContract('1.0.0');
  const after = readContract('1.1.0');

  assert.equal(before._meta.version, '1.0.0');
  assert.equal(after._meta.version, '1.1.0');
  for (const slug of Object.keys(before.models)) {
    assert.ok(after.models[slug], `${slug} must survive the bump`);
    assert.deepEqual(stableModelShape(after.models[slug]), stableModelShape(before.models[slug]), slug);
  }
  for (const slug of HEYGEN_MODELS) {
    assert.equal(slug in before.models, false, `${slug} is new in 1.1.0`);
    assert.ok(after.models[slug], `${slug} is added by 1.1.0`);
  }
});
