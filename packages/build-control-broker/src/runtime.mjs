import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { createRequire } from 'node:module';
import { chmod, cp, lstat, mkdir, mkdtemp, open, readFile, readdir, readlink, realpath, rename, rm, symlink, writeFile } from 'node:fs/promises';
import { execFile as nodeExecFile, spawn } from 'node:child_process';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { promisify } from 'node:util';
import { BUILD_CONTROL_PROTOCOL, RUNTIME_MANIFEST_SCHEMA, normalizeRuntimeIdentity } from '@rightkit/build-control-core';
import { spawnManagedProcess, startManagedProcess, terminateProcessTree } from './process-tree.mjs';
import { safeResourceSnapshot } from './resource-telemetry.mjs';

// Windows editors (PowerShell Set-Content/Out-File, Notepad) write a leading
// UTF-8 BOM by default; JSON.parse does not strip it & fails opaquely.
function stripBom(text) { return typeof text === 'string' && text.charCodeAt(0) === 0xfeff ? text.slice(1) : text; }

const packageNames = ['build-control-broker', 'build-control-core', 'build-control-clients'];
/** Ownership scanner bundle: the @rightkit/release scan worker, its relative-import closure and the
 * aggregate catalog, read from the committed source revision (never the working tree, so in-flight
 * edits neither leak into nor block a runtime) and staged so an agent cannot swap the scanner under a
 * live broker. */
export const OWNERSHIP_SCANNER_DIR = 'ownership-scanner';
export const OWNERSHIP_SCANNER_ENTRIES = Object.freeze(['ownership-scan-worker.mjs', 'rightkit-ownership.json']);
const MODULE_SPECIFIERS = /^\s*(?:import|export)\s[^'"`;]*?from\s*['"]([^'"]+)['"]|^\s*import\s*['"]([^'"]+)['"]/gm;

/** Committed bytes of the scanner closure plus the bare packages it imports. */
export async function committedOwnershipScanner(sourcePackagesRoot, revision) {
  const repository = resolve(await gitAt(sourcePackagesRoot, ['rev-parse', '--show-toplevel']));
  const releaseScoped = `${normalizeGitScopedPath(relative(repository, resolve(sourcePackagesRoot)))}/release`;
  const files = new Map(); const packages = new Set(); const queue = [...OWNERSHIP_SCANNER_ENTRIES];
  while (queue.length) {
    const name = queue.shift(); if (files.has(name)) continue;
    const bytes = await gitBlob(repository, `${revision}:${releaseScoped}/${name}`);
    files.set(name, bytes);
    if (!name.endsWith('.mjs')) continue;
    for (const match of bytes.toString('utf8').matchAll(MODULE_SPECIFIERS)) {
      const specifier = match[1] ?? match[2];
      if (specifier.startsWith('node:')) continue;
      if (specifier.startsWith('./')) {
        const target = normalizeGitScopedPath(join(dirname(name), specifier.slice(2)));
        if (target.startsWith('..')) throw new Error(`ownership scanner import escapes @rightkit/release: ${name} -> ${specifier}`);
        queue.push(target); continue;
      }
      if (specifier.startsWith('.') || isAbsolute(specifier)) throw new Error(`ownership scanner import escapes @rightkit/release: ${name} -> ${specifier}`);
      packages.add(specifier.split('/').slice(0, specifier.startsWith('@') ? 2 : 1).join('/'));
    }
  }
  return Object.freeze({ files, packages: [...packages].sort(), releasePackageJson: await gitBlob(repository, `${revision}:${releaseScoped}/package.json`) });
}
export const WINDOWS_JOB_ADDON_RELATIVE_PATH = 'build-control-broker/native/windows-job/build/Release/rightkit_windows_job.node';
export const MACOS_RESOURCE_ADDON_RELATIVE_PATH = 'build-control-broker/native/macos-resource/build/Release/rightkit_macos_resource.node';
const execFile = promisify(nodeExecFile);
export const RUNTIME_ACTIVATION_FENCE_SCHEMA = 'rightkit.build-control.runtime-activation.v1';
export const normalizeGitScopedPath = (path) => path.replaceAll('\\', '/');
const pathEscapesRoot = (root, candidate) => {
  const scoped = relative(resolve(root), resolve(candidate));
  return isAbsolute(scoped) || scoped === '..' || scoped.startsWith(`..${sep}`);
};

/** Materialize an immutable runtime bundle. This function never changes runtime-current or a service. */
export async function stageManagedRuntime({ externalRoot, sourcePackagesRoot, version, sourceRevision, sourceTreeDigest, platform = process.platform, windowsJobAddon, macosResourceAddon }) {
  if (!externalRoot || !sourcePackagesRoot || !version) throw new TypeError('externalRoot, sourcePackagesRoot & version are required');
  if (!/^[A-Za-z0-9._-]+$/.test(version)) throw new TypeError('runtime version must be a safe immutable identifier');
  const root = resolve(externalRoot); const source = resolve(sourcePackagesRoot); const provenance = await verifySourceProvenance(source, { sourceRevision, sourceTreeDigest }); const staging = join(root, `.runtime-${randomUUID()}.staging`);
  await mkdir(root, { recursive: true }); await mkdir(staging, { recursive: false });
  try {
    for (const name of packageNames) { const target = join(staging, name); await mkdir(target, { recursive: true }); await Promise.all([cp(join(source, name, 'src'), join(target, 'src'), { recursive: true }), cp(join(source, name, 'package.json'), join(target, 'package.json'))]); }
    await mkdir(join(staging, 'release'), { recursive: true });
    await cp(join(source, 'release', 'managed-roots.mjs'), join(staging, 'release', 'managed-roots.mjs'));
    await stageRuntimeDependencies(staging, source);
    await stageOwnershipScanner(staging, source, provenance.sourceRevision);
    const addon = platform === 'win32' ? await stageWindowsJobAddon(staging, source, windowsJobAddon) : undefined;
    const macAddon = platform === 'darwin' ? await stageMacosResourceAddon(staging, source, macosResourceAddon) : undefined;
    await linkRuntimePackages(staging);
    const contentDigest = await digestTree(staging, { excludeManifest: true });
    const manifest = await writeRuntimeManifest(staging, version, provenance, addon, { platform, macosResourceAddon: macAddon });
    // Directory identity is derived from package content, never from a mutable
    // release label.  The manifest is written before the rename, so a visible
    // runtime is complete and immutable from the first observation.
    const afterCopy = await verifySourceProvenance(source, { sourceRevision: provenance.sourceRevision, sourceTreeDigest: provenance.sourceTreeDigest });
    if (afterCopy.sourceRevision !== provenance.sourceRevision || afterCopy.sourceTreeDigest !== provenance.sourceTreeDigest) throw new Error('source provenance changed while staging runtime');
    const immutableRuntime = join(root, `runtime-${contentDigest}`);
    if (await lstat(immutableRuntime).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error))) {
      const existing = normalizeRuntimeManifest(JSON.parse(stripBom(await readFile(join(immutableRuntime, 'runtime-manifest.json'), 'utf8'))));
      if (existing.schema !== manifest.schema || existing.clientDigest !== manifest.clientDigest || existing.brokerDigest !== manifest.brokerDigest || existing.sourceRevision !== manifest.sourceRevision || existing.sourceTreeDigest !== manifest.sourceTreeDigest) throw new Error(`runtime content-address conflict: ${immutableRuntime}`);
      await rm(staging, { recursive: true, force: true });
      return Object.freeze({ runtime: immutableRuntime, version: existing.version, contentDigest, manifest: existing });
    }
    await makeImmutable(staging, { platform });
    await rename(staging, immutableRuntime);
    await syncDirectory(root, { platform });
    return Object.freeze({ runtime: immutableRuntime, version, contentDigest, manifest });
  } catch (error) { await rm(staging, { recursive: true, force: true }); throw error; }
}

