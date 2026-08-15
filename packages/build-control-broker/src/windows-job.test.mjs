import assert from 'node:assert/strict';
import test from 'node:test';
import { loadWindowsJobAddon, resetWindowsJobAddonCacheForTests } from './windows-job.mjs';

test('addon loader returns null, not throws, when the addon has not been built', () => {
  resetWindowsJobAddonCacheForTests();
  const addon = loadWindowsJobAddon({ exists: () => false });
  assert.equal(addon, null);
});

test('addon loader caches its result across calls', () => {
  resetWindowsJobAddonCacheForTests();
  let existsCalls = 0;
  loadWindowsJobAddon({ exists: () => { existsCalls += 1; return false; } });
  loadWindowsJobAddon({ exists: () => { existsCalls += 1; return false; } });
  assert.equal(existsCalls, 1);
});

test('addon loader returns null rather than throwing when the file exists but fails to load', () => {
  resetWindowsJobAddonCacheForTests();
  // A real .node file that is not a valid binary for this Node ABI would
  // throw from require(); the loader must degrade to fail-closed, not crash
  // the broker.
  const addon = loadWindowsJobAddon({ exists: () => true, path: import.meta.url });
  assert.equal(addon, null);
});
