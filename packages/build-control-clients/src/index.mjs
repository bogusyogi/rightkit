import { BUILD_CONTROL_PROTOCOL, FRONTENDS, SOURCE_RUNTIME_IDENTITY, captureDelegatedCompilerEnvironment, captureManagedRustEnvironment, normalizeBuildRequest, normalizeManagedAgentRequest, normalizeRuntimeIdentity, validateManagedRustupInvocation } from '@rightkit/build-control-core';
import net from 'node:net';
import { spawnSync } from 'node:child_process';
import { chmod, mkdir, readFile, readdir, rename, rm, writeFile } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, resolve, win32 } from 'node:path';
import { fileURLToPath } from 'node:url';

export const adapterRegistry = Object.freeze(Object.fromEntries(FRONTENDS.map((frontend) => [frontend, Object.freeze({ frontend, mode: 'REQUEST_ONLY' })])));
export function createClientRequest(frontend, input) {
  if (!adapterRegistry[frontend]) throw new TypeError('registered frontend is required');
  return normalizeBuildRequest({ ...input, frontend });
}

export const SHIM_ALIASES = Object.freeze({
  cargo: 'cargo', rustc: 'cargo', rustdoc: 'cargo', rustup: 'cargo',
  pnpm: 'node-pnpm', 'node-gyp': 'node-gyp',
  swift: 'swiftpm', swiftc: 'swiftpm',
  cmake: 'cmake', make: 'make', cc: 'raw-compiler', 'c++': 'raw-compiler', clang: 'raw-compiler', gcc: 'raw-compiler', npm: 'node-pnpm'
});
export const FRONTEND_SHIMS = Object.freeze({ cargo: 'cargo', swiftpm: 'swift', 'node-pnpm': 'pnpm', 'node-gyp': 'node-gyp', cmake: 'cmake', make: 'make', 'raw-compiler': 'cc' });
export const INSTALLED_MANAGED_SHIMS = Object.freeze(['cargo', 'rustc', 'rustdoc', 'rustup']);
export const MAX_BROKER_LINE_BYTES = 1024 * 1024;
export const BUILD_PRIORITIES = Object.freeze(['INSTALLER', 'INTERACTIVE', 'BATCH']);
export const JOB_REQUEST_TYPES = Object.freeze({ SUBMIT: 'SUBMIT', WATCH: 'WATCH', STATUS: 'STATUS', CANCEL: 'CANCEL' });
export const JOB_RESPONSE_TYPES = Object.freeze({ SUBMITTED: 'SUBMITTED', STATUS: 'STATUS', CANCEL_REQUESTED: 'CANCEL_REQUESTED' });

export class ToolChangedError extends Error {
  constructor(event) {
    super(`RightKit tool changed: ${event.toolId ?? '<unknown>'}; ${event.registrationAction ?? 'register the pinned tool before retrying'}`);
    this.name = 'ToolChangedError'; this.code = 'TOOL_CHANGED'; this.event = Object.freeze({ ...event });
  }
}

function jobId(value) {
  if (typeof value !== 'string' || !value.trim() || value.length > 200) throw new TypeError('non-empty request id is required');
  return value;
}

function jobRuntime(value) { return normalizeRuntimeIdentity(value); }

function buildPriority(environment) {
  const priority = environment.RIGHTKIT_BUILD_PRIORITY;
  if (priority === undefined) return null;
  if (!BUILD_PRIORITIES.includes(priority)) throw new TypeError('RIGHTKIT_BUILD_PRIORITY must be INSTALLER, INTERACTIVE, or BATCH');
  return priority;
}

export function createNestedManagedAgentRequest({ parentId, capabilityToken, ...input }) {
  return createManagedAgentRequest({ ...input, environment: { ...(input.environment ?? {}), RIGHTKIT_PARENT_REQUEST_ID: jobId(parentId), ...(capabilityToken === undefined ? {} : { RIGHTKIT_PARENT_CAPABILITY_TOKEN: capabilityToken }) } });
}

