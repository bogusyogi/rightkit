import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { chmod, cp, lstat, mkdir, open, readFile, readdir, readlink, realpath, rename, rm, symlink, writeFile } from 'node:fs/promises';
import { execFile as nodeExecFile, spawn } from 'node:child_process';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { promisify } from 'node:util';
import { BUILD_CONTROL_PROTOCOL, RUNTIME_MANIFEST_SCHEMA, normalizeRuntimeIdentity } from '@rightkit/build-control-core';
import { spawnManagedProcess, startManagedProcess, terminateProcessTree } from './process-tree.mjs';

// Windows editors (PowerShell Set-Content/Out-File, Notepad) write a leading
// UTF-8 BOM by default; JSON.parse does not strip it & fails opaquely.
function stripBom(text) { return typeof text === 'string' && text.charCodeAt(0) === 0xfeff ? text.slice(1) : text; }

const packageNames = ['build-control-broker', 'build-control-core', 'build-control-clients'];
export const WINDOWS_JOB_ADDON_RELATIVE_PATH = 'build-control-broker/native/windows-job/build/Release/rightkit_windows_job.node';
const execFile = promisify(nodeExecFile);
export const RUNTIME_ACTIVATION_FENCE_SCHEMA = 'rightkit.build-control.runtime-activation.v1';
export const normalizeGitScopedPath = (path) => path.replaceAll('\\', '/');

/** Materialize an immutable runtime bundle. This function never changes runtime-current or a service. */
export async function stageManagedRuntime({ externalRoot, sourcePackagesRoot, version, sourceRevision, sourceTreeDigest, platform = process.platform, windowsJobAddon }) {
  if (!externalRoot || !sourcePackagesRoot || !version) throw new TypeError('externalRoot, sourcePackagesRoot & version are required');
  if (!/^[A-Za-z0-9._-]+$/.test(version)) throw new TypeError('runtime version must be a safe immutable identifier');
  const root = resolve(externalRoot); const source = resolve(sourcePackagesRoot); const provenance = await verifySourceProvenance(source, { sourceRevision, sourceTreeDigest }); const staging = join(root, `.runtime-${randomUUID()}.staging`);
  await mkdir(root, { recursive: true }); await mkdir(staging, { recursive: false });
  try {
    for (const name of packageNames) { const target = join(staging, name); await mkdir(target, { recursive: true }); await Promise.all([cp(join(source, name, 'src'), join(target, 'src'), { recursive: true }), cp(join(source, name, 'package.json'), join(target, 'package.json'))]); }
    const addon = platform === 'win32' ? await stageWindowsJobAddon(staging, source, windowsJobAddon) : undefined;
    await linkRuntimePackages(staging);
    const contentDigest = await digestTree(staging, { excludeManifest: true });
    const manifest = await writeRuntimeManifest(staging, version, provenance, addon);
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
    await makeImmutable(staging);
    await rename(staging, immutableRuntime);
    await syncDirectory(root);
    return Object.freeze({ runtime: immutableRuntime, version, contentDigest, manifest });
  } catch (error) { await rm(staging, { recursive: true, force: true }); throw error; }
}

