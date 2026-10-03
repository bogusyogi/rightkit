import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const ADDON_PATH = fileURLToPath(new URL('../native/macos-resource/build/Release/rightkit_macos_resource.node', import.meta.url));
const RUNTIME_MANIFEST_PATH = fileURLToPath(new URL('../../runtime-manifest.json', import.meta.url));
const ADDON_RELATIVE_PATH = 'build-control-broker/native/macos-resource/build/Release/rightkit_macos_resource.node';
const require = createRequire(import.meta.url);

let cached;

/**
 * Load the installer-owned macOS resource addon. Source checkouts deliberately
 * return null until a native runtime is staged; callers preserve typed
 * UNAVAILABLE results instead of using ps CPU estimates as native telemetry.
 */
export function loadMacosResourceAddon({ path = ADDON_PATH, exists = existsSync, manifestPath = RUNTIME_MANIFEST_PATH, readFile = readFileSync } = {}) {
  if (cached !== undefined) return cached;
  if (!exists(path)) { cached = null; return cached; }
  try {
    if (!lstatSync(path).isFile()) { cached = null; return cached; }
    const manifest = JSON.parse(readFile(manifestPath, 'utf8').replace(/^\uFEFF/, ''));
    const descriptor = manifest?.macosResourceAddon;
    if (descriptor?.path !== ADDON_RELATIVE_PATH || !/^[0-9a-f]{64}$/i.test(descriptor.sha256 ?? '') || !Number.isSafeInteger(descriptor.sizeBytes) || descriptor.sizeBytes !== readFile(path).length) { cached = null; return cached; }
    const digest = createHash('sha256').update(readFile(path)).digest('hex');
    if (digest !== descriptor.sha256.toLowerCase()) { cached = null; return cached; }
  } catch { cached = null; return cached; }
  try { cached = require(path); }
  catch { cached = null; }
  return cached;
}

export function resetMacosResourceAddonCacheForTests() { cached = undefined; }