/** Compatibility activation wrapper. Bundle construction itself remains isolated in stageManagedRuntime. */
export async function installManagedRuntime({ externalRoot, sourcePackagesRoot, config, configPath, rollbackSnapshot, stopService, version, sourceRevision, sourceTreeDigest, nodePath = process.execPath, healthTimeoutMs = 8_000, healthWorkspace, taskXmlPath, postSwitchHealth, rollbackSwitch, restartService, probeService, rollbackService, processProbe, stage = stageManagedRuntime, health = healthProbe, platform = process.platform, windowsJobAddon, macosResourceAddon }) {
  if (!externalRoot || !sourcePackagesRoot || !config || !version) throw new TypeError('externalRoot, sourcePackagesRoot, config & version are required');
  if (!Number.isSafeInteger(healthTimeoutMs) || healthTimeoutMs < 100) throw new TypeError('healthTimeoutMs must be at least 100');
  if (typeof healthWorkspace !== 'string' || !isAbsolute(healthWorkspace)) throw new TypeError('absolute healthWorkspace is required for functional runtime activation proof');
  const healthManifest = await lstat(join(healthWorkspace, 'Cargo.toml')).catch(() => null);
  if (!healthManifest?.isFile()) throw new TypeError('healthWorkspace must contain a regular Cargo.toml');
  if (typeof restartService !== 'function' || typeof probeService !== 'function' || typeof rollbackService !== 'function' || typeof processProbe !== 'function') throw new TypeError('restartService, probeService, rollbackService & processProbe are required for activation');
  const root = resolve(externalRoot);
  rollbackSnapshot ??= configPath ? await snapshotRuntimeSelection({ root, configPath }) : null;
  if (rollbackSnapshot && platform === 'darwin' && typeof stopService !== 'function') throw new TypeError('stopService is required to restore config & runtime selection safely');
  const staged = await stage({ externalRoot: root, sourcePackagesRoot, version, sourceRevision, sourceTreeDigest, platform, windowsJobAddon, macosResourceAddon }); const { runtime, manifest, contentDigest } = staged;
  await verifyRuntimeBundle(runtime, manifest, contentDigest);
  const activationFencePath = activationFencePathFor(config, root);
  const activation = await prepareRuntimeActivationFence({ path: activationFencePath, manifest });
  let selected = false; let committed = false; let prior = null; let service;
  try {
    await health({ runtime, config, nodePath, timeoutMs: healthTimeoutMs, healthWorkspace, platform });
    const current = join(root, 'runtime-current'); prior = await readlink(current).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (rollbackSnapshot && rollbackSnapshot.prior !== prior) throw new Error('runtime rollback snapshot differs from prior selection');
    const next = join(root, `.runtime-current-${randomUUID()}`); await symlink(basename(runtime), next); await replaceRuntimeSelection({ root, current, next, platform }); await syncDirectory(root);
    selected = true;
    try {
      await verifyRuntimeBundle(runtime, manifest, contentDigest);
      service = await restartService({ runtime, current, prior, manifest, activation, taskXmlPath, timeoutMs: healthTimeoutMs });
      service = await probeService({ runtime, current, prior, manifest, activation, restart: service, timeoutMs: healthTimeoutMs });
      await assertServiceIdentity(service, runtime, manifest, processProbe);
      await verifyRuntimeBundle(runtime, manifest, contentDigest);
      await commitRuntimeActivationFence(activation);
      committed = true;
    } catch (error) {
      if (committed) throw error;
      if (rollbackSnapshot) {
        await stopService?.();
        await restoreRuntimeSelection(rollbackSnapshot, { platform });
      } else {
        const rollback = join(root, `.runtime-current-${randomUUID()}`); if (prior === null) await rm(current, { force: true }); else { await symlink(prior, rollback); await replaceRuntimeSelection({ root, current, next: rollback, platform }); } await syncDirectory(root);
      }
      selected = false;
      await removeRuntimeActivationFence(activation);
      try { await rollbackService({ prior, current, failedRuntime: runtime, manifest, service, taskXmlPath, timeoutMs: healthTimeoutMs }); }
      catch (rollbackError) { throw new Error(`runtime activation rollback failed: ${rollbackError.message}; activation failure: ${error.message}`); }
      throw Object.assign(new Error(`runtime activation rolled back: ${error.message}`), { runtimeRestored: true });
    }
    if (rollbackSnapshot) await discardRuntimeSnapshot(rollbackSnapshot);
    return Object.freeze({ runtime, current: join(root, 'runtime-current'), version: manifest.version, contentDigest, manifest, activation: Object.freeze({ path: activation.path, tokenDigest: activation.tokenDigest, state: 'COMMITTED' }) });
  } catch (error) {
    if (selected && !committed) throw new Error(`runtime activation selected candidate without rollback: ${error.message}`);
    if (!committed) await removeRuntimeActivationFence(activation).catch(() => {});
    throw error;
  }
}