/** Compatibility activation wrapper. Bundle construction itself remains isolated in stageManagedRuntime. */
export async function installManagedRuntime({ externalRoot, sourcePackagesRoot, config, version, sourceRevision, sourceTreeDigest, nodePath = process.execPath, healthTimeoutMs = 8_000, healthWorkspace, postSwitchHealth, rollbackSwitch, restartService, probeService, rollbackService, processProbe, stage = stageManagedRuntime, health = healthProbe, platform = process.platform, windowsJobAddon }) {
  if (!externalRoot || !sourcePackagesRoot || !config || !version) throw new TypeError('externalRoot, sourcePackagesRoot, config & version are required');
  if (!Number.isSafeInteger(healthTimeoutMs) || healthTimeoutMs < 100) throw new TypeError('healthTimeoutMs must be at least 100');
  if (typeof healthWorkspace !== 'string' || !isAbsolute(healthWorkspace)) throw new TypeError('absolute healthWorkspace is required for functional runtime activation proof');
  const healthManifest = await lstat(join(healthWorkspace, 'Cargo.toml')).catch(() => null);
  if (!healthManifest?.isFile()) throw new TypeError('healthWorkspace must contain a regular Cargo.toml');
  if (typeof restartService !== 'function' || typeof probeService !== 'function' || typeof rollbackService !== 'function' || typeof processProbe !== 'function') throw new TypeError('restartService, probeService, rollbackService & processProbe are required for activation');
  const root = resolve(externalRoot); const staged = await stage({ externalRoot: root, sourcePackagesRoot, version, sourceRevision, sourceTreeDigest, platform, windowsJobAddon }); const { runtime, manifest, contentDigest } = staged;
  await verifyRuntimeBundle(runtime, manifest, contentDigest);
  const activationFencePath = activationFencePathFor(config, root);
  const activation = await prepareRuntimeActivationFence({ path: activationFencePath, manifest });
  let selected = false; let committed = false; let prior = null; let service;
  try {
    await health({ runtime, config, nodePath, timeoutMs: healthTimeoutMs, healthWorkspace });
    const current = join(root, 'runtime-current'); prior = await readlink(current).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    const next = join(root, `.runtime-current-${randomUUID()}`); await symlink(basename(runtime), next); await rename(next, current); await syncDirectory(root);
    selected = true;
    try {
      await verifyRuntimeBundle(runtime, manifest, contentDigest);
      service = await restartService({ runtime, current, prior, manifest, activation, timeoutMs: healthTimeoutMs });
      service = await probeService({ runtime, current, prior, manifest, activation, restart: service, timeoutMs: healthTimeoutMs });
      await assertServiceIdentity(service, runtime, manifest, processProbe);
      await verifyRuntimeBundle(runtime, manifest, contentDigest);
      await commitRuntimeActivationFence(activation);
      committed = true;
    } catch (error) {
      if (committed) throw error;
      const rollback = join(root, `.runtime-current-${randomUUID()}`); if (prior === null) await rm(current, { force: true }); else { await symlink(prior, rollback); await rename(rollback, current); } await syncDirectory(root);
      selected = false;
      await removeRuntimeActivationFence(activation);
      try { await rollbackService({ prior, current, failedRuntime: runtime, manifest, service, timeoutMs: healthTimeoutMs }); }
      catch (rollbackError) { throw new Error(`runtime activation rollback failed: ${rollbackError.message}; activation failure: ${error.message}`); }
      throw new Error(`runtime activation rolled back: ${error.message}`);
    }
    return Object.freeze({ runtime, current: join(root, 'runtime-current'), version: manifest.version, contentDigest, manifest, activation: Object.freeze({ path: activation.path, tokenDigest: activation.tokenDigest, state: 'COMMITTED' }) });
  } catch (error) {
    if (!committed) await removeRuntimeActivationFence(activation).catch(() => {});
    if (selected && !committed) throw new Error(`runtime activation selected candidate without rollback: ${error.message}`);
    throw error;
  }
}