export function createJobCommand(type, { message, id, runtimeIdentity, source } = {}) {
  if (!Object.values(JOB_REQUEST_TYPES).includes(type)) throw new TypeError('registered job operation is required');
  const runtime = jobRuntime(runtimeIdentity ?? message?.runtime ?? SOURCE_RUNTIME_IDENTITY);
  if (type === JOB_REQUEST_TYPES.SUBMIT) {
    if (!message || message.type !== 'MANAGED_AGENT' || message.protocol !== BUILD_CONTROL_PROTOCOL) throw new TypeError('managed-agent job payload is required');
    const normalizedMessage = Object.freeze({ ...message, runtime: jobRuntime(message.runtime) });
    if (normalizedMessage.runtime.version !== runtime.version || normalizedMessage.runtime.clientDigest !== runtime.clientDigest || normalizedMessage.runtime.brokerDigest !== runtime.brokerDigest) throw new TypeError('job & managed-agent runtime identities must match');
    return Object.freeze({ type, protocol: BUILD_CONTROL_PROTOCOL, runtime, job: normalizedMessage });
  }
  if (type === JOB_REQUEST_TYPES.CANCEL && source !== undefined && !['USER_CANCEL', 'OPERATOR_CANCEL', 'CLIENT_SIGNAL', 'RELEASE_PRIORITY_CANCEL'].includes(source)) throw new TypeError('registered cancellation source is required');
  return Object.freeze({ type, protocol: BUILD_CONTROL_PROTOCOL, runtime, id: jobId(id), ...(type === JOB_REQUEST_TYPES.CANCEL && source ? { source } : {}) });
}

export function shimName(argv0) {
  const name = basename(argv0 ?? '').replace(/\.(?:cmd|exe|ps1)$/i, '').toLowerCase();
  if (!SHIM_ALIASES[name]) throw new TypeError(`unregistered managed-agent shim: ${name || '<empty>'}`);
  return name;
}

export function createManagedAgentRequest({ argv0, argv = [], workspace = process.cwd(), environment = {}, runtimeIdentity = SOURCE_RUNTIME_IDENTITY }) {
  const shim = shimName(argv0);
  const frontend = SHIM_ALIASES[shim];
  const invocation = normalizedInvocation(frontend, argv, resolve(workspace));
  validateManagedRustupInvocation(shim, invocation.argv);
  const observedEnvironment = captureManagedRustEnvironment(environment);
  const parentId = environment.RIGHTKIT_PARENT_REQUEST_ID;
  const capabilityToken = environment.RIGHTKIT_PARENT_CAPABILITY_TOKEN;
  const delegatedCompilerEnvironment = parentId !== undefined && ['rustc', 'rustdoc'].includes(shim) ? captureDelegatedCompilerEnvironment(environment) : {};
  const request = normalizeManagedAgentRequest({ frontend, argv: invocation.argv, workspace: invocation.workspace, environment: delegatedCompilerEnvironment });
  const priority = buildPriority(environment);
  const execution = parentId === undefined ? null : Object.freeze({ mode: 'NESTED', parentId: jobId(parentId), ...(capabilityToken === undefined ? {} : { capabilityToken }) });
  return Object.freeze({ type: 'MANAGED_AGENT', protocol: BUILD_CONTROL_PROTOCOL, runtime: normalizeRuntimeIdentity(runtimeIdentity), shim, observedEnvironment, request, ...(priority ? { priority } : {}), ...(execution ? { execution } : {}) });
}

