// The gate's per-user host configuration file (docs/GATE-CONTRACT.md §1 "Broker interface" Snapshot root: "a durable root named in
// gate configuration"; item 5 row 5.16). It is the ONE place a Linux host names its snapshot root.
//
//   File:     <home>/.config/rightkit/gate.json   (home = the account's passwd home directory)
//   Content:  { "schema": "rightkit.gate-host-config/1", "snapshotRoot": "/absolute/normalised/durable/path" }
//
// Why a file and not a variable or a flag: two concurrent gate runs on one host could otherwise choose two roots for one repository,
// and "one snapshot per repository per host" (GC §1) would be false. So nothing in the process environment or on a command line
// selects the root, and nothing here reads one: not RIGHTKIT_SNAPSHOT_ROOT (removed as a source), not XDG_CONFIG_HOME, not HOME.
// The file's own location comes from the account database (`os.userInfo().homedir`), the one home directory an environment
// variable cannot move. The contract's named default (`$XDG_STATE_HOME/rightkit/snapshots`, else `~/.local/state/rightkit/snapshots`)
// applies only when no file exists.
//
// The file is outside every repository, snapshot and scratch directory by construction (a dot directory of the account's home).
// It must be what a persistent per-user setting is, or it is refused (fail closed, never ignored, because an ignored file would
// silently pick the default root): a regular file (a link is never followed: a dotfiles link into a repository would put the
// setting inside one), owned by this user, writable by no group or other principal, in a directory owned by this user that no group or
// other principal can write; valid JSON of exactly the schema above (an unknown key is a typo that would silently pick the default);
// and a `snapshotRoot` that is an absolute, normalised POSIX path (no `..`, no `.`, no doubled or trailing separator).

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export const HOST_CONFIG_SCHEMA = "rightkit.gate-host-config/1";
export const HOST_CONFIG_DIR = path.posix.join(".config", "rightkit");
export const HOST_CONFIG_NAME = "gate.json";

export class HostConfigError extends Error {
  constructor(message) { super(message); this.code = "host-config"; }
}

/** The account's home directory from the account database, not from HOME (which any process can change); HOME only if there is no entry. */
export function accountHome() {
  try { const h = os.userInfo().homedir; if (h) return h; } catch { /* no passwd entry (a container with an arbitrary uid) */ }
  return os.homedir();
}

/** Where the gate's host configuration file lives for `home` (default: this account's). */
export function hostConfigFile(home = accountHome()) {
  return path.posix.join(home, HOST_CONFIG_DIR, HOST_CONFIG_NAME);
}

const groupOrOtherWrites = (mode) => (Number(mode) & 0o022) !== 0;

/**
 * The snapshot root the file at `file` names, or null when there is no such file. Throws HostConfigError when the file exists but
 * is not an acceptable host configuration (see the header). `getuid` is injectable for the ownership rule; where the host has no
 * uid (Windows) the ownership and mode rules do not apply, and the Windows root is the managed root's, never a file's.
 */
export function readSnapshotRootConfig(file, { getuid = process.getuid?.bind(process) } = {}) {
  let st;
  try { st = fs.lstatSync(file); } catch (e) { if (e.code === "ENOENT" || e.code === "ENOTDIR") return null; throw new HostConfigError(`gate host configuration ${file} cannot be inspected: ${e.code ?? e.message}`); }
  const refuse = (why) => new HostConfigError(`gate host configuration ${file} ${why}`);
  if (!st.isFile()) throw refuse("is not a regular file (a link is never followed: a setting reached through one could live inside a repository)");
  if (getuid) {
    const uid = getuid();
    let dirSt;
    try { dirSt = fs.lstatSync(path.dirname(file)); } catch (e) { throw refuse(`has a directory that cannot be inspected: ${e.code ?? e.message}`); }
    if (st.uid !== uid) throw refuse(`is owned by uid ${st.uid}, not by this user (uid ${uid})`);
    if (groupOrOtherWrites(st.mode)) throw refuse(`is writable by group or other (mode ${(Number(st.mode) & 0o777).toString(8)}); only this user may change where snapshots live`);
    if (!dirSt.isDirectory() || dirSt.uid !== uid || groupOrOtherWrites(dirSt.mode)) throw refuse("is in a directory that is not a real directory owned by this user and closed to group and other writes");
  }
  let doc;
  try { doc = JSON.parse(fs.readFileSync(file, "utf8")); } catch (e) { throw refuse(`cannot be read as JSON: ${e.message}`); }
  if (!doc || typeof doc !== "object" || Array.isArray(doc)) throw refuse("is not a JSON object");
  if (doc.schema !== HOST_CONFIG_SCHEMA) throw refuse(`has schema ${JSON.stringify(doc.schema)}, not ${JSON.stringify(HOST_CONFIG_SCHEMA)}`);
  const unknown = Object.keys(doc).filter((k) => k !== "schema" && k !== "snapshotRoot");
  if (unknown.length) throw refuse(`has unknown key(s) ${unknown.map((k) => JSON.stringify(k)).join(", ")} (only "schema" and "snapshotRoot" exist; a misspelled key would silently select the default root)`);
  if (doc.snapshotRoot === undefined) return null;
  const root = doc.snapshotRoot;
  if (typeof root !== "string" || !root || root.includes("\0")) throw refuse('has a "snapshotRoot" that is not a non-empty string');
  if (!path.posix.isAbsolute(root)) throw refuse(`names the relative "snapshotRoot" ${JSON.stringify(root)}; it must be absolute`);
  if (root === "/" || root.endsWith("/") || path.posix.normalize(root) !== root) throw refuse(`names the "snapshotRoot" ${JSON.stringify(root)}, which is not a normalised path below the filesystem root (no "..", ".", doubled or trailing separators)`);
  return root;
}
