// OS-agnostic cutover contract. Service control differs by OS; active-root
// proof and path validation do not.
import { readFile, readdir } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve } from 'node:path';

// Deliberately narrower than config.mjs's absolute(): this only ever runs on
// the same host it validates paths for (a cutover CLI's own argv), where
// node:path is already that host's semantics — unlike config.mjs, which
// validates a config that may describe a different OS's paths than whatever
// host is currently reading it, and so must recognize Windows drive-letter
// paths even when running on POSIX.
export function absoluteHostPath(value, name) {
  if (typeof value !== 'string' || !isAbsolute(value)) throw new TypeError(`--${name} must be an absolute path`);
  return resolve(value);
}

export async function assertFile(path, label, statFile) {
  const metadata = await statFile(path);
  if (!metadata.isFile()) throw new TypeError(`${label} must be a regular file`);
}
export async function assertDirectory(path, label, statFile) {
  const metadata = await statFile(path);
  if (!metadata.isDirectory()) throw new TypeError(`${label} must be a directory`);
}

export function contained(path, root) { const value = relative(root, path); return value === '' || (!value.startsWith('..') && !isAbsolute(value)); }

/** Node's zero-signal existence probe is portable: on Windows it maps to
 * OpenProcess + a liveness check internally, no real signal delivered, same
 * ESRCH-on-absent contract as POSIX. */
/** Prove no thin root owns a process before service replacement. */
export async function proveThinRequestsDrained(externalRoot, { list = readdir, load = readFile } = {}) {
  const activeDirectory = join(resolve(externalRoot), 'rightkit-build-control', 'global', 'active');
  let entries;
  try { entries = await list(activeDirectory, { withFileTypes: true }); }
  catch (error) {
    if (error?.code === 'ENOENT') return Object.freeze({ activeDirectory, open: 0, present: false, inFlight: Object.freeze([]) });
    throw new Error(`cannot prove thin request drain: ${error.message}`, { cause: error });
  }
  const inFlight = [];
  for (const entry of entries) {
    const name = typeof entry === 'string' ? entry : entry.name;
    const file = typeof entry === 'string' || entry.isFile() ? name : null;
    if (!file?.endsWith('.json')) continue;
    let marker;
    try { marker = JSON.parse(await load(join(activeDirectory, file), 'utf8')); }
    catch { marker = null; }
    inFlight.push(Object.freeze({ id: marker?.id ?? file.slice(0, -5), workspaceKey: marker?.workspaceKey ?? null }));
  }
  if (inFlight.length) throw new Error(`cannot activate while ${inFlight.length} thin request(s) are in flight: ${inFlight.map(({ id }) => id).join(', ')}`);
  return Object.freeze({ activeDirectory, open: 0, present: true, inFlight: Object.freeze([]) });
}