export function createRightkitRequest({ argv = [], workspace = process.cwd(), environment = {}, runtimeIdentity = SOURCE_RUNTIME_IDENTITY }) {
  if (!Array.isArray(argv) || argv.some((value) => typeof value !== 'string')) throw new TypeError('rightkit arguments must be strings');
  let shim; let toolArgs;
  if (argv[0] === 'build') {
    const [,, separator, ...values] = argv;
    const frontend = argv[1];
    if (!FRONTEND_SHIMS[frontend] || separator !== '--') throw new TypeError('usage: rightkit build <registered-frontend> -- <args>');
    shim = FRONTEND_SHIMS[frontend]; toolArgs = values;
  } else {
    if (!argv[0] || argv[0] === '--') throw new TypeError('usage: rightkit <registered-shim> [args]');
    shim = shimName(argv[0]); toolArgs = argv.slice(1);
  }
  return Object.freeze({ ...createManagedAgentRequest({ argv0: shim, argv: toolArgs, workspace, environment, runtimeIdentity }), entrypoint: 'rightkit' });
}

export async function loadRuntimeIdentity({ manifestPath = fileURLToPath(new URL('../../runtime-manifest.json', import.meta.url)) } = {}) {
  try { return normalizeRuntimeIdentity(JSON.parse(await readFile(manifestPath, 'utf8'))); }
  catch (error) { if (error.code === 'ENOENT') return SOURCE_RUNTIME_IDENTITY; throw new Error(`invalid RightKit runtime manifest: ${error.message}`); }
}

function normalizedInvocation(frontend, argv, workspace) {
  if (frontend === 'cargo') {
    const separator = argv.indexOf('--');
    const index = argv.findIndex((value, candidate) => (separator < 0 || candidate < separator) && (value === '--manifest-path' || value.startsWith('--manifest-path=')));
    if (index >= 0) {
      const manifest = argv[index] === '--manifest-path' ? argv[index + 1] : argv[index].slice('--manifest-path='.length);
      if (manifest) {
        const selected = resolve(workspace, manifest); const values = [...argv];
        if (values[index] === '--manifest-path') values[index + 1] = selected; else values[index] = `--manifest-path=${selected}`;
        return { argv: values, workspace: dirname(selected) };
      }
    }
    return { argv, workspace };
  }
  if (frontend !== 'node-pnpm') return { argv, workspace };
  const values = [...argv];
  for (let index = 0; index < values.length; index += 1) {
    const value = values[index];
    if (['--dir', '-C', '--prefix'].includes(value)) {
      if (!values[index + 1]) throw new TypeError(`${value} requires a workspace path`);
      const selected = resolve(workspace, values[index + 1]);
      values.splice(index, 2);
      return { argv: values, workspace: selected };
    }
    const match = value.match(/^--(?:dir|prefix)=(.+)$/);
    if (match) {
      values.splice(index, 1);
      return { argv: values, workspace: resolve(workspace, match[1]) };
    }
  }
  return { argv: values, workspace };
}

export function brokerEndpoint(environment = process.env) {
  const endpoint = environment.RIGHTKIT_BUILD_BROKER_SOCKET;
  if (!endpoint || typeof endpoint !== 'string') throw new Error('RightKit broker endpoint is not configured');
  return endpoint;
}

