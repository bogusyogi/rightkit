import { existsSync, readFileSync } from 'node:fs';
import { homedir, userInfo } from 'node:os';
import { posix, win32 } from 'node:path';
import path from 'node:path';

/**
 * The value of `RUSTRIGHT_<name>`, else of its legacy `RIGHTKIT_<name>` spelling (empty is unset). The same precedence as RustRight's resolver
 * (rustright-config managed_roots.rs `env_named`): a current name always wins over the legacy one it replaces, so a stale legacy variable can never override it.
 */
export const envNamed = (env, name) => {
  for (const key of [`RUSTRIGHT_${name}`, `RIGHTKIT_${name}`]) if (typeof env?.[key] === 'string' && env[key] !== '') return env[key];
  return undefined;
};

/**
 * The managed roots (GB-63: ONE resolver, the shape of RustRight's `rustright-config::managed_roots`, exercised against its shared fixtures). The data root is `DATA_ROOT`,
 * `MANAGED_ROOT`, `EXTERNAL_ROOT` (RUSTRIGHT_ names first, then the legacy RIGHTKIT_ ones), else the legacy root; the control root is `CONTROL_ROOT`, else the platform default.
 * `runtimeCurrent` (the wire key, kept for compatibility) is `<control>/runtime`, the one fixed live runtime directory; the installer's own runtime sits at its final RustRight
 * path, `runtimeRoot` (`RUSTRIGHT_RUNTIME_ROOT`, else RustRight's per-platform default) and `liveRuntime` = `<runtimeRoot>/runtime`, which supersedes it.
 */
export function resolveManagedRoots({ env = process.env, platform = process.platform, home = homedir(), controlRoot, dataRoot, runtimeRoot } = {}) {
  const paths = platform === 'win32' ? win32 : posix;
  const legacyRoot = platform === 'win32' ? 'D:\\.rightkit-managed' : '/Volumes/D/.rightkit-managed';
  dataRoot ??= envNamed(env, 'DATA_ROOT') || envNamed(env, 'MANAGED_ROOT') || envNamed(env, 'EXTERNAL_ROOT') || legacyRoot;
  controlRoot ??= envNamed(env, 'CONTROL_ROOT') || (platform === 'darwin'
    ? paths.join(home, 'Library', 'Application Support', 'RightKit', 'build-control') : dataRoot);
  runtimeRoot ??= envNamed(env, 'RUNTIME_ROOT') || (platform === 'darwin' ? paths.join(home, 'Library', 'Application Support', 'RustRight') : platform === 'win32' ? 'D:\\.rustright' : controlRoot);
  return Object.freeze({ controlRoot, dataRoot, legacyRoot, runtimeCurrent: paths.join(controlRoot, 'runtime'), runtimeRoot, liveRuntime: paths.join(runtimeRoot, 'runtime'),
    configPath: paths.join(controlRoot, 'config.json'), socket: platform === 'win32' ? null : paths.join(controlRoot, 'broker.sock'),
    logs: paths.join(controlRoot, 'logs'), agentBin: paths.join(controlRoot, 'agent-bin'),
    agentPolicy: paths.join(controlRoot, 'agent-policy'), ownershipInstall: paths.join(controlRoot, 'ownership-install'),
    // The Mac signing agent already lives on the internal disk; moving it would force a re-provision.
    signingAgent: platform === 'darwin' ? paths.join(home, 'Library', 'Application Support', 'Orthic Labs', 'shared', 'signing-agent')
      : paths.join(controlRoot, 'signing-agent') });
}

export function managedControlCandidates(options = {}) {
  const platform = options.platform ?? process.platform;
  if (!['darwin', 'win32'].includes(platform)) {
    const env = options.env ?? process.env;
    return [...new Set([options.controlRoot, envNamed(env, 'CONTROL_ROOT'), options.dataRoot, envNamed(env, 'DATA_ROOT'), envNamed(env, 'MANAGED_ROOT'), envNamed(env, 'EXTERNAL_ROOT'), options.clientRoot].filter(Boolean))];
  }
  const roots = resolveManagedRoots(options);
  return [...new Set([roots.controlRoot, roots.dataRoot, options.clientRoot, roots.legacyRoot].filter(Boolean))];
}

