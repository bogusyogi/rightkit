import { realpathSync } from 'node:fs';
import { resolveManagedRoots } from '../../release/managed-roots.mjs';
import { createHash } from 'node:crypto';
import { copyFile, mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const packageRoot = resolve(fileURLToPath(new URL('..', import.meta.url)));
const files = [
  'ownership-write-hook-cli.mjs',
  'ownership-write-hook.mjs',
  'ownership-source.mjs',
  'ownership-availability.mjs',
  'ownership-session-hook-cli.mjs',
  'ownership-session-hook.mjs',
  'ownership-fingerprints.json',
  'ownership-session-catalog.json',
  'ownership-session-versions.json',
  'rightkit-drift.mjs',
  'background-process.mjs',
].sort();

function rootFor(platform) {
  if (platform === 'win32') return 'D:\\.rightkit-managed\\agent-policy';
  if (platform === 'darwin') return resolveManagedRoots({ platform }).agentPolicy;
  throw new Error(`Unsupported platform: ${platform}`);
}

async function readPolicySnapshot() {
  const entries = [];
  const hash = createHash('sha256');
  for (const name of files) {
    const bytes = await readFile(join(packageRoot, 'src', name));
    hash.update(name).update('\0').update(bytes).update('\0');
    entries.push({ name, bytes, sha256: createHash('sha256').update(bytes).digest('hex') });
  }
  return { digest: hash.digest('hex'), entries };
}

async function matches(directory, entries) {
  try {
    for (const entry of entries) {
      const bytes = await readFile(join(directory, 'src', entry.name));
      if (!bytes.equals(entry.bytes)) return false;
    }
    return true;
  } catch { return false; }
}

export async function installPolicyAssets({ root = rootFor(process.platform) } = {}) {
  const { digest, entries } = await readPolicySnapshot();
  const installRoot = join(root, `content-${digest}`);
  const src = join(installRoot, 'src');
  if (!(await matches(installRoot, entries))) {
    await mkdir(root, { recursive: true });
    const staging = join(root, `.staging-${digest}-${process.pid}`);
    await rm(staging, { recursive: true, force: true });
    await mkdir(join(staging, 'src'), { recursive: true });
    try {
      for (const entry of entries) await copyFile(join(packageRoot, 'src', entry.name), join(staging, 'src', entry.name));
      await writeFile(join(staging, 'manifest.json'), `${JSON.stringify({
        schema: 'rightkit.agent-policy-assets.v1',
        digest,
        files: entries.map(({ name, sha256 }) => ({ name, sha256 })),
      }, null, 2)}\n`, { flag: 'wx' });
      try { await rename(staging, installRoot); }
      catch (error) {
        if (!(await matches(installRoot, entries))) throw error;
        await rm(staging, { recursive: true, force: true });
      }
    } catch (error) {
      await rm(staging, { recursive: true, force: true });
      throw error;
    }
  }
  const manifest = JSON.parse(await readFile(join(installRoot, 'manifest.json'), 'utf8'));
  if (manifest.digest !== digest || !(await matches(installRoot, entries))) throw new Error(`Installed policy asset digest mismatch: ${installRoot}`);
  return Object.freeze({ digest, installRoot, writeHook: join(src, 'ownership-write-hook-cli.mjs'), sessionHook: join(src, 'ownership-session-hook-cli.mjs') });
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const result = await installPolicyAssets();
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