export async function sendManagedAgentRequest(message, { endpoint = brokerEndpoint(), connect = net.createConnection, onEvent = () => {} } = {}) {
  return new Promise((resolvePromise, reject) => {
    let settled = false;
    let terminalResult = null; let handshake = false;
    let buffer = '';
    let socket;
    const fail = (error) => { if (!settled) { settled = true; socket?.destroy(); reject(error); } };
    try { socket = connect(endpoint); } catch (error) { fail(new Error(`RightKit broker unavailable: ${error.message}`)); return; }
    socket.once('error', (error) => fail(new Error(`RightKit broker unavailable: ${error.message}`)));
    socket.once('connect', () => socket.write(`${JSON.stringify(message)}\n`));
    socket.on('data', (chunk) => {
      if (settled) return;
      buffer += chunk.toString('utf8');
      if (Buffer.byteLength(buffer, 'utf8') > MAX_BROKER_LINE_BYTES) return fail(new Error('RightKit broker response exceeds line limit'));
      for (;;) {
        const newline = buffer.indexOf('\n');
        if (newline < 0) break;
        const line = buffer.slice(0, newline); buffer = buffer.slice(newline + 1);
        if (Buffer.byteLength(line, 'utf8') > MAX_BROKER_LINE_BYTES) return fail(new Error('RightKit broker response exceeds line limit'));
        if (!line) continue;
        let event;
        try { event = JSON.parse(line); } catch { return fail(new Error('RightKit broker returned invalid JSON')); }
        if (!event || typeof event !== 'object' || typeof event.type !== 'string') return fail(new Error('RightKit broker returned untyped response'));
        if (terminalResult) return fail(new Error('RightKit broker sent event after terminal result'));
        if (event.type === 'TOOL_CHANGED') {
          if (typeof event.toolId !== 'string' || !event.toolId || typeof event.registrationAction !== 'string' || !event.registrationAction) return fail(new Error('RightKit broker returned malformed TOOL_CHANGED event'));
          onEvent(event); return fail(new ToolChangedError(event));
        }
        if (!handshake && event.type === 'HANDSHAKE') {
          let identity; try { identity = normalizeRuntimeIdentity(event.runtime); } catch (error) { return fail(error); }
          if (identity.version !== message.runtime?.version || identity.clientDigest !== message.runtime?.clientDigest || identity.brokerDigest !== message.runtime?.brokerDigest) return fail(new Error('RightKit client/broker runtime digest mismatch'));
          handshake = true;
        } else if (!handshake && !(event.type === 'RESULT' && event.status === 'REJECTED')) return fail(new Error('RightKit broker omitted v2 runtime handshake'));
        if (event.type === 'RESULT' && (!Number.isInteger(event.exitCode) || event.exitCode < 0 || event.exitCode > 255)) return fail(new Error('RightKit broker returned invalid exit status'));
        onEvent(event);
        if (event.type === 'RESULT') {
          terminalResult = event;
        }
      }
      if (terminalResult && !settled) {
        settled = true;
        socket.destroy();
        resolvePromise(terminalResult);
      }
    });
    socket.once('end', () => {
      if (settled) return;
      if (!terminalResult) return fail(new Error('RightKit broker closed before typed result'));
      settled = true;
      resolvePromise(terminalResult);
    });
    socket.once('close', (hadError) => {
      if (settled) return;
      if (hadError) return fail(new Error('RightKit broker connection closed with error'));
      if (!terminalResult) return fail(new Error('RightKit broker closed before typed result'));
      settled = true;
      resolvePromise(terminalResult);
    });
  });
}

