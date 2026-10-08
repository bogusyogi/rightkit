import { readdir, lstat, realpath } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { join, relative } from 'node:path';
import { STORAGE_DEFAULTS, inside, measureTree } from './storage.mjs';
const exec = promisify(execFile);
export async function ownershipStorageFindings({ appRoot, managedRoot, thresholdBytes = STORAGE_DEFAULTS.ownershipThresholdBytes, ignored = async path => { try { await exec('git', ['-C', appRoot, 'check-ignore', '--no-index', '-q', '--', relative(appRoot, path)]); return true; } catch { return false; } } }) {
  const findings = [];
  const walk = async path => {
    let total = 0;
    for (const entry of await readdir(path, { withFileTypes: true })) {
      if (entry.name === '.git') continue;
      const child = join(path, entry.name); const s = await lstat(child);
      if (s.isSymbolicLink()) {
        const target = await realpath(child).catch(() => null); if (!target) continue;
        if (inside(target, managedRoot) && await ignored(child)) continue;
        // Unmanaged symlinks are not scanned: avoid walking volumes or cyclic trees.
        continue;
      }
      if (!s.isDirectory()) { total += s.size; continue; }
      if (inside(await realpath(child), managedRoot) && await ignored(child)) continue;
      const bytes = await walk(child); total += bytes;
      if (bytes > thresholdBytes) findings.push({ path: child, bytes, reason: 'repository-directory-exceeds-5GB-outside-ignored-managed-path' });
    }
    return total;
  };
  await walk(appRoot); return findings;
}
