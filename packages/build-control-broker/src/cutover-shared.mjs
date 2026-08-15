// Genuinely OS-agnostic pieces of the cutover contract, shared by both
// runtime-cutover-macos-cli.mjs and runtime-cutover-windows-cli.mjs. The
// service-control layer (launchctl vs. schtasks) differs by OS; drain proof,
// path validation, and the managed-database lookup do not.
import { existsSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { readFile } from 'node:fs/promises';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { stripBom } from './config.mjs';

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
export function targetAbsent(target, processKill = process.kill) {
  try { processKill(target, 0); return false; }
  catch (error) { return error?.code === 'ESRCH'; }
}

const terminalV1States = new Set(['RELEASED', 'FAILED', 'CANCELLED']);

export async function proveV1Drained(statePath, { readFile: load = readFile } = {}) {
  let state;
  try { state = JSON.parse(stripBom(await load(statePath, 'utf8'))); }
  catch (cause) { throw new Error(`cannot prove v1 drain: ${cause.message}`, { cause }); }
  if (!state || typeof state !== 'object' || Array.isArray(state) || !Array.isArray(state.entries)) throw new Error('cannot prove v1 drain: state lacks entries array');
  const open = state.entries.filter((entry) => !entry || !terminalV1States.has(entry.state));
  if (open.length) throw new Error(`cannot prove v1 drain: ${open.length} non-terminal request(s) remain`);
  return Object.freeze({ statePath, entries: state.entries.length, open: 0 });
}

export function managedDatabasePath(config) {
  const configured = config?.broker?.databasePath;
  if (typeof configured === 'string' && configured) return resolve(configured);
  const stateFile = config?.broker?.stateFile;
  if (typeof stateFile !== 'string' || !stateFile) throw new TypeError('broker databasePath or stateFile is required to prove drain');
  return resolve(join(dirname(stateFile), `${stateFile.split(/[\\/]/).pop()}.sqlite`));
}

// Every request state that still owns a process or a queue slot.
const managedOpenStates = Object.freeze(['SUBMITTED', 'CLASSIFIED', 'QUEUED', 'ADMITTED', 'STARTING', 'RUNNING', 'REAPING']);

/**
 * Live v2 admission drain.
 *
 * `proveV1Drained` reads only the legacy state file, so on its own an
 * activation will stop the service while a managed build is mid-compile,
 * kill it, and still report a clean drain. This inspects the durable v2
 * store that actually owns running requests, and must run before the
 * service is stopped — the exact fix applied after an earlier cutover did
 * kill a mid-compile build by checking in the wrong order.
 */
export function proveManagedRequestsDrained(databasePath, { openDatabase = (path) => new DatabaseSync(path, { readOnly: true, allowExtension: false }), exists = existsSync } = {}) {
  // A host that has never run the v2 broker has nothing in flight.
  if (!exists(databasePath)) return Object.freeze({ databasePath, open: 0, present: false, inFlight: Object.freeze([]) });
  let database;
  try { database = openDatabase(databasePath); }
  catch (cause) { throw new Error(`cannot prove managed request drain: ${cause.message}`, { cause }); }
  try {
    const placeholders = managedOpenStates.map(() => '?').join(',');
    const rows = database.prepare(`select id, state from requests where state in (${placeholders})`).all(...managedOpenStates);
    if (rows.length) throw new Error(`cannot activate while ${rows.length} managed request(s) are in flight: ${rows.map((row) => `${row.id}=${row.state}`).join(', ')}`);
    return Object.freeze({ databasePath, open: 0, present: true, inFlight: Object.freeze([]) });
  } catch (error) {
    // A store predating the requests table cannot be holding a v2 build.
    if (/no such table/i.test(error.message)) return Object.freeze({ databasePath, open: 0, present: true, inFlight: Object.freeze([]) });
    throw error;
  } finally { try { database.close(); } catch { /* closing a read-only handle cannot lose state */ } }
}
