import { lstat, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, normalize, relative, resolve, sep } from 'node:path';

export const CACHE_PROOF_MARKER = '.rightkit-cache-proof.json';
const CACHE_PROOF_SCHEMA = 'rightkit.cache-proof-target.v1';

const comparable = (value, platform = process.platform) => {
  const selected = normalize(resolve(value));
  return platform === 'win32' ? selected.toLowerCase() : selected;
};

/** Refuse every broad cleanup target; only one live fixture generation target is valid. */
export function assertCacheProofTarget({ target, expectedTarget, managedRoot, fixtureWorkspace, platform = process.platform } = {}) {
  for (const [label, value] of Object.entries({ target, expectedTarget, managedRoot, fixtureWorkspace })) if (typeof value !== 'string' || !value || !isAbsolute(value)) throw new TypeError(`${label} must be an absolute path`);
  const selected = comparable(target, platform);
  if (selected !== comparable(expectedTarget, platform)) throw new Error('cache proof cleanup target does not match live fixture generation');
  if (comparable(fixtureWorkspace, platform) !== comparable(join(managedRoot, 'fixture-rust'), platform)) throw new Error('cache proof workspace must be managed fixture-rust');
  const protectedRoots = [managedRoot, fixtureWorkspace, dirname(managedRoot), resolve(managedRoot, sep)];
  if (protectedRoots.some((value) => selected === comparable(value, platform))) throw new Error('cache proof cleanup refuses protected root');
  const workspaceStore = join(managedRoot, 'rightkit-build-control', 'ws');
  const segments = relative(workspaceStore, target).split(/[\\/]/);
  if (segments.length !== 5 || !/^[a-f0-9]{12}$/.test(segments[0]) || segments[1] !== 'g' || !/^\d{3}$/.test(segments[2]) || segments[3] !== 'target' || segments[4] !== '') {
    // path.relative never retains a trailing empty segment; accept canonical 4-part shape.
    if (segments.length !== 4 || !/^[a-f0-9]{12}$/.test(segments[0]) || segments[1] !== 'g' || !/^\d{3}$/.test(segments[2]) || segments[3] !== 'target') throw new Error('cache proof cleanup target is outside managed fixture target layout');
  }
  if (basename(target) !== 'target') throw new Error('cache proof cleanup target must be target directory');
  return Object.freeze({ target: normalize(target), fixtureWorkspace: normalize(fixtureWorkspace), managedRoot: normalize(managedRoot) });
}

export async function initializeCacheProofTarget(options) {
  const proof = assertCacheProofTarget(options);
  await mkdir(proof.target, { recursive: true });
  const metadata = await lstat(proof.target);
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) throw new Error('cache proof target must be a real directory');
  const marker = { schema: CACHE_PROOF_SCHEMA, target: proof.target, fixtureWorkspace: proof.fixtureWorkspace };
  await writeFile(join(proof.target, CACHE_PROOF_MARKER), `${JSON.stringify(marker)}\n`, { flag: 'w', mode: 0o600 });
  return proof;
}

export async function clearCacheProofTarget(options) {
  const proof = assertCacheProofTarget(options);
  const markerPath = join(proof.target, CACHE_PROOF_MARKER);
  const marker = JSON.parse(await readFile(markerPath, 'utf8'));
  if (marker.schema !== CACHE_PROOF_SCHEMA || comparable(marker.target, options.platform) !== comparable(proof.target, options.platform) || comparable(marker.fixtureWorkspace, options.platform) !== comparable(proof.fixtureWorkspace, options.platform)) throw new Error('cache proof target marker mismatch');
  const entries = await readdir(proof.target, { withFileTypes: true });
  for (const entry of entries) {
    if (entry.name === CACHE_PROOF_MARKER) continue;
    const child = join(proof.target, entry.name);
    if (dirname(child) !== proof.target) throw new Error('cache proof cleanup escaped target');
    await rm(child, { recursive: true, force: true, ...(options.platform === 'win32' ? { maxRetries: 3, retryDelay: 100 } : {}) });
  }
  return Object.freeze({ target: proof.target, removed: entries.filter((entry) => entry.name !== CACHE_PROOF_MARKER).map((entry) => entry.name) });
}
