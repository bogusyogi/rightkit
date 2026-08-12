import { FRONTENDS, normalizeBuildRequest, normalizeManagedAgentRequest } from '@rightkit/build-control-core';
import net from 'node:net';
import { chmod, mkdir, readFile, readdir, rename, rm, writeFile } from 'node:fs/promises';
import { basename, isAbsolute, resolve, win32 } from 'node:path';
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

export function shimName(argv0) {
  const name = basename(argv0 ?? '').replace(/\.(?:cmd|exe|ps1)$/i, '').toLowerCase();
  if (!SHIM_ALIASES[name]) throw new TypeError(`unregistered managed-agent shim: ${name || '<empty>'}`);
  return name;
}

export function createManagedAgentRequest({ argv0, argv = [], workspace = process.cwd(), environment = {} }) {
  const shim = shimName(argv0);
  const frontend = SHIM_ALIASES[shim];
  const invocation = normalizedInvocation(frontend, argv, resolve(workspace));
  const request = normalizeManagedAgentRequest({ frontend, argv: invocation.argv, workspace: invocation.workspace, environment });
  return Object.freeze({ type: 'MANAGED_AGENT', protocol: 'rightkit.build-control.client.v1', shim, request });
}

export function createRightkitRequest({ argv = [], workspace = process.cwd(), environment = {} }) {
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
  return Object.freeze({ ...createManagedAgentRequest({ argv0: shim, argv: toolArgs, workspace, environment }), entrypoint: 'rightkit' });
}

function normalizedInvocation(frontend, argv, workspace) {
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
    let terminalResult = null;
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
        if (event.type === 'RESULT' && (!Number.isInteger(event.exitCode) || event.exitCode < 0 || event.exitCode > 255)) return fail(new Error('RightKit broker returned invalid exit status'));
        onEvent(event);
        if (event.type === 'RESULT') {
          terminalResult = event;
          socket.end();
        }
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

export function shimManifest({ installRoot = '<rightkit-agent-bin>' } = {}) {
  return Object.freeze({ schema: 'rightkit.build-control.agent-shims.v1', installRoot, executable: 'rightkit', pathPrecedence: 'prepend', links: Object.freeze([...INSTALLED_MANAGED_SHIMS.map((name) => Object.freeze({ name, target: 'rightkit-managed-agent' })), Object.freeze({ name: 'rightkit', target: 'rightkit' })]), guarantee: 'Installed Rust/Cargo shims & rightkit submit MANAGED_AGENT requests only; they never spawn a real toolchain.' });
}

const managedAgentPath = fileURLToPath(new URL('./managed-agent.mjs', import.meta.url));
const rightkitPath = fileURLToPath(new URL('./rightkit.mjs', import.meta.url));
const INSTALL_MARKER = 'rightkit.build-control.agent-shims.install.v1';
const INSTALL_MANIFEST = '.rightkit-managed-agent-shims.json';

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
  try { manifest = JSON.parse(await readFile(resolve(directory, INSTALL_MANIFEST), 'utf8')); } catch { throw new Error('agent shim directory has invalid ownership manifest'); }
  const owned = ownedLinkNames(manifest); owned.add(INSTALL_MANIFEST);
  if (!entries.every((entry) => owned.has(entry))) throw new Error('agent shim directory contains foreign files');
  return manifest;
}

export async function installAgentShims({ directory, platform = process.platform, launcher = managedAgentPath, rightkitLauncher = rightkitPath, nodePath = process.execPath } = {}) {
  if (!directory || !isAbsolute(directory)) throw new TypeError('agent shim directory must be absolute');
  const isPlatformAbsolute = platform === 'win32' ? win32.isAbsolute : isAbsolute;
  if (![launcher, rightkitLauncher, nodePath].every((value) => typeof value === 'string' && isPlatformAbsolute(value))) throw new TypeError('launcher, rightkit launcher, and node path must be absolute');
  const target = resolve(directory);
  const manifest = shimManifest({ installRoot: target });
  const names = manifest.links.map(({ name }) => platform === 'win32' ? `${name}.cmd` : name);
  const previous = await existingInstall(target);
  if (previous) {
    const priorNames = ownedLinkNames(previous);
    for (const name of priorNames) if (!names.includes(name)) await rm(resolve(target, name), { force: false });
  }
  if (platform === 'win32') {
    for (const { name } of manifest.links) {
      const destination = resolve(target, `${name}.cmd`);
      const executable = name === 'rightkit' ? rightkitLauncher : launcher;
      const identity = name === 'rightkit' ? '' : `set "RIGHTKIT_MANAGED_AGENT_SHIM=%~n0"\r\n`;
      const wrapper = `@echo off\r\nrem ${INSTALL_MARKER}\r\nsetlocal\r\n${identity}"${nodePath}" "${executable}" %*\r\nexit /b %ERRORLEVEL%\r\n`;
      await writeAtomic(destination, wrapper);
    }
  } else {
    for (const { name } of manifest.links) {
      const executable = name === 'rightkit' ? rightkitLauncher : launcher;
      const identity = name === 'rightkit' ? '' : `export RIGHTKIT_MANAGED_AGENT_SHIM=${shellQuote(name)}\n`;
      const wrapper = `#!/bin/sh\n# ${INSTALL_MARKER}\n${identity}exec ${shellQuote(nodePath)} ${shellQuote(executable)} "$@"\n`;
      const destination = resolve(target, name);
      await writeAtomic(destination, wrapper, 0o755);
      await chmod(destination, 0o755);
    }
  }
  const installed = Object.freeze({ ...manifest, marker: INSTALL_MARKER, platform, launcher, rightkitLauncher, nodePath, links: Object.freeze(names) });
  await writeAtomic(resolve(target, INSTALL_MANIFEST), `${JSON.stringify(installed, null, 2)}\n`);
  return installed;
}