/** One atomic journal binds the previous pointer to the exact bytes it ran with.
 * Bootstrap must capture this before rewriting config, not at the later switch. */
export async function snapshotRuntimeSelection({ root, configPath, prior, configBytes }) {
  root = resolve(root); configPath = resolve(configPath);
  prior ??= await readlink(join(root, 'runtime-current')).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
  if (prior === null) return null;
  const path = join(root, '.runtime-rollback.json');
  const saved = await readFile(path, 'utf8').catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
  if (saved) {
    const snapshot = JSON.parse(saved);
    if (snapshot.schema !== 'rightkit.runtime-rollback.v1' || snapshot.prior !== prior || snapshot.configPath !== configPath || typeof snapshot.configBytes !== 'string') throw new Error('runtime rollback snapshot differs from prior selection');
    return { ...snapshot, root, path };
  }
  configBytes ??= await readFile(configPath);
  const snapshot = { schema: 'rightkit.runtime-rollback.v1', prior, configPath, configBytes: Buffer.from(configBytes).toString('base64') };
  await writeRuntimeFileAtomic(path, `${JSON.stringify(snapshot)}\n`);
  await syncDirectory(root);
  return { ...snapshot, root, path };
}

async function writeRuntimeFileAtomic(path, bytes) {
  const temporary = `${path}.${randomUUID()}.tmp`;
  try {
    const handle = await open(temporary, 'wx', 0o600);
    try { await handle.writeFile(bytes); await handle.sync(); } finally { await handle.close(); }
    await rename(temporary, path);
  } finally { await rm(temporary, { force: true }); }
}

/** Restore under a stopped service & pending admission fence. Both file
 * replacements are atomic; no restarted runtime can observe a mixed pair. */
export async function restoreRuntimeSelection(snapshot, { platform = process.platform } = {}) {
  const { root, prior, configPath, configBytes } = snapshot;
  const current = join(root, 'runtime-current'); const next = join(root, `.runtime-current-${randomUUID()}`);
  await symlink(prior, next);
  try {
    await writeRuntimeFileAtomic(configPath, Buffer.from(configBytes, 'base64'));
    await replaceRuntimeSelection({ root, current, next, platform });
    await syncDirectory(root, { platform });
  } finally { await rm(next, { force: true }); }
}

export async function discardRuntimeSnapshot(snapshot) { await rm(snapshot.path, { force: true }); }

/** POSIX rename replaces a symlink atomically. Windows refuses to rename over
 * an existing directory link, so preserve it under a unique recovery name,
 * install the candidate, then remove the displaced link. */
export async function replaceRuntimeSelection({ root, current, next, platform = process.platform }) {
  if (platform !== 'win32') { await rename(next, current); return; }
  const displaced = join(root, `.runtime-current-displaced-${randomUUID()}`);
  const present = await lstat(current).then(() => true).catch((error) => error.code === 'ENOENT' ? false : Promise.reject(error));
  if (present) await rename(current, displaced);
  try { await rename(next, current); }
  catch (error) {
    if (present) await rename(displaced, current).catch(() => {});
    throw error;
  }
  if (present) await rm(displaced, { force: true });
}

export async function writeRuntimeManifest(runtime, version, provenance, windowsJobAddon, { platform = process.platform, macosResourceAddon } = {}) {
  const coreDigest = await digestTree(join(runtime, 'build-control-core')); const clientPackageDigest = await digestTree(join(runtime, 'build-control-clients')); const brokerPackageDigest = hash(`${await digestTree(join(runtime, 'build-control-broker'))}:${await digestTree(join(runtime, OWNERSHIP_SCANNER_DIR))}`);
  const identity = normalizeRuntimeIdentity({ schema: RUNTIME_MANIFEST_SCHEMA, version, clientDigest: hash(`${coreDigest}:${clientPackageDigest}`), brokerDigest: hash(`${coreDigest}:${brokerPackageDigest}`) });
  const manifest = normalizeRuntimeManifest({ ...identity, ...provenance, ...(windowsJobAddon ? { windowsJobAddon } : {}), ...(macosResourceAddon ? { macosResourceAddon } : {}) });
  const path = join(runtime, 'runtime-manifest.json'); const handle = await open(path, 'wx', 0o444);
  try { await handle.writeFile(`${JSON.stringify(manifest)}\n`, 'utf8'); await handle.sync(); await handle.chmod(platform === 'win32' ? 0o644 : 0o444); }
  finally { await handle.close(); }
  await syncDirectory(runtime, { platform }); return manifest;
}

async function digestTree(root, { excludeManifest = false } = {}) { const files = []; const walk = async (directory, prefix = '') => { for (const name of (await readdir(directory)).sort()) { if (excludeManifest && !prefix && name === 'runtime-manifest.json') continue; const path = join(directory, name); const metadata = await lstat(path); if (metadata.isSymbolicLink()) continue; if (metadata.isDirectory()) await walk(path, prefix ? `${prefix}/${name}` : name); else files.push([prefix ? `${prefix}/${name}` : name, await readFile(path)]); } }; await walk(root); const value = createHash('sha256'); for (const [path, contents] of files) value.update(path).update('\0').update(contents).update('\0'); return value.digest('hex'); }
const hash = (value) => createHash('sha256').update(value).digest('hex');

