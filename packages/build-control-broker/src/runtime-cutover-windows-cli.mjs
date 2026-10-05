#!/usr/bin/env node
import { execFile as childExecFile } from 'node:child_process';
import net from 'node:net';
import { promisify } from 'node:util';
import { realpath, stat, writeFile } from 'node:fs/promises';
import { isAbsolute, resolve } from 'node:path';
import { loadBrokerConfig } from './config.mjs';
import { installManagedRuntimeWindows, quotePowerShell, renderBrokerLauncherVbs, renderTaskDefinition, withTaskDefinitionFile } from './runtime-cutover-windows.mjs';
import { absoluteHostPath as absolute, assertDirectory, assertFile, beginCutoverDrain, contained, endCutoverDrain, proveBrokerRequestsDrained, proveThinRequestsDrained } from './cutover-shared.mjs';
// A `file://${process.argv[1]}` comparison never matches on Windows, where
// argv[1] is a drive path: the CLI exited 0 having silently done nothing.
import { assertElevatedWindows, isDirectMain } from './bootstrap-windows-cli.mjs';

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

export async function stopManagedTask({ taskName, endpoint, configPath = null, brokerScript = null, execFile = execDefault, netModule = net, pipeReleaseTimeoutMs = 20_000, pipePollIntervalMs = 250, orphanTakeoverMs = 5_000, sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)) }) {
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
  const reapOwnedOrphan = async () => {
    if (typeof configPath !== 'string' || !configPath || typeof brokerScript !== 'string' || !brokerScript) return [];
    const { stdout } = await execFile('powershell', ['-NoProfile', '-NonInteractive', '-Command',
      `@(Get-CimInstance Win32_Process | Where-Object { $_.Name -ieq 'node.exe' -and $_.CommandLine -like '*${quotePowerShell(configPath)}*' -and $_.CommandLine -like '*${quotePowerShell(brokerScript)}*' } | Select-Object -ExpandProperty ProcessId) | ConvertTo-Json -Compress`]);
    let parsed;
    try { parsed = JSON.parse(String(stdout).trim() || '[]'); } catch { return []; }
    const pids = (Array.isArray(parsed) ? parsed : [parsed]).filter((pid) => Number.isSafeInteger(pid) && pid > 0);
    for (const pid of pids) await execFile('taskkill', ['/PID', String(pid), '/T', '/F']);
    return pids;
  };
  const startedAt = Date.now(); const deadline = startedAt + pipeReleaseTimeoutMs; let reEnded = false; let forcedPids = [];
  for (;;) {
    if (!(await reachable())) return Object.freeze({ stopped: true, target: taskName, listener: false, ...(forcedPids.length ? { forcedPids: Object.freeze(forcedPids) } : {}) });
    if (Date.now() >= deadline) throw new Error('managed broker listener remains reachable after stop');
    if (!reEnded && Date.now() > deadline - Math.floor((pipeReleaseTimeoutMs * 2) / 3)) { await end(); reEnded = true; }
    if (forcedPids.length === 0 && Date.now() - startedAt >= Math.min(orphanTakeoverMs, Math.floor(pipeReleaseTimeoutMs / 2))) forcedPids = await reapOwnedOrphan();
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
  const { platform = process.platform, loadConfig = loadBrokerConfig, install = installManagedRuntimeWindows, canonicalize = realpath, statFile = stat, stopManaged = stopManagedTask, proveThinDrain = proveThinRequestsDrained, beginDrain = beginCutoverDrain, endDrain = endCutoverDrain, proveBrokerDrain = proveBrokerRequestsDrained, execFile = execDefault } = dependencies;
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
  // Keep stable invocation path (for example NVM's nodejs junction) in task
  // definition. Runtime PID proof accepts either this literal path or its
  // resolved physical target.
  const nodePath = process.execPath;
  const runtimeScript = resolve(externalRoot, 'runtime-current', 'build-control-broker', 'src', 'broker.mjs');
  const drainFence = await beginDrain(externalRoot);
  try {
    const brokerDrain = await proveBrokerDrain(externalRoot, options.endpoint);
    const thinDrain = await proveThinDrain(externalRoot);
    const stopProof = await stopManaged({ taskName: options.taskName, endpoint: options.endpoint, configPath, brokerScript: runtimeScript, execFile });
    // Persist the hidden-window launcher the task action references; every
    // start (including logon-trigger starts) goes through it so no console
    // window ever appears on the operator's desktop.
    const launcherScript = resolve(externalRoot, 'broker-launch.vbs');
    await writeFile(launcherScript, renderBrokerLauncherVbs({ nodePath, brokerScript: runtimeScript, configPath }), 'utf8');
    const definition = renderTaskDefinition({ taskName: options.taskName, nodePath, brokerScript: runtimeScript, configPath, launcherScript });
    const result = await withTaskDefinitionFile(definition, (taskXmlPath) => install({
      externalRoot, sourcePackagesRoot, sourceRevision: options.sourceRevision, sourceTreeDigest: options.sourceTreeDigest, config, version: options.version, taskName: options.taskName, endpoint: options.endpoint,
      configPath, nodePath, healthWorkspace, healthTimeoutMs: options.timeoutMs, timeoutMs: options.timeoutMs, taskXmlPath,
      drainProbe: async () => proveThinDrain(externalRoot),
    }));
    return Object.freeze({ configPath, externalRoot, sourcePackagesRoot, brokerDrain, thinDrain, stopProof, result });
  } finally { await endDrain(drainFence); }
}

async function main() {
  await assertElevatedWindows();
  const receipt = await runWindowsCutover(process.argv.slice(2));
  process.stdout.write(`${JSON.stringify(receipt)}\n`);
}

if (isDirectMain({ moduleUrl: import.meta.url })) main().catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; });