export async function sendJobCommand(command, { endpoint = brokerEndpoint(), connect = net.createConnection, onEvent = () => {}, completeOn } = {}) {
  if (!command || !Object.values(JOB_REQUEST_TYPES).includes(command.type)) throw new TypeError('registered job command is required');
  if (!(completeOn instanceof Set) || completeOn.size === 0) throw new TypeError('job completion event types are required');
  return new Promise((resolvePromise, reject) => {
    let settled = false; let completed = null; let handshake = false; let buffer = ''; let socket;
    const fail = (error) => { if (!settled) { settled = true; socket?.destroy(); reject(error); } };
    try { socket = connect(endpoint); } catch (error) { fail(new Error(`RightKit broker unavailable: ${error.message}`)); return; }
    socket.once('error', (error) => fail(new Error(`RightKit broker unavailable: ${error.message}`)));
    socket.once('connect', () => socket.write(`${JSON.stringify(command)}\n`));
    socket.on('data', (chunk) => {
      if (settled) return;
      buffer += chunk.toString('utf8');
      if (Buffer.byteLength(buffer, 'utf8') > MAX_BROKER_LINE_BYTES) return fail(new Error('RightKit broker response exceeds line limit'));
      for (;;) {
        const newline = buffer.indexOf('\n'); if (newline < 0) break;
        const line = buffer.slice(0, newline); buffer = buffer.slice(newline + 1);
        if (Buffer.byteLength(line, 'utf8') > MAX_BROKER_LINE_BYTES) return fail(new Error('RightKit broker response exceeds line limit'));
        if (!line) continue;
        let event; try { event = JSON.parse(line); } catch { return fail(new Error('RightKit broker returned invalid JSON')); }
        if (!event || typeof event !== 'object' || typeof event.type !== 'string') return fail(new Error('RightKit broker returned untyped response'));
        if (completed) return fail(new Error('RightKit broker sent event after terminal job response'));
        if (event.type === 'TOOL_CHANGED') {
          if (typeof event.toolId !== 'string' || !event.toolId || typeof event.registrationAction !== 'string' || !event.registrationAction) return fail(new Error('RightKit broker returned malformed TOOL_CHANGED event'));
          onEvent(event); return fail(new ToolChangedError(event));
        }
        if (!handshake && event.type === 'HANDSHAKE') {
          let identity; try { identity = normalizeRuntimeIdentity(event.runtime); } catch (error) { return fail(error); }
          if (identity.version !== command.runtime.version || identity.clientDigest !== command.runtime.clientDigest || identity.brokerDigest !== command.runtime.brokerDigest) return fail(new Error('RightKit client/broker runtime digest mismatch'));
          handshake = true;
        } else if (!handshake && !(event.type === 'RESULT' && event.status === 'REJECTED')) return fail(new Error('RightKit broker omitted v2 runtime handshake'));
        if (event.type === 'RESULT' && (!Number.isInteger(event.exitCode) || event.exitCode < 0 || event.exitCode > 255)) return fail(new Error('RightKit broker returned invalid exit status'));
        if (event.type === 'RESULT' && event.status === 'REJECTED') { onEvent(event); return fail(new Error(event.error ?? 'RightKit broker rejected job command')); }
        onEvent(event);
        if (completeOn.has(event.type)) completed = event;
      }
      if (completed && !settled) { settled = true; socket.destroy(); resolvePromise(completed); }
    });
    socket.once('end', () => { if (!settled) fail(new Error('RightKit broker closed before typed job response')); });
    socket.once('close', (hadError) => { if (!settled) fail(new Error(hadError ? 'RightKit broker connection closed with error' : 'RightKit broker closed before typed job response')); });
  });
}

export function submitManagedAgentJob(message, options = {}) { return sendJobCommand(createJobCommand(JOB_REQUEST_TYPES.SUBMIT, { message }), { ...options, completeOn: new Set([JOB_RESPONSE_TYPES.SUBMITTED, 'RESULT']) }); }
export function watchManagedAgentJob(id, options = {}) { return sendJobCommand(createJobCommand(JOB_REQUEST_TYPES.WATCH, { id, runtimeIdentity: options.runtimeIdentity }), { ...options, completeOn: new Set(['RESULT']) }); }
export function statusManagedAgentJob(id, options = {}) { return sendJobCommand(createJobCommand(JOB_REQUEST_TYPES.STATUS, { id, runtimeIdentity: options.runtimeIdentity }), { ...options, completeOn: new Set([JOB_RESPONSE_TYPES.STATUS]) }); }
export function cancelManagedAgentJob(id, options = {}) { return sendJobCommand(createJobCommand(JOB_REQUEST_TYPES.CANCEL, { id, runtimeIdentity: options.runtimeIdentity, source: options.source }), { ...options, completeOn: new Set([JOB_RESPONSE_TYPES.CANCEL_REQUESTED]) }); }
export async function submitAndWatchManagedAgentJob(message, options = {}) {
  const submitted = await submitManagedAgentJob(message, options);
  if (submitted.type === 'RESULT') return submitted;
  const id = jobId(submitted?.id);
  options.onSubmitted?.(Object.freeze({ id, state: submitted.state }));
  return watchManagedAgentJob(id, { ...options, runtimeIdentity: message.runtime });
}