async function stageWindowsJobAddon(staging, sourcePackagesRoot, descriptor) {
  if (!descriptor || resolve(descriptor.path ?? '') !== resolve(sourcePackagesRoot, WINDOWS_JOB_ADDON_RELATIVE_PATH) || !/^[0-9a-f]{64}$/i.test(descriptor.sha256 ?? '') || !Number.isSafeInteger(descriptor.sizeBytes) || descriptor.sizeBytes < 1) throw new TypeError('Windows Job Object addon path, digest & size are required');
  const sourceStat = await lstat(descriptor.path).catch(() => null);
  if (!sourceStat?.isFile()) throw new Error('Windows Job Object addon must be a regular file');
  const bytes = await readFile(descriptor.path);
  if (bytes.length !== descriptor.sizeBytes) throw new Error('Windows Job Object addon size mismatch');
  const actual = hash(bytes);
  if (actual !== descriptor.sha256.toLowerCase()) throw new Error('Windows Job Object addon digest mismatch');
  const target = join(staging, WINDOWS_JOB_ADDON_RELATIVE_PATH);
  await mkdir(dirname(target), { recursive: true }); await writeFile(target, bytes);
  return Object.freeze({ path: WINDOWS_JOB_ADDON_RELATIVE_PATH, sha256: actual, sizeBytes: bytes.length });
}

export async function describeWindowsJobAddon(sourcePackagesRoot) {
  const path = resolve(sourcePackagesRoot, WINDOWS_JOB_ADDON_RELATIVE_PATH);
  const metadata = await lstat(path).catch(() => null);
  if (!metadata?.isFile()) throw new Error(`Windows Job Object addon must be a regular file: ${path}`);
  const bytes = await readFile(path).catch(() => null);
  if (!bytes) throw new Error(`Windows Job Object addon is missing: ${path}`);
  return Object.freeze({ path, sha256: hash(bytes), sizeBytes: bytes.length });
}

async function stageMacosResourceAddon(staging, sourcePackagesRoot, descriptor) {
  if (!descriptor || resolve(descriptor.path ?? '') !== resolve(sourcePackagesRoot, MACOS_RESOURCE_ADDON_RELATIVE_PATH) || !/^[0-9a-f]{64}$/i.test(descriptor.sha256 ?? '') || !Number.isSafeInteger(descriptor.sizeBytes) || descriptor.sizeBytes < 1) throw new TypeError('macOS resource addon path, digest & size are required');
  const metadata = await lstat(descriptor.path).catch(() => null);
  if (!metadata?.isFile()) throw new Error('macOS resource addon must be a regular file');
  const bytes = await readFile(descriptor.path);
  if (bytes.length !== descriptor.sizeBytes) throw new Error('macOS resource addon size mismatch');
  if (hash(bytes) !== descriptor.sha256.toLowerCase()) throw new Error('macOS resource addon digest mismatch');
  const target = join(staging, MACOS_RESOURCE_ADDON_RELATIVE_PATH);
  await mkdir(dirname(target), { recursive: true }); await writeFile(target, bytes);
  return Object.freeze({ path: MACOS_RESOURCE_ADDON_RELATIVE_PATH, sha256: hash(bytes), sizeBytes: bytes.length });
}

export async function describeMacosResourceAddon(sourcePackagesRoot) {
  const path = resolve(sourcePackagesRoot, MACOS_RESOURCE_ADDON_RELATIVE_PATH);
  const metadata = await lstat(path).catch(() => null);
  if (!metadata?.isFile()) throw new Error(`macOS resource addon must be a regular file: ${path}`);
  const bytes = await readFile(path);
  return Object.freeze({ path, sha256: hash(bytes), sizeBytes: bytes.length });
}

function normalizeRuntimeManifest(manifest) {
  const identity = normalizeRuntimeIdentity(manifest);
  if (!/^[0-9a-f]{40,64}$/.test(manifest?.sourceRevision ?? '') || !/^[0-9a-f]{40,64}$/.test(manifest?.sourceTreeDigest ?? '')) throw new TypeError('runtime manifest requires immutable source revision & tree digest');
  const addon = manifest?.windowsJobAddon;
  if (addon !== undefined && (addon.path !== WINDOWS_JOB_ADDON_RELATIVE_PATH || !/^[0-9a-f]{64}$/i.test(addon.sha256 ?? '') || !Number.isSafeInteger(addon.sizeBytes) || addon.sizeBytes < 1)) throw new TypeError('runtime manifest has malformed Windows Job Object addon descriptor');
  const macAddon = manifest?.macosResourceAddon;
  if (macAddon !== undefined && (macAddon.path !== MACOS_RESOURCE_ADDON_RELATIVE_PATH || !/^[0-9a-f]{64}$/i.test(macAddon.sha256 ?? '') || !Number.isSafeInteger(macAddon.sizeBytes) || macAddon.sizeBytes < 1)) throw new TypeError('runtime manifest has malformed macOS resource addon descriptor');
  return Object.freeze({ ...identity, sourceRevision: manifest.sourceRevision, sourceTreeDigest: manifest.sourceTreeDigest, ...(addon ? { windowsJobAddon: Object.freeze({ path: addon.path, sha256: addon.sha256.toLowerCase(), sizeBytes: addon.sizeBytes }) } : {}), ...(macAddon ? { macosResourceAddon: Object.freeze({ path: macAddon.path, sha256: macAddon.sha256.toLowerCase(), sizeBytes: macAddon.sizeBytes }) } : {}) });
}

function sameRuntimeIdentity(left, right) {
  const expected = normalizeRuntimeIdentity(right); const observed = normalizeRuntimeIdentity(left);
  return observed.schema === expected.schema && observed.version === expected.version && observed.clientDigest === expected.clientDigest && observed.brokerDigest === expected.brokerDigest;
}

function sameRuntimeManifest(left, right) {
  const expected = normalizeRuntimeManifest(right); const observed = normalizeRuntimeManifest(left);
  return sameRuntimeIdentity(observed, expected) && observed.sourceRevision === expected.sourceRevision && observed.sourceTreeDigest === expected.sourceTreeDigest && JSON.stringify(observed.windowsJobAddon ?? null) === JSON.stringify(expected.windowsJobAddon ?? null) && JSON.stringify(observed.macosResourceAddon ?? null) === JSON.stringify(expected.macosResourceAddon ?? null);
}

