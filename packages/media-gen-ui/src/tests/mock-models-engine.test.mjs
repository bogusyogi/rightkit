import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';

// Drift guard: every model the browser mock lists must carry the engine's label, kind and est_usd
// exactly as the Rust provider tables state them (crates/rightkit-media-gen/src/providers/*.rs).

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

function mockRows() {
  const mock = readFileSync(new URL('../lib/browser-mock-api.ts', import.meta.url), 'utf8');
  const start = mock.indexOf('const MOCK_MODELS');
  const block = mock.slice(start, mock.indexOf('\n];', start));
  return [...block.matchAll(/\{ provider: '([^']+)', model: '([^']+)', kind: '([^']+)', label: '([^']+)', est_usd: ([\d.]+)/g)].map(
    ([, provider, model, kind, label, est]) => ({ provider, model, kind, label, est: Number(est) })
  );
}

function engineSource() {
  const dir = new URL('../../../../crates/rightkit-media-gen/src/providers/', import.meta.url);
  return readdirSync(dir)
    .filter((f) => f.endsWith('.rs'))
    .map((f) => readFileSync(new URL(f, dir), 'utf8'))
    .join('\n');
}

test('mock model rows match the engine label, kind and est_usd', () => {
  const rows = mockRows();
  assert.ok(rows.length >= 25, `expected the mock table, found ${rows.length} rows`);
  const rust = engineSource();
  const consts = new Map([...rust.matchAll(/const\s+([A-Z0-9_]+)\s*:\s*f64\s*=\s*([\d.]+)\s*;/g)].map((m) => [m[1], Number(m[2])]));
  for (const row of rows) {
    // `\s*` after `label:` and `est_usd:` tolerates rustfmt moving a long label literal to its own line.
    const found = rust.match(new RegExp(`kind: Kind::(\\w+),\\s*label:\\s*"${escapeRe(row.label)}",\\s*est_usd:\\s*([A-Z0-9_.]+),`));
    assert.ok(found, `${row.provider}/${row.model}: no engine ModelInfo carries this label`);
    assert.equal(found[1].toLowerCase(), row.kind, `${row.provider}/${row.model}: kind`);
    const literal = /^[\d.]+$/.test(found[2]) ? Number(found[2]) : consts.get(found[2]);
    assert.notEqual(literal, undefined, `${row.provider}/${row.model}: unresolved est_usd ${found[2]}`);
    assert.equal(row.est, literal, `${row.provider}/${row.model}: est_usd`);
  }
});

test('mock model rows do not list HeyGen or InfiniteTalk at the stale prices', () => {
  const rows = mockRows();
  assert.equal(rows.find((r) => r.model === 'avatar-standard').est, 0.5);
  assert.equal(rows.find((r) => r.model === 'avatar-iv').est, 2);
  assert.equal(rows.find((r) => r.model === 'infinitetalk').est, 0.3);
});