export function shimManifest({ installRoot = '<rightkit-agent-bin>' } = {}) {
  return Object.freeze({ schema: 'rightkit.build-control.agent-shims.v2', installRoot, executable: 'rightkit', pathPrecedence: 'prepend', links: Object.freeze([...INSTALLED_MANAGED_SHIMS.map((name) => Object.freeze({ name, target: 'rightkit-managed-agent' })), Object.freeze({ name: 'rightkit', target: 'rightkit' })]), guarantee: 'Installed Rust/Cargo shims & rightkit resolve runtime-current, submit digest-bound MANAGED_AGENT v2 requests only, & never spawn a real toolchain.' });
}

const INSTALL_MARKER = 'rightkit.build-control.agent-shims.install.v1';
const INSTALL_MANIFEST = '.rightkit-managed-agent-shims.json';
const NATIVE_WINDOWS_SHIMS = Object.freeze(['cargo', 'rustc', 'rustdoc', 'rustup']);
const stripBom = (value) => typeof value === 'string' && value.charCodeAt(0) === 0xfeff ? value.slice(1) : value;

function shellQuote(value) { return `'${String(value).replaceAll("'", "'\\\"'\\\"'")}'`; }
async function writeAtomic(path, data, mode) {
  const temporary = `${path}.${process.pid}.${Math.random().toString(16).slice(2)}.tmp`;
  await writeFile(temporary, data, { encoding: 'utf8', flag: 'wx', mode });
  await rename(temporary, path);
}

function ownedLinkNames(manifest) {
  if (!manifest || manifest.marker !== INSTALL_MARKER || !Array.isArray(manifest.links) || manifest.links.some((name) => typeof name !== 'string' || !name || basename(name) !== name)) throw new Error('agent shim directory is not an owned RightKit install');
  return new Set(manifest.links);
}

async function existingInstall(directory) {
  let entries;
  try { entries = await readdir(directory); } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    await mkdir(directory);
    return null;
  }
  if (entries.length === 0) return null;
  if (!entries.includes(INSTALL_MANIFEST)) throw new Error('agent shim directory is not an owned RightKit install');
  let manifest;
  try { manifest = JSON.parse(stripBom(await readFile(resolve(directory, INSTALL_MANIFEST), 'utf8'))); } catch { throw new Error('agent shim directory has invalid ownership manifest'); }
  const owned = ownedLinkNames(manifest); owned.add(INSTALL_MANIFEST);
  if (!entries.every((entry) => owned.has(entry))) throw new Error('agent shim directory contains foreign files');
  return manifest;
}

export function compileNativeWindowsLauncher({ outputPath, cscPath = process.env.RIGHTKIT_CSC_PATH, sourcePath = fileURLToPath(new URL('./native-windows-launcher/ManagedRustLauncher.cs', import.meta.url)) } = {}) {
  if (!outputPath || !isAbsolute(outputPath)) throw new TypeError('native launcher output path must be absolute');
  const compiler = cscPath || (process.env.WINDIR ? join(process.env.WINDIR, 'Microsoft.NET', 'Framework64', 'v4.0.30319', 'csc.exe') : null);
  if (!compiler) throw new Error('Windows .NET Framework csc.exe is required for native agent shims');
  const result = spawnSync(compiler, ['/nologo', '/target:exe', '/optimize+', `/out:${outputPath}`, sourcePath], { encoding: 'utf8', windowsHide: true });
  if (result.error || result.status !== 0) throw new Error(`native Windows launcher compilation failed: ${result.stderr || result.error?.message || `exit ${result.status}`}`);
  return outputPath;
}

async function nativeLauncherBytes({ outputPath, injected, cscPath }) {
  if (injected !== undefined) {
    if (!Buffer.isBuffer(injected) || injected.length < 2 || injected[0] !== 0x4d || injected[1] !== 0x5a) throw new TypeError('native Windows launcher bytes must be a PE image');
    return injected;
  }
  compileNativeWindowsLauncher({ outputPath, cscPath });
  return readFile(outputPath);
}

