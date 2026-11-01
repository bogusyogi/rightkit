import test from 'node:test';
import assert from 'node:assert/strict';

await import('./ts-resolve.mjs');
const { parseVastKey, vastKeyStatusText } = await import('../views/vast-key-logic.ts');

test('an empty or spaced key is refused before anything is sent', () => {
  assert.ok('error' in parseVastKey('   '));
  assert.ok('error' in parseVastKey('ab cd'));
  assert.ok('error' in parseVastKey('ab\ncd'));
});

test('a pasted key is trimmed and returned for saving', () => {
  assert.deepEqual(parseVastKey('  abc123  '), { key: 'abc123' });
});

test('the status line says what a missing key blocks and never shows a key', () => {
  assert.match(vastKeyStatusText({ vast_key_stored: false }), /refused until one is saved/);
  assert.match(vastKeyStatusText({ vast_key_stored: true }), /Saved in your system keychain/);
  assert.match(vastKeyStatusText(null), /Checking/);
});