async function gitBlob(repository, spec) {
  try { return (await execFile('git', ['-C', repository, 'cat-file', 'blob', spec], { encoding: 'buffer', maxBuffer: 64 * 1024 * 1024 })).stdout; }
  catch (error) { throw new Error(`runtime source provenance unavailable: ${spec}: ${error.stderr?.toString().trim() || error.message}`); }
}

async function gitAt(cwd, args) {
  try { return (await execFile('git', ['-C', cwd, ...args], { encoding: 'utf8' })).stdout.trim(); }
  catch (error) { throw new Error(`runtime source provenance unavailable: ${error.stderr?.trim() || error.message}`); }
}

async function verifySourceProvenance(sourcePackagesRoot, { sourceRevision, sourceTreeDigest } = {}) {
  const repository = resolve(await gitAt(sourcePackagesRoot, ['rev-parse', '--show-toplevel'])); const packages = resolve(sourcePackagesRoot); const filesystemScopedPath = relative(repository, packages);
  if (!filesystemScopedPath || filesystemScopedPath === '.' || pathEscapesRoot(repository, packages)) throw new Error('runtime source packages root must be a committed repository subdirectory');
  const scopedPath = normalizeGitScopedPath(filesystemScopedPath);
  // Only these packages are staged. Unrelated RightKit release/app work must
  // not block promotion of a fully committed managed runtime.
  const runtimePaths = [...packageNames.map((name) => `${scopedPath}/${name}`), `${scopedPath}/release/managed-roots.mjs`];
  const status = await gitAt(repository, ['status', '--porcelain=v1', '--untracked-files=all', '--', ...runtimePaths]);
  if (status) throw new Error(`runtime source packages root is dirty or contains untracked files: ${scopedPath}`);
  const actualRevision = await gitAt(repository, ['rev-parse', 'HEAD']); const actualTree = await gitAt(repository, ['rev-parse', `HEAD:${scopedPath}`]);
  if (sourceRevision !== undefined && sourceRevision !== actualRevision) throw new Error('runtime source revision does not match verified HEAD');
  if (sourceTreeDigest !== undefined && sourceTreeDigest !== actualTree) throw new Error('runtime source tree digest does not match verified tree');
  return Object.freeze({ sourceRevision: actualRevision, sourceTreeDigest: actualTree });
}

function activationFencePathFor(config, root) {
  const path = config?.broker?.activationFencePath;
  if (typeof path !== 'string' || !path) throw new TypeError('broker.activationFencePath is required for runtime activation');
  const resolved = resolve(path);
  if (pathEscapesRoot(root, resolved)) throw new TypeError('broker.activationFencePath must remain under externalRoot');
  return resolved;
}

async function atomicWrite(path, value, mode = 0o600) {
  await mkdir(dirname(path), { recursive: true }); const temporary = join(dirname(path), `.${basename(path)}-${randomUUID()}.tmp`);
  try {
    const handle = await open(temporary, 'wx', mode);
    try { await handle.writeFile(`${JSON.stringify(value)}\n`, 'utf8'); await handle.sync(); await handle.chmod(mode); }
    finally { await handle.close(); }
    await rename(temporary, path); await syncDirectory(dirname(path));
  }
  finally { await rm(temporary, { force: true }).catch(() => {}); }
}

export async function prepareRuntimeActivationFence({ path, manifest }) {
  if (typeof path !== 'string' || !path) throw new TypeError('activation fence path is required');
  const runtime = normalizeRuntimeIdentity(manifest); const token = randomBytes(32).toString('hex'); const tokenDigest = hash(token);
  const activation = Object.freeze({ path: resolve(path), token, tokenDigest, runtime });
  await atomicWrite(activation.path, { schema: RUNTIME_ACTIVATION_FENCE_SCHEMA, state: 'PENDING', runtime, tokenDigest });
  return activation;
}

export async function commitRuntimeActivationFence(activation) {
  if (!activation?.path || !activation?.token || !activation?.tokenDigest || !activation?.runtime) throw new TypeError('prepared runtime activation fence is required');
  if (hash(activation.token) !== activation.tokenDigest) throw new Error('runtime activation token digest mismatch');
  const pending = JSON.parse(stripBom(await readFile(activation.path, 'utf8')));
  if (pending.schema !== RUNTIME_ACTIVATION_FENCE_SCHEMA || pending.state !== 'PENDING' || pending.tokenDigest !== activation.tokenDigest || !sameRuntimeIdentity(pending.runtime, activation.runtime)) throw new Error('runtime activation fence changed before commit');
  await atomicWrite(activation.path, { ...pending, state: 'COMMITTED' });
}

async function removeRuntimeActivationFence(activation) {
  if (!activation?.path) return;
  const current = await readFile(activation.path, 'utf8').then((value) => JSON.parse(stripBom(value))).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
  if (current?.state === 'PENDING' && current.tokenDigest === activation.tokenDigest) { await rm(activation.path, { force: true }); await syncDirectory(dirname(activation.path)); }
}

async function verifyRuntimeBundle(runtime, manifest, contentDigest) {
  const expected = normalizeRuntimeManifest(manifest); const onDisk = normalizeRuntimeManifest(JSON.parse(stripBom(await readFile(join(runtime, 'runtime-manifest.json'), 'utf8'))));
  if (!sameRuntimeManifest(onDisk, expected)) throw new Error('runtime manifest mismatch');
  if (expected.windowsJobAddon) {
    const addonPath = join(runtime, expected.windowsJobAddon.path); const metadata = await lstat(addonPath).catch(() => null); const bytes = metadata?.isFile() ? await readFile(addonPath).catch(() => null) : null;
    if (!bytes || bytes.length !== expected.windowsJobAddon.sizeBytes || hash(bytes) !== expected.windowsJobAddon.sha256) throw new Error('Windows Job Object addon missing or digest mismatch');
  }
  if (expected.macosResourceAddon) {
    const addonPath = join(runtime, expected.macosResourceAddon.path); const metadata = await lstat(addonPath).catch(() => null); const bytes = metadata?.isFile() ? await readFile(addonPath).catch(() => null) : null;
    if (!bytes || bytes.length !== expected.macosResourceAddon.sizeBytes || hash(bytes) !== expected.macosResourceAddon.sha256) throw new Error('macOS resource addon missing or digest mismatch');
  }
  if (contentDigest && await digestTree(runtime, { excludeManifest: true }) !== contentDigest) throw new Error('runtime content digest mismatch');
}

