import { existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { posix, win32 } from 'node:path';

const envValue = (env, key) => (typeof env?.[key] === 'string' && env[key] !== '' ? env[key] : undefined);

/**
 * `RUSTRIGHT_<name>` first, else its legacy `RIGHTKIT_<name>` spelling: the broker's `env_named` (rustright-config managed_roots.rs),
 * empty values unset. Every root and the socket read through this, so a host that moves to RUSTRIGHT_ only resolves the broker's roots.
 */
export const envNamed = (env, name) => envValue(env, `RUSTRIGHT_${name}`) ?? envValue(env, `RIGHTKIT_${name}`);

/** RIGHTKIT_MANAGED_ROOT remains a data-root override for existing release callers. */
export function resolveManagedRoots({ env = process.env, platform = process.platform, home = homedir(), controlRoot, dataRoot } = {}) {
  const paths = platform === 'win32' ? win32 : posix;
  const legacyRoot = platform === 'win32' ? 'D:\\.rightkit-managed' : '/Volumes/D/.rightkit-managed';
  dataRoot ??= envNamed(env, 'DATA_ROOT') ?? envNamed(env, 'MANAGED_ROOT') ?? envNamed(env, 'EXTERNAL_ROOT') ?? legacyRoot;
  controlRoot ??= envNamed(env, 'CONTROL_ROOT') ?? (platform === 'darwin'
    ? paths.join(home, 'Library', 'Application Support', 'RightKit', 'build-control') : dataRoot);
  return Object.freeze({ controlRoot, dataRoot, legacyRoot, runtimeCurrent: paths.join(controlRoot, 'runtime'), // contract rule 3: the one live runtime dir is `runtime`; the key stays runtimeCurrent

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
  // After the legacy root, the renamed RustRight roots (`rustright-install cutover --migrate-roots`; identity.rs renamed::control_root/data_root),
  // in the broker's order: a net for a host whose forwarding alias is gone.
  const home = options.home ?? homedir();
  const renamed = platform === 'win32' ? ['D:\\.rustright'] : [...(home ? [posix.join(home, 'Library', 'Application Support', 'RustRight', 'broker')] : []), '/Volumes/D/.rustright'];
  return [...new Set([roots.controlRoot, roots.dataRoot, options.clientRoot, roots.legacyRoot, ...renamed].filter(Boolean))];
}

export function discoverBrokerSocket(env = process.env, { exists = existsSync, ...options } = {}) {
  // The client reads RUSTRIGHT_BUILD_BROKER_SOCKET (rustright-cli client.rs); the RIGHTKIT_ spelling is the legacy one, read here, never set.
  const explicit = envNamed(env, 'BUILD_BROKER_SOCKET');
  if (explicit) return explicit;
  if ((options.platform ?? process.platform) === 'win32') return undefined;
  for (const root of managedControlCandidates({ ...options, env })) {
    const socket = posix.join(root, 'broker.sock');
    if (exists(socket)) return socket;
  }
  return undefined;
}
