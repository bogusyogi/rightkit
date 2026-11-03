// The source-identity file of a gate snapshot (docs/GATE-CONTRACT.md GC-A23; rightkit#1504, rustright#176).
//
// A gate snapshot has no `.git`, so a build script that wants the commit it is built from (Pulse's membrane-runtime and this repository's
// rightkit-context-runtime read it) finds nothing and reports "unknown". The gate therefore writes `.rightkit-source-identity.json`
// into the snapshot root for the commit it syncs:
//   { "schema": "rightkit.source-identity.v1", "commit": "<40 hex>", "trees": { "": "<root tree id>", "<dir>": "<tree id>", ... } }
// with one `trees` entry for the root and one for every directory of the commit that holds a Cargo.toml, Package.swift or
// package.json, each the git tree id `git rev-parse <commit>:<dir>` prints, read from the checkout's object database through the sealed
// runner. The bytes are a pure function of the commit (keys sorted, fixed spacing, one trailing newline), so every sync of one commit
// writes the same file and a verify can recompute it.
//
// It is a generated file, never a commit byte: snapshot.mjs admits it as its fourth named allowance (beside the pnpm dependency
// directories and the config-only `.git/config`) only when ALL hold: it sits at the snapshot root; the commit does not track a file
// of that name; it is a regular file with one hard link; its bytes equal sourceIdentityBytes() for the snapshot's commit; and the
// commit's own ignore rules ignore it (so the seeded tree id, which the broker computes the same way, still equals `<sha>^{tree}`).
// Any other byte, a link, a second name or a missing ignore rule is an extra path and fails the snapshot as before.
import { sealedRun } from "./snapshot-git.mjs";

export const SOURCE_IDENTITY_FILE = ".rightkit-source-identity.json";
export const SOURCE_IDENTITY_SCHEMA = "rightkit.source-identity.v1";
const MARKERS = new Set(["Cargo.toml", "Package.swift", "package.json"]);
const CHUNK = 200;

/** The directories of the commit (posix, "" for the root) that hold a Cargo.toml, Package.swift or package.json, sorted. */
export function identityDirectories(entries) {
  const dirs = new Set([""]);
  for (const e of entries) {
    const i = e.path.lastIndexOf("/");
    if (MARKERS.has(i < 0 ? e.path : e.path.slice(i + 1))) dirs.add(i < 0 ? "" : e.path.slice(0, i));
  }
  return [...dirs].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
}

/** The exact bytes (a Buffer) of the identity file for `commit`: `entries` are the commit's `treeEntries`, `run` is injectable (tests). */
export function sourceIdentityBytes({ checkout, commit, entries, run = sealedRun }) {
  if (!/^[0-9a-f]{40}$/.test(String(commit))) throw new Error(`the source identity needs a 40-hex SHA-1 commit id, not ${JSON.stringify(commit)}`);
  const dirs = identityDirectories(entries);
  const trees = {};
  for (let i = 0; i < dirs.length; i += CHUNK) {
    const chunk = dirs.slice(i, i + CHUNK);
    const r = run(checkout, ["rev-parse", ...chunk.map((d) => (d === "" ? `${commit}^{tree}` : `${commit}:${d}`))]);
    if (r.error || r.status !== 0) throw new Error(`git rev-parse of the commit's trees failed: ${r.error ? r.error.message : String(r.stderr ?? "").trim() || `exit ${r.status}`}`);
    const ids = String(r.stdout ?? "").split("\n").filter(Boolean);
    if (ids.length !== chunk.length || !ids.every((id) => /^[0-9a-f]{40}$/.test(id))) throw new Error(`git rev-parse answered ${ids.length} tree ids for ${chunk.length} directories`);
    chunk.forEach((d, k) => { trees[d] = ids[k]; });
  }
  return Buffer.from(`${JSON.stringify({ schema: SOURCE_IDENTITY_SCHEMA, commit, trees }, null, 2)}\n`, "utf8");
}
