// Which paths of a snapshot changed since its tree was last proven, and what that leaves to prove (GATE-CONTRACT NN11).
//
// NN11: a run hashes the snapshot's tree at most once. Until 2026-11-19 every proof that was not answered "nothing changed" hashed the whole
// tree again (`git add -A`, `git write-tree`): a first run of the witness fixture hashed it 6 times for a check and 9 times for a compile,
// because each stage writes its outputs into the snapshot (installed dependencies, build outputs) and every event there dropped the proof.
//
// The open gate process (gate-serve.mjs) sees every change by name. So it keeps, per snapshot, the paths that changed since the tree was
// proven; a later proof walks the snapshot as before (the walk refuses anything that is not the commit or an admitted output) and then checks
// only the commit's files that those changes could have touched, each against the commit's own blob id. A stage that wrote only outputs touched
// none of them. The tree id is a function of the commit's files alone once the walk has passed, so it is still the proven one.
//
// What is kept is deliberately coarse in the safe direction:
//   - `changed`: paths whose content or metadata changed (the watcher's "change"). A commit file is checked when it IS such a path.
//   - `renamed`: paths that appeared, disappeared or were renamed (the watcher's "rename"). A directory renamed into place brings its whole
//     content without one event per file, so a commit file is checked when it is such a path OR lies under one.
//   - past MAX_PATHS the two sets collapse to their first two path components, kept as `renamed` (everything under them is checked); past
//     MAX_PREFIXES, or after an event without a name, nothing is known and the caller hashes the tree.
// When the files to check are ALL of the commit's files, that is a whole-tree hash by another name: the caller hashes the tree and counts it.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

export const MAX_PATHS = 5000;
export const MAX_PREFIXES = 2000;
const LINK_MODE = "120000";

const slash = (p) => String(p ?? "").split(/[\\/]+/).filter(Boolean).join("/");
const prefixOf = (rel) => rel.split("/").slice(0, 2).join("/");

/** The record the open process keeps for one snapshot since its tree was proven. `touch(type, rel)`: one watcher event, `rel` relative to the snapshot. */
export function createChangeLedger({ maxPaths = MAX_PATHS, maxPrefixes = MAX_PREFIXES } = {}) {
  let changed = new Set(); let renamed = new Set(); let collapsed = false; let unknown = false;
  const collapse = () => {
    const next = new Set();
    for (const p of [...changed, ...renamed]) next.add(prefixOf(p));
    changed = new Set(); renamed = next; collapsed = true;
    if (renamed.size > maxPrefixes) { unknown = true; renamed = new Set(); }
  };
  return {
    touch(type, rel) {
      if (unknown) return;
      const p = slash(rel);
      // The snapshot directory itself: a "change" is its own listing or metadata (each entry that changed reports under its own name); a
      // "rename" is the directory replaced, and then nothing under it is known.
      if (!p) { if (type !== "change") { unknown = true; changed = new Set(); renamed = new Set(); } return; }
      if (collapsed) { renamed.add(prefixOf(p)); if (renamed.size > maxPrefixes) { unknown = true; renamed = new Set(); } return; }
      (type === "change" ? changed : renamed).add(p);
      if (changed.size + renamed.size > maxPaths) collapse();
    },
    /** `{ changed: [...], renamed: [...] }`, or null when it is not known what changed. */
    list() { return unknown ? null : { changed: [...changed].sort(), renamed: [...renamed].sort() }; },
    get unknown() { return unknown; },
  };
}

/** The git blob id of `bytes` under `objectFormat` ("sha1" or "sha256"). */
export function blobOid(bytes, objectFormat = "sha1") {
  return createHash(objectFormat === "sha256" ? "sha256" : "sha1").update(`blob ${bytes.length}\0`).update(bytes).digest("hex");
}

/**
 * The commit's entries (`[{ path, mode, oid }]`) that the changes could have touched. On a file system that folds case (Windows, macOS) paths
 * are compared without case: the watcher names a path as it was written, the commit as it was committed.
 */
export function entriesToRecheck(entries, changes, { platform = process.platform } = {}) {
  const fold = platform === "win32" || platform === "darwin" ? (s) => s.toLowerCase() : (s) => s;
  const exact = new Set((changes?.changed ?? []).map((p) => fold(slash(p))));
  const roots = (changes?.renamed ?? []).map((p) => fold(slash(p)));
  const rootSet = new Set(roots);
  const under = (p) => { if (rootSet.has(p)) return true; let at = p.lastIndexOf("/"); while (at > 0) { if (rootSet.has(p.slice(0, at))) return true; at = p.lastIndexOf("/", at - 1); } return false; };
  return entries.filter((e) => { const p = fold(e.path); return exact.has(p) || under(p); });
}

/**
 * Whether the tree proven before still stands after `changes`: `{ ok: true, checked }` when every commit file the changes could have touched
 * still has the commit's bytes, else `{ ok: false, why }` and the caller hashes the tree (which then says exactly what differs). Never ok when
 * nothing is known (`changes` null), when every file of the commit would have to be checked, or when a touched entry is a link (the tree hash
 * judges how a link is held on this platform).
 */
export function treeStandsAfterChanges({ dir, entries, changes, objectFormat = null, platform = process.platform, read = (f) => fs.readFileSync(f), lstat = (f) => fs.lstatSync(f) }) {
  if (!changes || !Array.isArray(changes.changed) || !Array.isArray(changes.renamed)) return { ok: false, why: "what changed is not known" };
  if (!Array.isArray(entries) || entries.length === 0) return { ok: false, why: "the commit has no files to compare" };
  const named = new Set(entriesToRecheck(entries, changes, { platform }));
  // Every commit file is looked at (not read): one that is missing, or that has a second name (a hard link: pnpm links a `file:` package's
  // files into node_modules, and a write through that other name is reported under it, never under the commit's path; review 2026-11-19),
  // is compared whatever the change record says.
  const stats = new Map();
  for (const e of entries) {
    let st = null; try { st = lstat(path.join(dir, ...e.path.split("/"))); } catch { st = null; }
    stats.set(e, st);
    if (!st || st.nlink > 1) named.add(e);
  }
  const touched = entries.filter((e) => named.has(e));
  if (touched.length === entries.length) return { ok: false, why: "every file of the commit may have changed: that is a whole-tree hash" };
  for (const e of touched) {
    if (e.mode === LINK_MODE) return { ok: false, why: `${e.path} is a link` };
    const file = path.join(dir, ...e.path.split("/"));
    const st = stats.get(e);
    if (!st) { try { lstat(file); } catch (error) { return { ok: false, why: `${e.path}: ${error?.code ?? "cannot be read"}` }; } return { ok: false, why: `${e.path}: cannot be read` }; }
    if (!st.isFile() || st.isSymbolicLink?.()) return { ok: false, why: `${e.path} is not a regular file` };
    let bytes; try { bytes = read(file); } catch (error) { return { ok: false, why: `${e.path}: ${error?.code ?? "cannot be read"}` }; }
    // The id says which hash made it: 40 hex is SHA-1, 64 is SHA-256. No question to git is needed.
    if (blobOid(bytes, objectFormat ?? (String(e.oid).length === 64 ? "sha256" : "sha1")) !== e.oid) return { ok: false, why: `${e.path} is not the commit's bytes` };
  }
  return { ok: true, checked: touched.length };
}