async function assertServiceIdentity(service, runtime, manifest, processProbe) {
  if (!service || !Number.isInteger(Number(service.pid)) || Number(service.pid) <= 0) throw new Error('service probe did not return a live PID');
  const processState = await processProbe({ pid: Number(service.pid), runtime, manifest });
  if (!processState || processState.alive !== true) throw new Error('service probe returned a dead PID');
  const identity = service.runtimeIdentity ?? service.runtime ?? service.runtimePath ?? service.manifest;
  if (!identity) throw new Error('service probe did not return runtime identity');
  if (typeof identity === 'string') {
    const expected = resolve(runtime);
    if (await realpath(identity).catch(() => identity) !== await realpath(expected)) throw new Error('service runtime identity mismatch');
  } else {
    const observed = normalizeRuntimeIdentity(identity);
    if (!sameRuntimeIdentity(observed, manifest)) throw new Error('service runtime identity mismatch');
  }
  const observedRuntime = processState.runtimePath ?? processState.runtime ?? processState.scriptPath;
  if (typeof observedRuntime !== 'string' || !observedRuntime) throw new Error('service process probe did not prove executed runtime path');
  const expectedRuntime = await realpath(runtime);
  const resolvedRuntime = await realpath(observedRuntime).catch(() => resolve(observedRuntime));
  const resolvedScript = join(expectedRuntime, 'build-control-broker', 'src', 'broker.mjs');
  if (resolvedRuntime !== expectedRuntime && resolvedRuntime !== resolvedScript) throw new Error('service process runtime path mismatch');
}

async function makeImmutable(root, { platform = process.platform } = {}) {
  const walk = async (directory) => {
    for (const name of await readdir(directory)) {
      const path = join(directory, name); const metadata = await lstat(path);
      if (metadata.isSymbolicLink()) continue;
      if (metadata.isDirectory()) await walk(path); else { const handle = await open(path, platform === 'win32' ? 'r+' : 'r'); try { await handle.sync(); } finally { await handle.close(); } await chmod(path, 0o444); }
    }
    // Keep directory metadata traversable/reclaimable while making every
    // materialized file immutable; content cannot be changed in place.
    await chmod(directory, 0o755); await syncDirectory(directory, { platform });
  };
  await walk(root);
}

export async function syncDirectoryForPlatform(path, { platform = process.platform, openDirectory = open } = {}) {
  let handle;
  try { handle = await openDirectory(path, 'r'); await handle.sync(); }
  catch (error) { if (platform === 'win32' && error?.code === 'EPERM') return; throw error; }
  finally { await handle?.close(); }
}
const syncDirectory = syncDirectoryForPlatform;

async function linkRuntimePackages(runtime) {
  for (const consumer of ['build-control-broker', 'build-control-clients']) { const directory = join(runtime, consumer, 'node_modules', '@rightkit'); await mkdir(directory, { recursive: true }); await symlink('../../../build-control-core', join(directory, 'build-control-core')); }
  await symlink('../../../build-control-clients', join(runtime, 'build-control-broker', 'node_modules', '@rightkit', 'build-control-clients'));
}

async function stageOwnershipScanner(runtime, sourcePackagesRoot, revision) {
  const releaseRoot = join(sourcePackagesRoot, 'release'); const target = join(runtime, OWNERSHIP_SCANNER_DIR);
  const scanner = await committedOwnershipScanner(sourcePackagesRoot, revision);
  for (const [file, bytes] of scanner.files) { await mkdir(dirname(join(target, file)), { recursive: true }); await writeFile(join(target, file), bytes); }
  const release = JSON.parse(stripBom(scanner.releasePackageJson.toString('utf8')));
  const dependencies = Object.fromEntries(scanner.packages.map((name) => {
    const version = release.dependencies?.[name];
    if (!version) throw new Error(`ownership scanner dependency ${name} is not declared by @rightkit/release`);
    return [name, version];
  }));
  await writeFile(join(target, 'package.json'), `${JSON.stringify({ name: '@rightkit/ownership-scanner-runtime', private: true, type: 'module', version: release.version, dependencies }, null, 2)}\n`);
  const sourceRequire = createRequire(join(releaseRoot, 'package.json')); const fallbackRequire = createRequire(import.meta.url); const seen = new Set();
  for (const [dependency, declaredVersion] of Object.entries(dependencies)) {
    await stageExternalDependency({ dependency, declaredVersion, targetModules: join(target, 'node_modules'), sourceRequire, fallbackRequire, seen });
  }
}

async function stageRuntimeDependencies(runtime, sourcePackagesRoot) {
  const fallbackRequire = createRequire(import.meta.url);
  for (const consumer of packageNames) {
    const sourcePackage = join(sourcePackagesRoot, consumer, 'package.json');
    const manifest = JSON.parse(stripBom(await readFile(sourcePackage, 'utf8')));
    const sourceRequire = createRequire(sourcePackage);
    const targetModules = join(runtime, consumer, 'node_modules'); const seen = new Set();
    for (const [dependency, declaredVersion] of Object.entries(manifest.dependencies ?? {})) {
      if (dependency.startsWith('@rightkit/')) continue;
      await stageExternalDependency({ dependency, declaredVersion, targetModules, sourceRequire, fallbackRequire, seen });
    }
  }
}

