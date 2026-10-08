#!/usr/bin/env node
import { chmod, cp, lstat, mkdir, readFile, readlink, readdir, realpath, symlink, unlink, writeFile } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { homedir } from 'node:os';
import { resolveManagedRoots } from '../../release/managed-roots.mjs';
import { renderPlist } from './bootstrap-macos-cli.mjs';
import { isDirectMain } from './bootstrap-windows-cli.mjs';

const CONTROL_ENTRIES = ['config.json', 'state.db', 'state.db-wal', 'state.db-shm', 'agent-policy', 'ownership-install', 'signing-agent', 'state', 'logs'];
const within = (root, path) => { const tail = relative(root, path); return tail === '' || (tail !== '..' && !tail.startsWith(`..${sep}`) && !isAbsolute(tail)); };

/** Preserve operator fields; rebase only control paths, never bulk build data. */
export function relocateControlConfig(config, { controlRoot, dataRoot, nodePath = config.nodePath ?? process.execPath }) {
  const oldRoot = resolve(config.broker.externalRoot.path);
  const oldControl = config.broker.controlRoot && resolve(config.broker.controlRoot);
  const rebase = (value) => {
    if (Array.isArray(value)) return value.map(rebase);
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, rebase(item)]));
    if (typeof value !== 'string' || !isAbsolute(value)) return value;
    if (oldControl && oldControl !== oldRoot && within(oldControl, value)) return join(controlRoot, relative(oldControl, value));
    if (!within(oldRoot, value)) return value;
    const tail = relative(oldRoot, value);
    const first = tail.split(sep)[0];
    const control = /^(?:runtime-|agent-policy$|ownership-install$|signing-agent$|state(?:\.db(?:-wal|-shm)?)?$|logs$|broker\.sock$|config\.json$)/.test(first);
    return join(control ? controlRoot : dataRoot, tail);
  };
  const next = rebase(config);
  next.nodePath = nodePath;
  next.broker.controlRoot = controlRoot;
  next.broker.externalRoot.path = dataRoot;
  const stateRoot = oldControl ?? oldRoot;
  if (!within(stateRoot, config.broker.stateFile)) throw new TypeError('stateFile must be contained by prior control root');
  next.broker.stateFile = join(controlRoot, relative(stateRoot, config.broker.stateFile));
  next.broker.activationFencePath = config.broker.activationFencePath && within(stateRoot, config.broker.activationFencePath)
    ? join(controlRoot, relative(stateRoot, config.broker.activationFencePath)) : join(controlRoot, 'state', 'runtime-activation.json');
  next.unixSocket = join(controlRoot, 'broker.sock');
  return next;
}

export function parseControlMigrationArgs(argv) {
  const options = { apply: false };
  for (let i = 0; i < argv.length; i++) {
    const flag = argv[i];
    if (flag === '--apply') { options.apply = true; continue; }
    if (flag === '--dry-run') { options.apply = false; continue; }
    const key = { '--old-root': 'oldRoot', '--control-root': 'controlRoot', '--data-root': 'dataRoot', '--node': 'nodePath' }[flag];
    if (!key || !argv[i + 1] || argv[i + 1].startsWith('--')) throw new TypeError(`invalid migration argument: ${flag}`);
    const value = argv[++i]; if (!isAbsolute(value)) throw new TypeError(`${flag} must be absolute`); options[key] = value;
  }
  return options;
}

