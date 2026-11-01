import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const catalog = JSON.parse(readFileSync(new URL('../studio-data/brands/brands.json', import.meta.url), 'utf8'));

test('brand catalogue holds the seven baseline codes in order', () => {
  assert.deepEqual(catalog.brands.map((b) => b.code), ['DD', 'RH', 'TS', 'HR', 'DV', 'KDP', 'NA']);
  assert.equal(catalog.default_code, 'NA');
});

test('every brand has a name, descriptor and a list of location rules', () => {
  for (const b of catalog.brands) {
    assert.ok(b.name && b.descriptor, b.code);
    assert.ok(Array.isArray(b.segments), b.code);
    for (const seq of b.segments) assert.ok(Array.isArray(seq) && seq.every((s) => s === s.toLowerCase()), `${b.code} segments must be lowercase`);
  }
  assert.deepEqual(catalog.brands.find((b) => b.code === 'NA').segments, []);
});

test('baseline path rules are preserved (compat-lib/media-probe.mjs inferProject)', () => {
  const rule = (code) => catalog.brands.find((b) => b.code === code);
  assert.deepEqual(rule('HR').segments[0], ['content', 'hr']);
  assert.deepEqual(rule('DD').segments[0], ['content', 'damned']);
  assert.deepEqual(rule('RH').segments[0], ['content', 'rh']);
  assert.deepEqual(rule('TS').segments[0], ['content', 'ts']);
  assert.deepEqual(rule('DV').segments[0], ['launches', 'puck']);
  assert.deepEqual(rule('KDP').segments[0], ['kdp']);
  assert.deepEqual(rule('KDP').segment_prefixes, ['book-']);
});