async function stageExternalDependency({ dependency, declaredVersion, targetModules, sourceRequire, fallbackRequire, seen }) {
  if (seen.has(dependency)) return;
  seen.add(dependency);
  const entry = resolveDependencyEntry(dependency, sourceRequire, fallbackRequire);
  const root = await dependencyPackageRoot(dependency, entry);
  const manifest = JSON.parse(stripBom(await readFile(join(root, 'package.json'), 'utf8')));
  if (/^\d+\.\d+\.\d+(?:[-+].+)?$/.test(declaredVersion) && manifest.version !== declaredVersion) throw new Error(`runtime dependency ${dependency} resolved ${manifest.version}, expected ${declaredVersion}`);
  const target = join(targetModules, ...dependency.split('/'));
  await mkdir(dirname(target), { recursive: true });
  await cp(root, target, { recursive: true, filter: (path) => basename(path) !== 'node_modules' });
  const nestedRequire = createRequire(join(root, 'package.json'));
  for (const [nested, nestedVersion] of Object.entries(manifest.dependencies ?? {})) {
    await stageExternalDependency({ dependency: nested, declaredVersion: nestedVersion, targetModules, sourceRequire: nestedRequire, fallbackRequire, seen });
  }
}

function resolveDependencyEntry(dependency, sourceRequire, fallbackRequire) {
  try { return sourceRequire.resolve(dependency); }
  catch (sourceError) {
    try { return fallbackRequire.resolve(dependency); }
    catch { throw new Error(`runtime dependency ${dependency} is unavailable: ${sourceError.message}`); }
  }
}

async function dependencyPackageRoot(dependency, entry) {
  let cursor = dirname(await realpath(entry));
  for (;;) {
    const manifest = await readFile(join(cursor, 'package.json'), 'utf8').then((value) => JSON.parse(stripBom(value))).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (manifest?.name === dependency) return cursor;
    const parent = dirname(cursor);
    if (parent === cursor) throw new Error(`runtime dependency package root not found for ${dependency}`);
    cursor = parent;
  }
}

export function loadRuntimeWindowsJobAddon(runtime, { loadAddon = createRequire(import.meta.url) } = {}) {
  const addonPath = join(runtime, 'build-control-broker', 'native', 'windows-job', 'build', 'Release', 'rightkit_windows_job.node');
  try { return loadAddon(addonPath); } catch { return null; }
}

export async function removeHealthProbeRoot(path, { platform = process.platform, remove = rm } = {}) {
  const options = { recursive: true, force: true, ...(platform === 'win32' ? { maxRetries: 3, retryDelay: 100 } : {}) };
  await remove(path, options);
}

export function healthProbeEndpoint(platform, id) {
  if (typeof id !== 'string' || !id) throw new TypeError('health probe id is required');
  return platform === 'win32' ? String.raw`\\.\pipe\rightkit-health-${id}` : join(tmpdir(), `rk-${id}.health.sock`);
}

const availableLoopbackPort = () => new Promise((resolvePort, reject) => {
  const server = net.createServer();
  const finish = (error, port = null) => server.close(() => error ? reject(error) : resolvePort(port));
  server.once('error', reject);
  server.listen({ host: '127.0.0.1', port: 0, exclusive: true }, () => {
    const address = server.address();
    if (!address || typeof address === 'string' || !Number.isSafeInteger(address.port)) return finish(new Error('failed to allocate isolated sccache health port'));
    finish(null, address.port);
  });
});

async function healthProbe({ runtime, config, nodePath, timeoutMs, healthWorkspace, platform = process.platform }) {
  const liveExternal = config.broker?.externalRoot; if (!liveExternal?.path) throw new TypeError('broker external root is required for isolated health probe');
  const probeId = randomUUID();
  let probeRoot;
  if (platform === 'win32') {
    probeRoot = join(liveExternal.path, '.runtime-health', probeId);
    await mkdir(probeRoot, { recursive: true, mode: 0o700 });
  } else {
    // sccache's macOS UDS has a 104-byte sockaddr limit. A full UUID nested in
    // .runtime-health pushes its derived gateway socket past that limit.
    const probeContainer = join(liveExternal.path, '.rh');
    await mkdir(probeContainer, { recursive: true, mode: 0o700 });
    await chmod(probeContainer, 0o700);
    probeRoot = await mkdtemp(join(probeContainer, 'p-'));
  }
  await chmod(probeRoot, 0o700);
  let controlProbeRoot = probeRoot;
  if (platform === 'darwin' && config.broker.controlRoot && resolve(config.broker.controlRoot) !== resolve(liveExternal.path)) {
    const container = join(config.broker.controlRoot, '.rh');
    await mkdir(container, { recursive: true, mode: 0o700 });
    controlProbeRoot = await mkdtemp(join(container, 'p-'));
  }
  const endpoint = platform === 'win32' ? healthProbeEndpoint(platform, probeId) : join(controlProbeRoot, 'broker.sock');
  const { activationFencePath, ...probeBroker } = config.broker;
  // Isolated health gets its own absent fence under probeRoot. This satisfies
  // schema-v2 containment without inheriting production PENDING state.
  const probeSccache = platform === 'win32' && config.sccache?.mode === 'managed'
    ? { ...config.sccache, gatewayPort: await availableLoopbackPort() }
    : config.sccache;
  const probeConfig = { ...config, ...(probeSccache ? { sccache: probeSccache } : {}), ...(platform === 'win32' ? { windowsPipe: endpoint } : { unixSocket: endpoint }), broker: { ...probeBroker, ...(probeBroker.controlRoot ? { controlRoot: controlProbeRoot } : {}), activationFencePath: join(controlProbeRoot, 'activation-fence.json'), externalRoot: { ...liveExternal, path: probeRoot }, stateFile: join(controlProbeRoot, 'state.json'), receiptsDir: join(probeRoot, 'receipts') } };
  const configPath = join(controlProbeRoot !== probeRoot ? controlProbeRoot : tmpdir(), `rk-${randomUUID()}.health-config.json`); await writeFile(configPath, `${JSON.stringify(probeConfig)}\n`, { mode: 0o600 });
  const spawnOptions = { stdio: ['ignore', 'ignore', 'pipe'], ...(platform === 'win32' ? { platform, loadJobAddon: () => loadRuntimeWindowsJobAddon(runtime) } : {}) };
  const child = spawnManagedProcess(nodePath, [join(runtime, 'build-control-broker', 'src', 'broker.mjs'), configPath], spawnOptions, spawn); let stderr = '';
  child.stderr.on('data', (data) => { stderr += data; });
  await startManagedProcess(child, { timeoutMs });
  try {
    await waitForSocket(endpoint, child, timeoutMs, () => stderr);
    if (healthWorkspace) {
      const manifest = JSON.parse(stripBom(await readFile(join(runtime, 'runtime-manifest.json'), 'utf8')));
      await waitForBrokerDataReady({ endpoint, runtime: normalizeRuntimeIdentity(manifest), timeoutMs });
      await functionalProbe(endpoint, healthWorkspace, timeoutMs, normalizeRuntimeIdentity(manifest), Boolean(manifest.macosResourceAddon || manifest.windowsJobAddon));
    }
  } finally {
    if (child.exitCode === null) await terminateProcessTree(child, { graceMs: Math.min(1_000, timeoutMs) });
    await rm(endpoint, { force: true }); await rm(configPath, { force: true }); await removeHealthProbeRoot(probeRoot, { platform });
    if (controlProbeRoot !== probeRoot) await removeHealthProbeRoot(controlProbeRoot, { platform });
  }
}