/** The pipe RustRight's installer enforces, and the pre-rename one still named by processes started before the cutover (which means the same broker). */
export const CANONICAL_WINDOWS_PIPE = '\\\\.\\pipe\\rustright-broker';
export const LEGACY_WINDOWS_PIPE = '\\\\.\\pipe\\rightkit-build-control';

/** The `windowsPipe` an installed broker config names (config.json of the first candidate root that has a valid one); a pre-rename name maps to the canonical pipe; anything but `\\.\pipe\<name>` is ignored. */
export function configuredWindowsPipe(roots, readFile = (file) => readFileSync(file, 'utf8')) {
  for (const root of roots) {
    let pipe;
    try { pipe = JSON.parse(readFile(win32.join(root, 'config.json')))?.windowsPipe; } catch { continue; }
    if (typeof pipe !== 'string' || !pipe.startsWith('\\\\.\\pipe\\')) continue;
    const name = pipe.slice('\\\\.\\pipe\\'.length);
    if (!name || /[\\/]/.test(name)) continue;
    return pipe.toLowerCase() === LEGACY_WINDOWS_PIPE.toLowerCase() ? CANONICAL_WINDOWS_PIPE : pipe;
  }
  return undefined;
}

/**
 * The broker endpoint, in RustRight's order (GB-63): an explicit `RUSTRIGHT_BUILD_BROKER_SOCKET` (else the legacy spelling) is authoritative (on Windows the pre-rename pipe means
 * the canonical one); on Unix the first existing `broker.sock` among the candidate roots; on Windows there is no socket file but there is always an endpoint: the pipe the installed
 * config names (a custom or migrated name is never ignored), else the canonical pipe, so a Windows shell with no environment still reaches the broker.
 */
export function discoverBrokerSocket(env = process.env, { exists = existsSync, readFile, ...options } = {}) {
  const platform = options.platform ?? process.platform;
  const explicit = envNamed(env, 'BUILD_BROKER_SOCKET');
  if (explicit) return platform === 'win32' && explicit.toLowerCase() === LEGACY_WINDOWS_PIPE.toLowerCase() ? CANONICAL_WINDOWS_PIPE : explicit;
  const candidates = managedControlCandidates({ ...options, env });
  if (platform === 'win32') return configuredWindowsPipe(candidates, readFile) ?? CANONICAL_WINDOWS_PIPE;
  for (const root of candidates) {
    const socket = posix.join(root, 'broker.sock');
    if (exists(socket)) return socket;
  }
  return undefined;
}

/**
 * The installed RustRight client, selected in ONE order everywhere (doctor, carry, status, lease and dispatch all call this): the live runtime at its final RustRight path
 * (`<runtimeRoot>/runtime`), the control root's `runtime` (the shared fixtures' `runtimeCurrent`), and only then the legacy `runtime-current` link of a host not yet cut over. The
 * Windows client is the native `rustright.exe`, never a `.cmd` shim; the caller probes the exact client returned before it marks any work (snapshot-broker.mjs clientStartable).
 */
export function managedRuntimeClient(env = process.env, platform = process.platform, exists = existsSync) {
  if (platform !== 'darwin' && platform !== 'win32') return undefined; // a direct (Linux) host has no broker client
  const paths = platform === 'win32' ? path.win32 : path.posix;
  const exe = platform === 'win32' ? 'rustright.exe' : 'rustright';
  const home = env.HOME || env.USERPROFILE || homedir();
  const roots = managedControlCandidates({ env, platform, home });
  const configured = resolveManagedRoots({ env, platform, home });
  const ordered = [paths.join(configured.liveRuntime, exe)];
  for (const root of roots) ordered.push(paths.join(root, 'runtime', exe));
  for (const root of roots) ordered.push(paths.join(root, 'runtime-current', exe)); // the pre-cutover link: last, never preferred over the live runtime
  if (platform === 'darwin') {
    ordered.push(path.posix.join(home, 'Library', 'Application Support', 'RustRight', 'runtime', 'rustright'));
    // Broker-managed HOME may point at a build home; the installed native client belongs to the OS account.
    ordered.push(path.posix.join(userInfo().homedir, 'Library', 'Application Support', 'RustRight', 'runtime', 'rustright'));
  } else {
    for (const root of roots) ordered.push(path.win32.join(path.win32.parse(root).root, '.rustright', 'runtime', 'rustright.exe'));
  }
  return ordered.find((client) => exists(client));
}
