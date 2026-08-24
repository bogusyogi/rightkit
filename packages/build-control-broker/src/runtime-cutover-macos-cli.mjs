#!/usr/bin/env node
import { execFile as childExecFile } from 'node:child_process';
import net from 'node:net';
import { promisify } from 'node:util';
import { readFile, realpath, stat } from 'node:fs/promises';
import { isAbsolute, resolve } from 'node:path';
import { loadBrokerConfig } from './config.mjs';
import { installManagedRuntimeMacOS } from './runtime-cutover-macos.mjs';
import { absoluteHostPath as absolute, assertDirectory, assertFile, beginCutoverDrain, contained, endCutoverDrain, proveBrokerRequestsDrained, proveThinRequestsDrained } from './cutover-shared.mjs';

const execDefault = promisify(childExecFile);

const required = new Set(['config', 'external-root', 'source-packages', 'source-revision', 'source-tree-digest', 'version', 'label', 'uid', 'endpoint', 'plist', 'health-workspace']);

// Re-exported: these now live in cutover-shared.mjs, shared with the Windows
// cutover CLI, but kept importable from here for existing callers & tests.
export { proveThinRequestsDrained };

export function parseMacOSCutoverArgs(argv) {
  const values = {};
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index]; const value = argv[index + 1];
    if (typeof flag !== 'string' || !flag.startsWith('--') || value === undefined) throw new TypeError('cutover arguments must be --name value pairs');
    const name = flag.slice(2);
    if (![...required, 'timeout-ms'].includes(name) || values[name] !== undefined) throw new TypeError(`unknown or duplicate cutover argument: ${flag}`);
    values[name] = value;
  }
  for (const name of required) if (values[name] === undefined) throw new TypeError(`--${name} is required`);
  const uid = Number(values.uid);
  if (!Number.isSafeInteger(uid) || uid < 0) throw new TypeError('--uid must be a non-negative integer');
  const timeoutMs = values['timeout-ms'] === undefined ? 8_000 : Number(values['timeout-ms']);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 100) throw new TypeError('--timeout-ms must be an integer of at least 100');
  if (!/^[A-Za-z0-9._-]+$/.test(values.version)) throw new TypeError('--version must be an immutable safe identifier');
  if (!/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(values.label)) throw new TypeError('--label is invalid');
  for (const name of ['source-revision', 'source-tree-digest']) if (!/^[a-f0-9]{40,64}$/i.test(values[name])) throw new TypeError(`--${name} must be a Git object digest`);
  return Object.freeze({
    configPath: absolute(values.config, 'config'), externalRoot: absolute(values['external-root'], 'external-root'), sourcePackagesRoot: absolute(values['source-packages'], 'source-packages'),
    version: values.version, sourceRevision: values['source-revision'], sourceTreeDigest: values['source-tree-digest'], launchdLabel: values.label, uid, endpoint: absolute(values.endpoint, 'endpoint'), plistPath: absolute(values.plist, 'plist'),
    healthWorkspace: absolute(values['health-workspace'], 'health-workspace'), timeoutMs,
  });
}

function exactPlistLabel(contents, label) {
  const escaped = label.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp(`<key>\\s*Label\\s*</key>\\s*<string>\\s*${escaped}\\s*</string>`, 's').test(contents);
}
function plistProgramArguments(contents) {
  const block = contents.match(/<key>\s*ProgramArguments\s*<\/key>\s*<array>([\s\S]*?)<\/array>/)?.[1];
  if (!block) throw new Error('plist ProgramArguments are required');
  return [...block.matchAll(/<string>([\s\S]*?)<\/string>/g)].map((match) => match[1].replaceAll('&amp;', '&').replaceAll('&lt;', '<').replaceAll('&gt;', '>'));
}
export async function stopManagedLaunchd({ launchdLabel, uid, plistPath, endpoint, execFile = execDefault, netModule = net }) {
  const target = `gui/${uid}/${launchdLabel}`; const domain = `gui/${uid}`;
  await execFile('launchctl', ['disable', target]);
  await execFile('launchctl', ['bootout', domain, plistPath]).catch(() => {});
  const visible = await execFile('launchctl', ['print', target]).then(() => true).catch(() => false);
  if (visible) throw new Error('managed launchd service remains loaded after bootout');
  const listener = await new Promise((resolveListener) => {
    const socket = netModule.createConnection(endpoint); const done = (open) => { socket.destroy(); resolveListener(open); };
    socket.once('connect', () => done(true)); socket.once('error', () => done(false));
  });
  if (listener) throw new Error('managed broker listener remains reachable after stop');
  return Object.freeze({ stopped: true, target, listener: false });
}

