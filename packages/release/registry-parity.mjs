#!/usr/bin/env node
import { realpathSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from './background-process.mjs';
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, join, relative, resolve } from 'node:path';
import { RENAMED_AWAY_NPM } from './renamed-away.mjs';
import { fileURLToPath } from 'node:url';

const packageRoot = fileURLToPath(new URL('.', import.meta.url));

async function files(root, relative = '') {
  const entries = await readdir(join(root, relative), { withFileTypes: true });
  const result = [];
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    const path = relative ? `${relative}/${entry.name}` : entry.name;
    if (entry.isDirectory()) result.push(...await files(root, path));
    else if (entry.isFile()) result.push(path);
  }
  return result;
}

export async function packedTree(root) {
  const paths = await files(root);
  const values = [];
  for (const path of paths) {
    const bytes = await readFile(join(root, path));
    values.push([path, createHash('sha256').update(bytes).digest('hex')]);
  }
  return values;
}

export function comparePackedTrees(local, published) {
  const left = JSON.stringify(local); const right = JSON.stringify(published);
  if (left === right) return Object.freeze({ equal: true, differences: [] });
  const localMap = new Map(local); const publishedMap = new Map(published);
  const differences = [...new Set([...localMap.keys(), ...publishedMap.keys()])]
    .sort()
    .filter((path) => localMap.get(path) !== publishedMap.get(path));
  return Object.freeze({ equal: false, differences: Object.freeze(differences) });
}

export function packedArchivePath(output) {
  const parsed = typeof output === 'string' ? JSON.parse(output) : output;
  const record = Array.isArray(parsed) ? parsed[0] : parsed;
  if (typeof record?.filename !== 'string' || !record.filename.trim()) {
    throw new Error('pnpm pack output omitted filename');
  }
  return record.filename;
}

function runPnpm(args, options = {}) {
  const npmExecPath = process.env.npm_execpath;
  if (typeof npmExecPath !== 'string' || !npmExecPath.trim()) {
    throw new Error('registry parity must run through the pnpm package script');
  }
  return execFileSync(process.execPath, [npmExecPath, ...args], options);
}

async function extract(archive, destination) {
  await mkdir(destination, { recursive: true });
  // GNU tar reads the colon in an absolute Windows path (C:\...) as a remote
  // host spec, and MSYS tar rejects absolute Windows -C targets; run from the
  // destination with a relative archive path so no absolute path reaches tar.
  execFileSync('tar', ['-xzf', relative(destination, archive)], { cwd: destination, stdio: 'pipe' });
  return join(destination, 'package');
}

// Every npm package RightKit publishes: the names in rightkit-versions.json (adopted, staged, legacy).
export async function publishedPackageNames() {
  const versions = JSON.parse(await readFile(join(packageRoot, 'rightkit-versions.json'), 'utf8'));
  return [...new Set([...Object.keys(versions.npm ?? {}), ...Object.keys(versions.stagedNpm ?? {}), ...Object.keys(versions.legacyNpm ?? {})])].sort();
}

// name -> directory for every package under packages/*, resolved by package.json `name`.
async function treePackageDirs() {
  const dirs = new Map();
  for (const entry of await readdir(join(packageRoot, '..'), { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const dir = join(packageRoot, '..', entry.name);
    try { dirs.set(JSON.parse(await readFile(join(dir, 'package.json'), 'utf8')).name, dir); } catch { /* not a package */ }
  }
  return dirs;
}

export async function verifyAllRegistryParity({ allowUnpublished = false, fetchImpl = fetch, names } = {}) {
  const results = [];
  for (const name of names ?? await publishedPackageNames()) {
    results.push(await verifyRegistryParity({ allowUnpublished, fetchImpl, packageName: name }));
  }
  return Object.freeze(results);
}

export async function verifyRegistryParity({ allowUnpublished = false, fetchImpl = fetch, packageName = '@rightkit/release' } = {}) {
  const sourceRoot = packageName === '@rightkit/release' ? packageRoot : (await treePackageDirs()).get(packageName);
  if (!sourceRoot) {
    const successor = RENAMED_AWAY_NPM.get(packageName);
    if (successor) return Object.freeze({ status: 'RENAMED_AWAY', name: packageName, reason: `renamed to ${successor}; no tree package` });
    throw new Error(`no packages/* directory has package.json name ${packageName}`);
  }
  const manifest = JSON.parse(await readFile(join(sourceRoot, 'package.json'), 'utf8'));
  if (manifest.name !== packageName) throw new Error(`source ${sourceRoot} is ${manifest.name}, expected ${packageName}`);
  const encodedName = encodeURIComponent(manifest.name);
  const metadata = await fetchImpl(`https://registry.npmjs.org/${encodedName}/${manifest.version}`);
  if (metadata.status === 404 && allowUnpublished) return Object.freeze({ status: 'UNPUBLISHED', name: manifest.name, version: manifest.version });
  if (!metadata.ok) throw new Error(`registry metadata failed: HTTP ${metadata.status}`);
  const record = await metadata.json();
  if (typeof record?.dist?.tarball !== 'string') throw new Error('registry metadata omitted dist.tarball');
  const root = await mkdtemp(join(tmpdir(), 'rightkit-release-parity-'));
  try {
    const localDir = join(root, 'local'); const publishedArchive = join(root, 'published.tgz');
    await mkdir(localDir, { recursive: true });
    const packed = runPnpm(['pack', '--pack-destination', localDir, '--json'], { cwd: sourceRoot, encoding: 'utf8' });
    const localArchive = join(localDir, basename(packedArchivePath(packed)));
    const response = await fetchImpl(record.dist.tarball);
    if (!response.ok) throw new Error(`registry tarball failed: HTTP ${response.status}`);
    await writeFile(publishedArchive, Buffer.from(await response.arrayBuffer()));
    const localTree = await packedTree(await extract(localArchive, join(root, 'local-tree')));
    const publishedTree = await packedTree(await extract(publishedArchive, join(root, 'published-tree')));
    const comparison = comparePackedTrees(localTree, publishedTree);
    if (!comparison.equal) throw new Error(`published ${manifest.name}@${manifest.version} differs from source pack: ${comparison.differences.join(', ')}`);
    return Object.freeze({ status: 'MATCH', name: manifest.name, version: manifest.version, files: localTree.length });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const flag = process.argv.indexOf('--package');
  const allowUnpublished = process.argv.includes('--allow-unpublished');
  (flag > 0 ? verifyRegistryParity({ allowUnpublished, packageName: process.argv[flag + 1] }) : verifyAllRegistryParity({ allowUnpublished }))
    .then((result) => process.stdout.write(`${JSON.stringify(result)}\n`))
    .catch((error) => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });
}