export async function writeRuntimeManifest(runtime, version, provenance, windowsJobAddon) {
  const coreDigest = await digestTree(join(runtime, 'build-control-core')); const clientPackageDigest = await digestTree(join(runtime, 'build-control-clients')); const brokerPackageDigest = await digestTree(join(runtime, 'build-control-broker'));
  const identity = normalizeRuntimeIdentity({ schema: RUNTIME_MANIFEST_SCHEMA, version, clientDigest: hash(`${coreDigest}:${clientPackageDigest}`), brokerDigest: hash(`${coreDigest}:${brokerPackageDigest}`) });
  const manifest = normalizeRuntimeManifest({ ...identity, ...provenance, ...(windowsJobAddon ? { windowsJobAddon } : {}) });
  const path = join(runtime, 'runtime-manifest.json'); const handle = await open(path, 'wx', 0o444);
  try { await handle.writeFile(`${JSON.stringify(manifest)}\n`, 'utf8'); await handle.sync(); await handle.chmod(0o444); }
  finally { await handle.close(); }
  await syncDirectory(runtime); return manifest;
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
  await mkdir(dirname(target), { recursive: true }); await writeFile(target, bytes, { mode: 0o444 });
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

function normalizeRuntimeManifest(manifest) {
  const identity = normalizeRuntimeIdentity(manifest);
  if (!/^[0-9a-f]{40,64}$/.test(manifest?.sourceRevision ?? '') || !/^[0-9a-f]{40,64}$/.test(manifest?.sourceTreeDigest ?? '')) throw new TypeError('runtime manifest requires immutable source revision & tree digest');
  const addon = manifest?.windowsJobAddon;
  if (addon !== undefined && (addon.path !== WINDOWS_JOB_ADDON_RELATIVE_PATH || !/^[0-9a-f]{64}$/i.test(addon.sha256 ?? '') || !Number.isSafeInteger(addon.sizeBytes) || addon.sizeBytes < 1)) throw new TypeError('runtime manifest has malformed Windows Job Object addon descriptor');
  return Object.freeze({ ...identity, sourceRevision: manifest.sourceRevision, sourceTreeDigest: manifest.sourceTreeDigest, ...(addon ? { windowsJobAddon: Object.freeze({ path: addon.path, sha256: addon.sha256.toLowerCase(), sizeBytes: addon.sizeBytes }) } : {}) });
}

function sameRuntimeIdentity(left, right) {
  const expected = normalizeRuntimeIdentity(right); const observed = normalizeRuntimeIdentity(left);
  return observed.schema === expected.schema && observed.version === expected.version && observed.clientDigest === expected.clientDigest && observed.brokerDigest === expected.brokerDigest;
}

function sameRuntimeManifest(left, right) {
  const expected = normalizeRuntimeManifest(right); const observed = normalizeRuntimeManifest(left);
  return sameRuntimeIdentity(observed, expected) && observed.sourceRevision === expected.sourceRevision && observed.sourceTreeDigest === expected.sourceTreeDigest && JSON.stringify(observed.windowsJobAddon ?? null) === JSON.stringify(expected.windowsJobAddon ?? null);
}

async function gitAt(cwd, args) {
  try { return (await execFile('git', ['-C', cwd, ...args], { encoding: 'utf8' })).stdout.trim(); }
  catch (error) { throw new Error(`runtime source provenance unavailable: ${error.stderr?.trim() || error.message}`); }
}

async function verifySourceProvenance(sourcePackagesRoot, { sourceRevision, sourceTreeDigest } = {}) {
  const repository = resolve(await gitAt(sourcePackagesRoot, ['rev-parse', '--show-toplevel'])); const packages = resolve(sourcePackagesRoot); const filesystemScopedPath = relative(repository, packages);
  if (!filesystemScopedPath || filesystemScopedPath === '.' || filesystemScopedPath.startsWith('..')) throw new Error('runtime source packages root must be a committed repository subdirectory');
  const scopedPath = normalizeGitScopedPath(filesystemScopedPath);
  const status = await gitAt(repository, ['status', '--porcelain=v1', '--untracked-files=all', '--', scopedPath]);
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
  if (relative(root, resolved).startsWith('..')) throw new TypeError('broker.activationFencePath must remain under externalRoot');
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

async function makeImmutable(root) {
  const walk = async (directory) => {
    for (const name of await readdir(directory)) {
      const path = join(directory, name); const metadata = await lstat(path);
      if (metadata.isSymbolicLink()) continue;
      if (metadata.isDirectory()) await walk(path); else { const handle = await open(path, 'r'); try { await handle.sync(); } finally { await handle.close(); } await chmod(path, 0o444); }
    }
    // Keep directory metadata traversable/reclaimable while making every
    // materialized file immutable; content cannot be changed in place.
    await chmod(directory, 0o755); await syncDirectory(directory);
  };
  await walk(root);
}

async function syncDirectory(path) { const handle = await open(path, 'r'); try { await handle.sync(); } finally { await handle.close(); } }

async function linkRuntimePackages(runtime) {
  for (const consumer of ['build-control-broker', 'build-control-clients']) { const directory = join(runtime, consumer, 'node_modules', '@rightkit'); await mkdir(directory, { recursive: true }); await symlink('../../../build-control-core', join(directory, 'build-control-core')); }
}

async function healthProbe({ runtime, config, nodePath, timeoutMs, healthWorkspace }) {
  const liveExternal = config.broker?.externalRoot; if (!liveExternal?.path) throw new TypeError('broker external root is required for isolated health probe');
  const probeRoot = join(liveExternal.path, '.runtime-health', randomUUID()); await mkdir(probeRoot, { recursive: true, mode: 0o700 }); await chmod(probeRoot, 0o700); const endpoint = join(probeRoot, 'broker.sock');
  const { activationFencePath, ...probeBroker } = config.broker;
  // Isolated health gets its own absent fence under probeRoot. This satisfies
  // schema-v2 containment without inheriting production PENDING state.
  const probeConfig = { ...config, unixSocket: endpoint, broker: { ...probeBroker, activationFencePath: join(probeRoot, 'activation-fence.json'), externalRoot: { ...liveExternal, path: probeRoot }, stateFile: join(probeRoot, 'state.json'), databasePath: join(probeRoot, 'state.sqlite'), eventLogPath: join(probeRoot, 'events.ndjson'), receiptsDir: join(probeRoot, 'receipts') } };
  const configPath = join(tmpdir(), `rk-${randomUUID()}.health-config.json`); await writeFile(configPath, `${JSON.stringify(probeConfig)}\n`, { mode: 0o600 });
  const child = spawnManagedProcess(nodePath, [join(runtime, 'build-control-broker', 'src', 'broker.mjs'), configPath], { stdio: ['ignore', 'ignore', 'pipe'] }, spawn); let stderr = '';
  child.stderr.on('data', (data) => { stderr += data; });
  await startManagedProcess(child, { timeoutMs });
  try {
    await waitForSocket(endpoint, child, timeoutMs, () => stderr); if (healthWorkspace) await functionalProbe(endpoint, healthWorkspace, timeoutMs, normalizeRuntimeIdentity(JSON.parse(stripBom(await readFile(join(runtime, 'runtime-manifest.json'), 'utf8')))));
  } finally {
    if (child.exitCode === null) await terminateProcessTree(child, { graceMs: Math.min(1_000, timeoutMs) });
    await rm(endpoint, { force: true }); await rm(configPath, { force: true }); await rm(probeRoot, { recursive: true, force: true });
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
const functionalProbe = (endpoint, workspace, timeoutMs, runtime) => new Promise((resolve, reject) => { const socket = net.createConnection(endpoint); let buffer = ''; let received = false; const timer = setTimeout(() => { socket.destroy(); reject(new Error('broker functional health timed out')); }, timeoutMs); socket.once('error', (error) => { clearTimeout(timer); reject(error); }); socket.on('data', (chunk) => { buffer += chunk; for (;;) { const line = buffer.indexOf('\n'); if (line < 0) return; const event = JSON.parse(buffer.slice(0, line)); buffer = buffer.slice(line + 1); if (event.type !== 'RESULT') continue; received = true; if (event.exitCode !== 0) { clearTimeout(timer); socket.destroy(); reject(new Error(`broker functional health failed: ${event.error ?? event.status}`)); } } }); socket.once('close', () => { clearTimeout(timer); received ? resolve() : reject(new Error('broker functional health closed without RESULT')); }); socket.once('connect', () => socket.write(`${JSON.stringify({ type: 'MANAGED_AGENT', protocol: BUILD_CONTROL_PROTOCOL, runtime, shim: 'cargo', request: { frontend: 'cargo', argv: ['--version'], workspace, environment: {} } })}\n`)); });
