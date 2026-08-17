import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

const ADDON_PATH = fileURLToPath(new URL('../native/windows-job/build/Release/rightkit_windows_job.node', import.meta.url));
const RUNTIME_MANIFEST_PATH = fileURLToPath(new URL('../../runtime-manifest.json', import.meta.url));
const ADDON_RELATIVE_PATH = 'build-control-broker/native/windows-job/build/Release/rightkit_windows_job.node';
const require = createRequire(import.meta.url);

let cached;
/** Loads the compiled Job Object addon, or null if it has not been built yet
 * — `node-gyp rebuild` inside native/windows-job/ on the target Windows
 * machine produces it. Callers must fail closed, not fall back, when this
 * returns null: that is the exact contract process-tree.mjs's
 * WINDOWS_CONTAINMENT_CONTRACT already enforces. */
export function loadWindowsJobAddon({ path = ADDON_PATH, exists = existsSync, manifestPath = RUNTIME_MANIFEST_PATH, readFile = readFileSync } = {}) {
  if (cached !== undefined) return cached;
  if (!exists(path)) { cached = null; return cached; }
  try {
    if (!lstatSync(path).isFile()) { cached = null; return cached; }
    const manifest = JSON.parse(readFile(manifestPath, 'utf8').replace(/^\uFEFF/, ''));
    const descriptor = manifest?.windowsJobAddon;
    if (descriptor?.path !== ADDON_RELATIVE_PATH || !/^[0-9a-f]{64}$/i.test(descriptor.sha256 ?? '') || !Number.isSafeInteger(descriptor.sizeBytes) || descriptor.sizeBytes !== readFile(path).length) { cached = null; return cached; }
    const digest = createHash('sha256').update(readFile(path)).digest('hex');
    if (digest !== descriptor.sha256.toLowerCase()) { cached = null; return cached; }
  } catch { cached = null; return cached; }
  try { cached = require(path); }
  catch { cached = null; }
  return cached;
}

/** Test-only: clears the module-level cache so a test can simulate a missing
 * or present addon without process isolation. */
export function resetWindowsJobAddonCacheForTests() { cached = undefined; }