/** Copies only; no launchctl, plist writes, runtime activation or data movement. */
export async function migrateControlPlane({ oldRoot, controlRoot, dataRoot, nodePath, home = homedir(), apply = false } = {}) {
  const roots = resolveManagedRoots({ platform: 'darwin', home, controlRoot, dataRoot });
  oldRoot = await realpath(resolve(oldRoot ?? roots.dataRoot)); controlRoot = resolve(roots.controlRoot); dataRoot = resolve(roots.dataRoot);
  if (within(oldRoot, controlRoot) || within(controlRoot, oldRoot)) throw new TypeError('control root must be separate from legacy root');
  const config = JSON.parse((await readFile(join(oldRoot, 'config.json'), 'utf8')).replace(/^\uFEFF/, ''));
  const next = relocateControlConfig(config, { controlRoot, dataRoot, nodePath: nodePath ?? config.nodePath ?? process.execPath });
  const current = await realpath(join(oldRoot, 'runtime-current'));
  if (!within(oldRoot, current) || !basename(current).startsWith('runtime-') || !(await lstat(current)).isDirectory()) throw new Error('runtime-current must select a runtime directory below legacy root');
  const links = [];
  const scan = async (directory) => {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await scan(path);
      else if (entry.isSymbolicLink()) {
        const target = await readlink(path);
        // Canonicalize so /var vs /private/var style aliases cannot mask or fake an escape.
        const lexical = resolve(dirname(path), target);
        const resolved = await realpath(lexical).catch(() => lexical);
        if (!within(current, resolved)) throw new Error(`runtime link escapes copied runtime: ${path}`);
        if (isAbsolute(target)) links.push({ path: relative(current, path), target: relative(current, resolved) });
      }
    }
  };
  await scan(current);
  const entries = [basename(current)];
  for (const name of CONTROL_ENTRIES) {
    const info = await lstat(join(oldRoot, name)).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (info?.isSymbolicLink()) throw new Error(`control entry must not be a symlink: ${name}`);
    if (info) entries.push(name);
  }
  // The signing agent stays at its internal Orthic Labs path (resolveManagedRoots().signingAgent).
  const copies = entries.filter((name) => name !== 'signing-agent').map((name) => ({ from: join(oldRoot, name), to: join(controlRoot, name) }));
  const plist = renderPlist({ nodePath: next.nodePath, brokerScript: join(controlRoot, 'runtime-current', 'build-control-broker', 'src', 'broker.mjs'), configPath: roots.configPath, home });
  if (apply) {
    // Migration expects an inactive destination. Never merge or clobber state.
    for (const name of [...entries, 'runtime-current']) {
      if (await lstat(join(controlRoot, name)).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error))) throw new Error(`migration destination already exists: ${join(controlRoot, name)}`);
    }
    const priorRoot = await lstat(controlRoot).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (priorRoot && (!priorRoot.isDirectory() || priorRoot.isSymbolicLink() || (process.getuid && priorRoot.uid !== process.getuid()))) throw new Error('migration control root must be an owned non-symlink directory');
    await mkdir(controlRoot, { recursive: true, mode: 0o700 });
    await chmod(controlRoot, 0o700);
    for (const copy of copies) {
      if (basename(copy.from) === 'config.json') continue;
      await cp(copy.from, copy.to, { recursive: true, errorOnExist: true, force: false, verbatimSymlinks: true, filter: async (source) => { const info = await lstat(source); return !info.isSocket() && !info.isFIFO(); } });
    }
    for (const link of links) {
      const path = join(controlRoot, basename(current), link.path);
      await unlink(path);
      await symlink(relative(dirname(path), join(controlRoot, basename(current), link.target)), path);
    }
    await writeFile(join(controlRoot, 'config.json'), `${JSON.stringify(next, null, 2)}\n`, { mode: 0o600, flag: 'wx' });
    await symlink(basename(current), join(controlRoot, 'runtime-current'));
  }
  return { dryRun: !apply, controlRoot, dataRoot, copies, runtimeCurrent: basename(current), config: next, plist };
}

if (isDirectMain({ moduleUrl: import.meta.url })) {
  migrateControlPlane(parseControlMigrationArgs(process.argv.slice(2))).then((plan) => {
    const { config, plist, ...summary } = plan;
    process.stdout.write(`${JSON.stringify(summary, null, 2)}\n${plist}`);
  }).catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; });
}
