import { createHash } from 'node:crypto';
import { existsSync, readFileSync, realpathSync } from 'node:fs';
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
 *
 * `legacyRoot` is the pre-rename Mac/Windows location the shared fixtures pin on every platform; it is never a default for a host that is neither: a Linux (or
 * other) host that names no data root gets `<home>/.local/state/rightkit` (the account's own state directory, where its runs, snapshots and build-ownership record
 * already live), never `/Volumes/D/.rightkit-managed`, a Mac volume that a Linux gate would otherwise create as root. Everything else is RustRight's order:
 * the data root is named by the same env, `controlRoot` = `dataRoot` off Mac, so a host that names its roots (every declared Linux broker, whose installer sets
 * RUSTRIGHT_DATA_ROOT / RUSTRIGHT_CONTROL_ROOT) resolves exactly as `rustright-config::managed_roots` does. RustRight resolves a host with NO environment the same
 * way (rustright 07829c05, pinned in the shared fixtures as "linux no-env defaults to the account state directory"). The broker's answering roots, not either
 * resolver's guess, say where a running broker keeps its state.
 */
export function resolveManagedRoots({ env = process.env, platform = process.platform, home = homedir(), controlRoot, dataRoot, runtimeRoot } = {}) {
  const paths = platform === 'win32' ? win32 : posix;
  const legacyRoot = platform === 'win32' ? 'D:\\.rightkit-managed' : '/Volumes/D/.rightkit-managed';
  const managedPlatform = platform === 'darwin' || platform === 'win32';
  dataRoot ??= envNamed(env, 'DATA_ROOT') || envNamed(env, 'MANAGED_ROOT') || envNamed(env, 'EXTERNAL_ROOT') || (managedPlatform ? legacyRoot : paths.join(home, '.local', 'state', 'rightkit'));
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

/**
 * Where a gate run keeps evidence that must not live in the repository (failing scans' complete output, the direct executor's
 * diagnostics): RIGHTKIT_SCRATCH_ROOT, else `<data root>/runs`. A managed Mac or Windows host has a data root, and so does any host
 * that names one in the environment. A direct (Linux and other) host with none used to fall to the Mac data volume,
 * `/Volumes/D/.rightkit-managed/runs`, and create it; its data root now defaults to the account's own state directory
 * (resolveManagedRoots: `~/.local/state/rightkit`), beside the snapshots and the build-ownership record, so its runs are `~/.local/state/rightkit/runs`.
 */
export function gateRunsRoot({ env = process.env, platform = process.platform, home = homedir() } = {}) {
  if (env.RIGHTKIT_SCRATCH_ROOT) return env.RIGHTKIT_SCRATCH_ROOT;
  return (platform === 'win32' ? win32 : posix).join(resolveManagedRoots({ env, platform, home }).dataRoot, 'runs');
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

// The declared Linux broker route's variables (owner: managed-cargo.mjs, which imports this leaf module and so cannot be imported here; its names are spelled
// once more because a cycle would break the leaf property broker-client.mjs relies on). The route is opt-in and declared explicitly, never inferred.
const LINUX_DECLARATION = Object.freeze({ client: 'RIGHTKIT_LINUX_BROKER_CLIENT', socket: 'RIGHTKIT_LINUX_BROKER_SOCKET', sha256: 'RIGHTKIT_LINUX_BROKER_CLIENT_SHA256' });
const sha256OfFile = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');
const declaredAbsolute = (value) => typeof value === 'string' && posix.isAbsolute(value) && !value.split('/').includes('..') && !value.includes('\0');

/**
 * The client of a Linux host's DECLARED broker route (RIGHTKIT_LINUX_BROKER_CLIENT / _SOCKET / _CLIENT_SHA256), which is the one client the gate's executor
 * (managed-cargo resolveLinuxBrokerRoute) drives: the same variables, the same absolute-path rule, the same required SHA-256 pin, the same refusal of another named
 * endpoint, and the executor's real path. A host that declared nothing stays direct and has no client (undefined, as before); a declaration that is incomplete,
 * names a client that is absent, is not the pinned bytes, or sits beside another endpoint also yields undefined here, never a different client: the typed refusal
 * (RK_LINUX_BROKER_UNAVAILABLE, with its reason) is the executor's. The owner-and-writability walk of the path is the executor's too; this lookup is for callers
 * that only need to find the client the declared route uses, and anything that RUNS a scan or a build goes through the executor's validated `client`.
 * A RustRight client on PATH, a shim directory or a managed root never makes a Linux host a broker host.
 */
function declaredLinuxBrokerClient(env, exists, { realpath = (file) => realpathSync.native(file), digest = sha256OfFile } = {}) {
  const named = (name) => (typeof env?.[name] === 'string' ? env[name] : undefined);
  if (!Object.values(LINUX_DECLARATION).some((name) => named(name) !== undefined)) return undefined;
  const client = named(LINUX_DECLARATION.client); const socket = named(LINUX_DECLARATION.socket); const pinned = named(LINUX_DECLARATION.sha256);
  if (!declaredAbsolute(client) || !declaredAbsolute(socket) || !/^[0-9a-f]{64}$/i.test(pinned ?? '') || !exists(client)) return undefined;
  const real = (file) => { try { return realpath(file); } catch { return file; } };
  for (const other of ['RUSTRIGHT_BUILD_BROKER_SOCKET', 'RIGHTKIT_BUILD_BROKER_SOCKET']) {
    const value = named(other);
    if (value !== undefined && value !== '' && value !== socket && real(value) !== real(socket)) return undefined; // two endpoints: ambiguous, so none
  }
  const resolved = real(client);
  try { if (digest(resolved).toLowerCase() !== pinned.toLowerCase()) return undefined; } catch { return undefined; }
  return resolved;
}

/**
 * The installed RustRight client, selected in ONE order everywhere (doctor, carry, status, lease and dispatch all call this): the live runtime at its final RustRight path
 * (`<runtimeRoot>/runtime`), the control root's `runtime` (the shared fixtures' `runtimeCurrent`), and only then the legacy `runtime-current` link of a host not yet cut over. The
 * Windows client is the native `rustright.exe`, never a `.cmd` shim; the caller probes the exact client returned before it marks any work (snapshot-broker.mjs clientStartable).
 */
export function managedRuntimeClient(env = process.env, platform = process.platform, exists = existsSync, ops = {}) {
  if (platform !== 'darwin' && platform !== 'win32') return declaredLinuxBrokerClient(env, exists, ops); // a direct (Linux) host has a broker client only by declaring one
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
