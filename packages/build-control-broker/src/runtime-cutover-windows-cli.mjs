#!/usr/bin/env node
import { execFile as childExecFile } from 'node:child_process';
import net from 'node:net';
import { promisify } from 'node:util';
import { realpath, stat } from 'node:fs/promises';
import { isAbsolute, resolve } from 'node:path';
import { loadBrokerConfig } from './config.mjs';
import { installManagedRuntimeWindows, renderTaskDefinition, withTaskDefinitionFile } from './runtime-cutover-windows.mjs';
import { absoluteHostPath as absolute, assertDirectory, assertFile, contained, proveThinRequestsDrained } from './cutover-shared.mjs';

const execDefault = promisify(childExecFile);

const required = new Set(['config', 'external-root', 'source-packages', 'source-revision', 'source-tree-digest', 'version', 'task-name', 'endpoint', 'health-workspace']);

// Re-exported: these now live in cutover-shared.mjs, shared with the macOS
// cutover CLI, but kept importable from here for existing callers & tests.
export { proveThinRequestsDrained };

export function parseWindowsCutoverArgs(argv) {
  const values = {};
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index]; const value = argv[index + 1];
    if (typeof flag !== 'string' || !flag.startsWith('--') || value === undefined) throw new TypeError('cutover arguments must be --name value pairs');
    const name = flag.slice(2);
    if (![...required, 'timeout-ms'].includes(name) || values[name] !== undefined) throw new TypeError(`unknown or duplicate cutover argument: ${flag}`);
    values[name] = value;
  }
  for (const name of required) if (values[name] === undefined) throw new TypeError(`--${name} is required`);
  const timeoutMs = values['timeout-ms'] === undefined ? 8_000 : Number(values['timeout-ms']);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 100) throw new TypeError('--timeout-ms must be an integer of at least 100');
  if (!/^[A-Za-z0-9._-]+$/.test(values.version)) throw new TypeError('--version must be an immutable safe identifier');
  if (!/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(values['task-name'])) throw new TypeError('--task-name is invalid');
  for (const name of ['source-revision', 'source-tree-digest']) if (!/^[a-f0-9]{40,64}$/i.test(values[name])) throw new TypeError(`--${name} must be a Git object digest`);
  return Object.freeze({
    configPath: absolute(values.config, 'config'), externalRoot: absolute(values['external-root'], 'external-root'), sourcePackagesRoot: absolute(values['source-packages'], 'source-packages'),
    version: values.version, sourceRevision: values['source-revision'], sourceTreeDigest: values['source-tree-digest'], taskName: values['task-name'], endpoint: values.endpoint,
    healthWorkspace: absolute(values['health-workspace'], 'health-workspace'), timeoutMs,
  });
}

export async function stopManagedTask({ taskName, endpoint, execFile = execDefault, netModule = net, pipeReleaseTimeoutMs = 20_000, pipePollIntervalMs = 250, sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)) }) {
  const end = () => execFile('schtasks', ['/End', '/TN', taskName]).catch(() => {});
  await end();
  await execFile('schtasks', ['/Change', '/TN', taskName, '/DISABLE']).catch(() => {});
  const reachable = () => new Promise((resolveListener) => {
    const socket = netModule.createConnection(endpoint); const done = (open) => { socket.destroy(); resolveListener(open); };
    socket.once('connect', () => done(true)); socket.once('error', () => done(false));
  });
  // `schtasks /End` terminates the task asynchronously: the broker process can
  // still hold the named-pipe handle for a moment after the call returns.
  // Probing once immediately therefore reports a false "still reachable" and
  // fails the whole cutover, even though the pipe frees a beat later — the
  // exact race that made re-running bootstrap "work" every time. Poll for the
  // handle to release instead, and re-issue /End once mid-window in case the
  // first call did not take.
  const deadline = Date.now() + pipeReleaseTimeoutMs; let reEnded = false;
  for (;;) {
    if (!(await reachable())) return Object.freeze({ stopped: true, target: taskName, listener: false });
    if (Date.now() >= deadline) throw new Error('managed broker listener remains reachable after stop');
    if (!reEnded && Date.now() > deadline - Math.floor((pipeReleaseTimeoutMs * 2) / 3)) { await end(); reEnded = true; }
    await sleep(pipePollIntervalMs);
  }
}

/**
 * Single supported Windows activation boundary. Mirrors
 * runMacOSCutover's verification order exactly: every immutable selection is
 * checked, then the managed-request drain is proven, then (and only then) the
 * legacy service is stopped.
 */
export async function runWindowsCutover(input, dependencies = {}) {
  const { platform = process.platform, loadConfig = loadBrokerConfig, install = installManagedRuntimeWindows, canonicalize = realpath, statFile = stat, stopManaged = stopManagedTask, proveThinDrain = proveThinRequestsDrained, execFile = execDefault } = dependencies;
  if (platform !== 'win32') throw new Error('windows cutover may only run on win32');
  const options = typeof input?.configPath === 'string' ? input : parseWindowsCutoverArgs(input);
  const [configPath, externalRoot, sourcePackagesRoot, healthWorkspace] = await Promise.all([
    canonicalize(options.configPath), canonicalize(options.externalRoot), canonicalize(options.sourcePackagesRoot), canonicalize(options.healthWorkspace),
  ]);
  await assertFile(configPath, 'config', statFile); await assertDirectory(healthWorkspace, 'health workspace', statFile); await assertFile(resolve(healthWorkspace, 'Cargo.toml'), 'health workspace Cargo.toml', statFile);
  const config = await loadConfig(configPath);
  if (resolve(config.broker.externalRoot.path) !== externalRoot) throw new Error('config broker.externalRoot.path differs from --external-root');
  if ((config.windowsPipe ?? '') !== options.endpoint) throw new Error('config windowsPipe differs from --endpoint');
  if (typeof config.broker.activationFencePath !== 'string' || !isAbsolute(config.broker.activationFencePath) || !contained(resolve(config.broker.activationFencePath), externalRoot)) throw new Error('config broker.activationFencePath must be an absolute path contained by --external-root');
  const nodePath = await canonicalize(process.execPath);
  const runtimeScript = resolve(externalRoot, 'runtime-current', 'build-control-broker', 'src', 'broker.mjs');
  const thinDrain = await proveThinDrain(externalRoot);
  const stopProof = await stopManaged({ taskName: options.taskName, endpoint: options.endpoint, execFile });
  const definition = renderTaskDefinition({ taskName: options.taskName, nodePath, brokerScript: runtimeScript, configPath });
  const result = await withTaskDefinitionFile(definition, (taskXmlPath) => install({
    externalRoot, sourcePackagesRoot, sourceRevision: options.sourceRevision, sourceTreeDigest: options.sourceTreeDigest, config, version: options.version, taskName: options.taskName, endpoint: options.endpoint,
    configPath, nodePath, healthWorkspace, healthTimeoutMs: options.timeoutMs, timeoutMs: options.timeoutMs, taskXmlPath,
    drainProbe: async () => proveThinDrain(externalRoot),
  }));
  return Object.freeze({ configPath, externalRoot, sourcePackagesRoot, thinDrain, stopProof, result });
}

async function main() {
  const receipt = await runWindowsCutover(process.argv.slice(2));
  process.stdout.write(`${JSON.stringify(receipt)}\n`);
}

if (import.meta.url === `file://${process.argv[1]}`) main().catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; });