/**
 * Single supported macOS activation boundary. It verifies every immutable
 * selection before allowing runtime.mjs to stage or switch runtime-current.
 */
export async function runMacOSCutover(input, dependencies = {}) {
  const { platform = process.platform, loadConfig = loadBrokerConfig, install = installManagedRuntimeMacOS, canonicalize = realpath, load = readFile, statFile = stat, stopManaged = stopManagedLaunchd, proveThinDrain = proveThinRequestsDrained, beginDrain = beginCutoverDrain, endDrain = endCutoverDrain, proveBrokerDrain = proveBrokerRequestsDrained } = dependencies;
  if (platform !== 'darwin') throw new Error('macOS cutover may only run on darwin');
  const options = typeof input?.configPath === 'string' ? input : parseMacOSCutoverArgs(input);
  const [configPath, externalRoot, sourcePackagesRoot, plistPath, healthWorkspace] = await Promise.all([
    canonicalize(options.configPath), canonicalize(options.externalRoot), canonicalize(options.sourcePackagesRoot), canonicalize(options.plistPath), canonicalize(options.healthWorkspace),
  ]);
  await assertFile(configPath, 'config', statFile); await assertFile(plistPath, 'plist', statFile); await assertDirectory(healthWorkspace, 'health workspace', statFile); await assertFile(resolve(healthWorkspace, 'Cargo.toml'), 'health workspace Cargo.toml', statFile);
  const config = await loadConfig(configPath);
  if (resolve(config.broker.externalRoot.path) !== externalRoot) throw new Error('config broker.externalRoot.path differs from --external-root');
  if (resolve(config.unixSocket ?? '') !== options.endpoint) throw new Error('config unixSocket differs from --endpoint');
  if (typeof config.broker.activationFencePath !== 'string' || !isAbsolute(config.broker.activationFencePath) || !contained(resolve(config.broker.activationFencePath), externalRoot)) throw new Error('config broker.activationFencePath must be an absolute path contained by --external-root');
  const plist = await load(plistPath, 'utf8');
  if (!exactPlistLabel(plist, options.launchdLabel)) throw new Error('plist Label differs from --label');
  const programArguments = plistProgramArguments(plist); const runtimeScript = resolve(externalRoot, 'runtime-current', 'build-control-broker', 'src', 'broker.mjs');
  if (programArguments.length !== 3 || programArguments.some((value) => !isAbsolute(value) || /\s/.test(value)) || resolve(programArguments[1]) !== runtimeScript || resolve(programArguments[2]) !== configPath) throw new Error('plist ProgramArguments must be exact node, runtime-current broker, & config paths');
  const nodePath = await canonicalize(programArguments[0]); await assertFile(nodePath, 'plist Node executable', statFile);
  // Fence new admissions first, then ask live broker for all admitting/queued/
  // running roots. Filesystem markers remain independent writer recovery proof.
  const drainFence = await beginDrain(externalRoot);
  try {
    const brokerDrain = await proveBrokerDrain(externalRoot, options.endpoint);
    const thinDrain = await proveThinDrain(externalRoot);
    const stopProof = await stopManaged({ launchdLabel: options.launchdLabel, uid: options.uid, plistPath, endpoint: options.endpoint });
    const result = await install({
      externalRoot, sourcePackagesRoot, sourceRevision: options.sourceRevision, sourceTreeDigest: options.sourceTreeDigest, config, version: options.version, launchdLabel: options.launchdLabel, uid: options.uid, endpoint: options.endpoint,
      plistPath, configPath, nodePath, healthWorkspace, healthTimeoutMs: options.timeoutMs, timeoutMs: options.timeoutMs,
      drainProbe: async () => proveThinDrain(externalRoot),
    });
    return Object.freeze({ configPath, externalRoot, sourcePackagesRoot, plistPath, brokerDrain, thinDrain, stopProof, result });
  } finally { await endDrain(drainFence); }
}

async function main() {
  const receipt = await runMacOSCutover(process.argv.slice(2));
  process.stdout.write(`${JSON.stringify(receipt)}\n`);
}

if (import.meta.url === `file://${process.argv[1]}`) main().catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; });