async function waitForSocket(endpoint, child, timeoutMs, errorOutput) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (child.exitCode !== null) throw new Error(`runtime health probe exited ${child.exitCode}: ${errorOutput().trim()}`);
    if (await socketOpen(endpoint)) return;
    if (Date.now() >= deadline) throw new Error(`runtime health probe exceeded ${timeoutMs}ms: ${errorOutput().trim()}`);
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}
const socketOpen = (endpoint) => new Promise((resolve) => { const socket = net.createConnection(endpoint); socket.once('connect', () => { socket.destroy(); resolve(true); }); socket.once('error', () => resolve(false)); });
export function functionalFailureDetail(event) {
  return event?.error ?? event?.receipt?.error ?? event?.receipt?.reason ?? event?.status ?? 'unknown';
}
export function functionalResourceFailure(event) {
  const snapshot = safeResourceSnapshot(event?.receipt?.resources?.processTree);
  return snapshot.status === 'OK' ? null : snapshot.reason ?? snapshot.status;
}
const functionalProbe = (endpoint, workspace, timeoutMs, runtime, requireResources = false) => new Promise((resolve, reject) => { const socket = net.createConnection(endpoint); let buffer = ''; let received = false; const timer = setTimeout(() => { socket.destroy(); reject(new Error('broker functional health timed out')); }, timeoutMs); socket.once('error', (error) => { clearTimeout(timer); reject(error); }); socket.on('data', (chunk) => { buffer += chunk; for (;;) { const line = buffer.indexOf('\n'); if (line < 0) return; const event = JSON.parse(buffer.slice(0, line)); buffer = buffer.slice(line + 1); if (event.type !== 'RESULT') continue; received = true; const resourceFailure = requireResources ? functionalResourceFailure(event) : null; if (event.exitCode !== 0 || resourceFailure) { clearTimeout(timer); socket.destroy(); reject(new Error(`broker functional health failed: ${resourceFailure ?? functionalFailureDetail(event)}`)); } } }); socket.once('close', () => { clearTimeout(timer); received ? resolve() : reject(new Error('broker functional health closed without RESULT')); }); socket.once('connect', () => socket.write(`${JSON.stringify({ type: 'MANAGED_AGENT', protocol: BUILD_CONTROL_PROTOCOL, runtime, shim: 'cargo', request: { frontend: 'cargo', argv: ['--version'], workspace, environment: {} } })}\n`)); });


// Candidate sockets now bind before data initialization. Prove readiness before
// functional Cargo health; never treat an early HANDSHAKE as job admission.
export async function waitForBrokerDataReady({ endpoint, runtime, timeoutMs, query = queryBrokerHealth, now = Date.now, sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)) }) {
  const deadline = now() + timeoutMs;
  for (;;) {
    const remaining = deadline - now();
    if (remaining <= 0) throw new Error('broker data readiness timed out');
    const status = await query({ endpoint, runtime, timeoutMs: remaining });
    if (status.type === 'STATUS' && status.state === 'READY') return status;
    if (status.code !== 'RK_DATA_ROOT_INITIALIZING') throw Object.assign(new Error(`broker data readiness failed: ${status.code ?? status.state ?? status.error}`), { code: status.code });
    await sleep(Math.min(25, Math.max(0, deadline - now())));
  }
}

function queryBrokerHealth({ endpoint, runtime, timeoutMs }) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(endpoint); let buffer = ''; let identity = false;
    const timer = setTimeout(() => { socket.destroy(); reject(new Error('broker data readiness timed out')); }, timeoutMs);
    const finish = (error, result) => { clearTimeout(timer); socket.destroy(); error ? reject(error) : resolve(result); };
    socket.once('error', (error) => finish(error));
    socket.once('end', () => finish(new Error('broker readiness closed without STATUS')));
    socket.once('connect', () => socket.write(`${JSON.stringify({ type: 'STATUS', id: 'runtime-health-probe', protocol: BUILD_CONTROL_PROTOCOL, runtime })}\n`));
    socket.on('data', (chunk) => {
      buffer += chunk;
      if (buffer.length > 1024 * 1024) return finish(new Error('broker readiness response exceeded limit'));
      for (;;) {
        const newline = buffer.indexOf('\n'); if (newline < 0) return;
        let event; try { event = JSON.parse(buffer.slice(0, newline)); } catch (error) { return finish(error); }
        buffer = buffer.slice(newline + 1);
        if (event.type === 'HANDSHAKE') {
          identity = ['version', 'clientDigest', 'brokerDigest'].every((key) => event.runtime?.[key] === runtime[key]);
          if (!identity) return finish(new Error('broker readiness runtime digest mismatch'));
        } else if (!identity) return finish(new Error('broker readiness omitted HANDSHAKE'));
        else if (event.type === 'STATUS' || event.type === 'RESULT') return finish(null, event);
      }
    });
  });
}
