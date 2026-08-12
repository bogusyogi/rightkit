import { randomUUID } from 'node:crypto';
import { cp, lstat, mkdir, readlink, rename, rm, symlink, writeFile } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';

const packageNames = ['build-control-broker', 'build-control-core', 'build-control-clients'];

/** Install a self-contained broker runtime, prove it can bind, then atomically select it. */
export async function installManagedRuntime({ externalRoot, sourcePackagesRoot, config, version, nodePath = process.execPath, healthTimeoutMs = 8_000, healthWorkspace, postSwitchHealth }) {
  if (!externalRoot || !sourcePackagesRoot || !config || !version) throw new TypeError('externalRoot, sourcePackagesRoot, config & version are required');
  if (!Number.isSafeInteger(healthTimeoutMs) || healthTimeoutMs < 100) throw new TypeError('healthTimeoutMs must be at least 100');
  const root = resolve(externalRoot); const source = resolve(sourcePackagesRoot); const runtime = join(root, `runtime-${version}`); const staging = `${runtime}.${randomUUID()}.staging`;
  if (await lstat(runtime).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error))) throw new Error(`runtime version already exists: ${runtime}`);
  await rm(staging, { recursive: true, force: true }); await mkdir(staging, { recursive: true });
  try {
    for (const name of packageNames) { const target = join(staging, name); await mkdir(target, { recursive: true }); await Promise.all([cp(join(source, name, 'src'), join(target, 'src'), { recursive: true }), cp(join(source, name, 'package.json'), join(target, 'package.json'))]); }
    await linkRuntimePackages(staging);
    await healthProbe({ runtime: staging, config, nodePath, timeoutMs: healthTimeoutMs, healthWorkspace }); await rename(staging, runtime);
    const current = join(root, 'runtime-current'); const prior = await readlink(current).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    const next = join(root, `.runtime-current-${randomUUID()}`); await symlink(`runtime-${version}`, next); await rename(next, current);
    try { if (postSwitchHealth) await postSwitchHealth({ runtime, current, timeoutMs: healthTimeoutMs }); else await healthProbe({ runtime: current, config, nodePath, timeoutMs: healthTimeoutMs, healthWorkspace }); }
    catch (error) { const rollback = join(root, `.runtime-current-${randomUUID()}`); if (prior === null) await rm(current, { force: true }); else { await symlink(prior, rollback); await rename(rollback, current); } throw new Error(`runtime activation rolled back: ${error.message}`); }
    return Object.freeze({ runtime, current: join(root, 'runtime-current'), version });
  } catch (error) { await rm(staging, { recursive: true, force: true }); throw error; }
}

async function linkRuntimePackages(runtime) {
  for (const consumer of ['build-control-broker', 'build-control-clients']) { const directory = join(runtime, consumer, 'node_modules', '@rightkit'); await mkdir(directory, { recursive: true }); await symlink('../../../build-control-core', join(directory, 'build-control-core')); }
}

async function healthProbe({ runtime, config, nodePath, timeoutMs, healthWorkspace }) {
  const endpoint = join(tmpdir(), `rk-${randomUUID().slice(0, 18)}.sock`);
  const probeConfig = { ...config, unixSocket: endpoint, broker: { ...config.broker } };
  const configPath = join(runtime, '.health-config.json'); await writeFile(configPath, `${JSON.stringify(probeConfig)}\n`, { mode: 0o600 });
  const child = spawn(nodePath, [join(runtime, 'build-control-broker', 'src', 'broker.mjs'), configPath], { stdio: ['ignore', 'ignore', 'pipe'] }); let stderr = '';
  child.stderr.on('data', (data) => { stderr += data; });
  try {
    await waitForSocket(endpoint, child, timeoutMs, () => stderr); if (healthWorkspace) await functionalProbe(endpoint, healthWorkspace, timeoutMs);
  } finally {
    if (child.exitCode === null) { const closed = new Promise((resolve) => child.once('close', resolve)); child.kill('SIGTERM'); await Promise.race([closed, new Promise((resolve) => setTimeout(resolve, 1_000))]); }
    await rm(endpoint, { force: true }); await rm(configPath, { force: true });
  }
}

async function waitForSocket(endpoint, child, timeoutMs, errorOutput) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (child.exitCode !== null) throw new Error(`runtime health probe exited ${child.exitCode}: ${errorOutput().trim()}`);
    if (await socketOpen(endpoint)) return;
    if (Date.now() >= deadline) throw new Error(`runtime health probe exceeded ${timeoutMs}ms`);
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}
const socketOpen = (endpoint) => new Promise((resolve) => { const socket = net.createConnection(endpoint); socket.once('connect', () => { socket.destroy(); resolve(true); }); socket.once('error', () => resolve(false)); });
const functionalProbe = (endpoint, workspace, timeoutMs) => new Promise((resolve, reject) => { const socket = net.createConnection(endpoint); let buffer = ''; let received = false; const timer = setTimeout(() => { socket.destroy(); reject(new Error('broker functional health timed out')); }, timeoutMs); socket.once('error', (error) => { clearTimeout(timer); reject(error); }); socket.on('data', (chunk) => { buffer += chunk; for (;;) { const line = buffer.indexOf('\n'); if (line < 0) return; const event = JSON.parse(buffer.slice(0, line)); buffer = buffer.slice(line + 1); if (event.type !== 'RESULT') continue; received = true; if (event.exitCode !== 0) { clearTimeout(timer); socket.destroy(); reject(new Error(`broker functional health failed: ${event.error ?? event.status}`)); } } }); socket.once('close', () => { clearTimeout(timer); received ? resolve() : reject(new Error('broker functional health closed without RESULT')); }); socket.once('connect', () => socket.write(`${JSON.stringify({ type: 'MANAGED_AGENT', protocol: 'rightkit.build-control.client.v1', shim: 'cargo', request: { frontend: 'cargo', argv: ['--version'], workspace, environment: {} } })}\n`)); });
