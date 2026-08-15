import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

const ADDON_PATH = fileURLToPath(new URL('../native/windows-job/build/Release/rightkit_windows_job.node', import.meta.url));
const require = createRequire(import.meta.url);

let cached;
/** Loads the compiled Job Object addon, or null if it has not been built yet
 * — `node-gyp rebuild` inside native/windows-job/ on the target Windows
 * machine produces it. Callers must fail closed, not fall back, when this
 * returns null: that is the exact contract process-tree.mjs's
 * WINDOWS_CONTAINMENT_CONTRACT already enforces. */
export function loadWindowsJobAddon({ path = ADDON_PATH, exists = existsSync } = {}) {
  if (cached !== undefined) return cached;
  if (!exists(path)) { cached = null; return cached; }
  try { cached = require(path); }
  catch { cached = null; }
  return cached;
}

/** Test-only: clears the module-level cache so a test can simulate a missing
 * or present addon without process isolation. */
export function resetWindowsJobAddonCacheForTests() { cached = undefined; }
