// PENDING 36: the UI reads the model catalogue from the engine and never hard-codes a model.
// This scan fails when a quoted engine model id or studio slug appears in src/views (non-test files)
// unless the (file, literal) pair is listed in engine-model-allowlist.json with a reason. Stale entries
// fail too, so the allow-list shrinks as the engine facts arrive.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '..', '..');
const viewsDir = join(repo, 'src', 'views');
const allowFile = join(here, 'engine-model-allowlist.json');

// Distinctive model-family tokens. Provider names (elevenlabs, heygen, fal, wavespeed) are not model ids
// and are not scanned; a bare `elevenlabs-` or `fal-ai/` prefix is.
const MODEL_TOKEN = /flux|kling|seedance|recraft|nano-banana|gpt-image|eleven_|elevenlabs-|minimax-speech|musicgen|qwen|imagen|mmaudio|topaz|veo[0-9.]|\bwan-|fal-ai\//i;
const QUOTED = /(['"`])([^'"`\s]{2,})\1/g;

function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else if (/\.(ts|tsx)$/.test(name) && !/\.test\./.test(name)) out.push(full);
  }
  return out;
}

/** Every (file, literal) pair in src/views whose quoted text looks like an engine model id or studio slug. */
function scan() {
  const hits = new Set();
  for (const file of walk(viewsDir)) {
    const rel = relative(repo, file).split('\\').join('/');
    const lines = readFileSync(file, 'utf8').split('\n');
    for (const line of lines) {
      for (const m of line.matchAll(QUOTED)) {
        if (MODEL_TOKEN.test(m[2])) hits.add(`${rel}\u0000${m[2]}`);
      }
    }
  }
  return [...hits].map((h) => {
    const [file, literal] = h.split('\u0000');
    return { file, literal };
  });
}

const allow = JSON.parse(readFileSync(allowFile, 'utf8'));

test('the scan recognises model ids and ignores provider names', () => {
  assert.match('flux-2-pro', MODEL_TOKEN);
  assert.match('fal-ai/flux-2/klein/9b', MODEL_TOKEN);
  assert.match('eleven_v4_turbo', MODEL_TOKEN);
  assert.match('veo3.1/fast', MODEL_TOKEN);
  assert.doesNotMatch('elevenlabs', MODEL_TOKEN);
  assert.doesNotMatch('heygen', MODEL_TOKEN);
  assert.doesNotMatch('overview', MODEL_TOKEN);
});

test('no hard-coded engine model id appears in src/views outside the allow-list', () => {
  const allowed = new Set(allow.map((e) => `${e.file}\u0000${e.literal}`));
  const unlisted = scan().filter((h) => !allowed.has(`${h.file}\u0000${h.literal}`));
  assert.deepEqual(
    unlisted,
    [],
    `Hard-coded engine model ids in src/views. Read the model from the engine list (models, studio catalog) or, if the engine lacks the fact, add a justified entry to src/tests/engine-model-allowlist.json:\n${unlisted.map((h) => `  ${h.file}: ${h.literal}`).join('\n')}`
  );
});

test('every allow-list entry still matches a live literal and has a reason', () => {
  const live = new Set(scan().map((h) => `${h.file}\u0000${h.literal}`));
  for (const entry of allow) {
    assert.ok(entry.reason && entry.reason.length > 10, `allow-list entry needs a reason: ${JSON.stringify(entry)}`);
    assert.ok(live.has(`${entry.file}\u0000${entry.literal}`), `stale allow-list entry (no longer in src/views): ${JSON.stringify(entry)}`);
  }
});
