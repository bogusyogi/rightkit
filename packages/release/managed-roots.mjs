import { existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { posix, win32 } from 'node:path';

/** RIGHTKIT_MANAGED_ROOT remains a data-root override for existing release callers. */
export function resolveManagedRoots({ env = process.env, platform = process.platform, home = homedir(), controlRoot, dataRoot } = {}) {
  const paths = platform === 'win32' ? win32 : posix;
  const legacyRoot = platform === 'win32' ? 'D:\\.rightkit-managed' : '/Volumes/D/.rightkit-managed';
  dataRoot ??= env.RIGHTKIT_DATA_ROOT || env.RIGHTKIT_MANAGED_ROOT || env.RIGHTKIT_EXTERNAL_ROOT || legacyRoot;
  controlRoot ??= env.RIGHTKIT_CONTROL_ROOT || (platform === 'darwin'
    ? paths.join(home, 'Library', 'Application Support', 'RightKit', 'build-control') : dataRoot);
  return Object.freeze({ controlRoot, dataRoot, legacyRoot, runtimeCurrent: paths.join(controlRoot, 'runtime-current'),
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
    return [...new Set([options.controlRoot, env.RIGHTKIT_CONTROL_ROOT, options.dataRoot, env.RIGHTKIT_DATA_ROOT, env.RIGHTKIT_MANAGED_ROOT, env.RIGHTKIT_EXTERNAL_ROOT, options.clientRoot].filter(Boolean))];
  }
  const roots = resolveManagedRoots(options);
  return [...new Set([roots.controlRoot, roots.dataRoot, options.clientRoot, roots.legacyRoot].filter(Boolean))];
}

export function discoverBrokerSocket(env = process.env, { exists = existsSync, ...options } = {}) {
  // The client reads RUSTRIGHT_BUILD_BROKER_SOCKET (rustright-cli client.rs); the RIGHTKIT_ spelling is the legacy one, read here, never set.
  for (const name of ['RUSTRIGHT_BUILD_BROKER_SOCKET', 'RIGHTKIT_BUILD_BROKER_SOCKET']) if (typeof env[name] === 'string' && env[name]) return env[name];
  if ((options.platform ?? process.platform) === 'win32') return undefined;
  for (const root of managedControlCandidates({ ...options, env })) {
    const socket = posix.join(root, 'broker.sock');
    if (exists(socket)) return socket;
  }
  return undefined;
}