export async function installAgentShims({ directory, platform = process.platform, runtimeCurrent, nodePath = process.execPath, nativeLauncherBytes: injectedNativeLauncherBytes, cscPath } = {}) {
  if (!directory || !isAbsolute(directory)) throw new TypeError('agent shim directory must be absolute');
  const isPlatformAbsolute = platform === 'win32' ? win32.isAbsolute : isAbsolute;
  const target = resolve(directory);
  const selectedRuntime = runtimeCurrent ?? (platform === 'win32' ? win32.resolve(win32.dirname(directory), 'runtime-current') : resolve(target, '..', 'runtime-current'));
  if (![selectedRuntime, nodePath].every((value) => typeof value === 'string' && isPlatformAbsolute(value))) throw new TypeError('runtime-current & node path must be absolute');
  const defaultBrokerEndpoint = platform === 'win32' ? null : resolve(target, '..', 'broker.sock');
  const manifest = shimManifest({ installRoot: target });
  const names = manifest.links.flatMap(({ name }) => platform === 'win32' ? [`${name}.cmd`, ...(NATIVE_WINDOWS_SHIMS.includes(name) ? [`${name}.exe`] : [])] : [name]);
  const previous = await existingInstall(target);
  if (previous) {
    const priorNames = ownedLinkNames(previous);
    for (const name of priorNames) if (!names.includes(name)) await rm(resolve(target, name), { force: false });
  }
  if (platform === 'win32') {
    const temporaryLauncher = join(dirname(target), `.rightkit-native-launcher-${process.pid}-${Math.random().toString(16).slice(2)}.exe`);
    let compiledLauncher;
    try {
      compiledLauncher = await nativeLauncherBytes({ outputPath: temporaryLauncher, injected: injectedNativeLauncherBytes, cscPath });
    } finally {
      await rm(temporaryLauncher, { force: true });
    }
    for (const { name } of manifest.links) {
      const destination = resolve(target, `${name}.cmd`);
      const executable = win32.join(selectedRuntime, 'build-control-clients', 'src', name === 'rightkit' ? 'rightkit.mjs' : 'managed-agent.mjs');
      const identity = name === 'rightkit' ? '' : `set "RIGHTKIT_MANAGED_AGENT_SHIM=%~n0"\r\n`;
      const wrapper = `@echo off\r\nrem ${INSTALL_MARKER}\r\nsetlocal\r\n${identity}"${nodePath}" "${executable}" %*\r\nexit /b %ERRORLEVEL%\r\n`;
      await writeAtomic(destination, wrapper);
    }
    for (const name of NATIVE_WINDOWS_SHIMS) await writeAtomic(resolve(target, `${name}.exe`), compiledLauncher);
  } else {
    for (const { name } of manifest.links) {
      const executable = join(selectedRuntime, 'build-control-clients', 'src', name === 'rightkit' ? 'rightkit.mjs' : 'managed-agent.mjs');
      const endpoint = `if [ -z "$RIGHTKIT_BUILD_BROKER_SOCKET" ]; then export RIGHTKIT_BUILD_BROKER_SOCKET=${shellQuote(defaultBrokerEndpoint)}; fi\n`;
      const identity = name === 'rightkit' ? '' : `export RIGHTKIT_MANAGED_AGENT_SHIM=${shellQuote(name)}\n`;
      const wrapper = `#!/bin/sh\n# ${INSTALL_MARKER}\n${endpoint}${identity}exec ${shellQuote(nodePath)} ${shellQuote(executable)} "$@"\n`;
      const destination = resolve(target, name);
      await writeAtomic(destination, wrapper, 0o755);
      await chmod(destination, 0o755);
    }
  }
  const installed = Object.freeze({ ...manifest, marker: INSTALL_MARKER, platform, runtimeCurrent: selectedRuntime, nodePath, links: Object.freeze(names) });
  await writeAtomic(resolve(target, INSTALL_MANIFEST), `${JSON.stringify(installed, null, 2)}\n`);
  return installed;
}
