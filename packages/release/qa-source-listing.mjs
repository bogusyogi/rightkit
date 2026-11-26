// The committed file listing a QA journeys run reads when its root is the door's git-less snapshot.
//
// `rightkit-qa journeys run` binds its build record and the scenario files it reads to committed source. In a checkout it asks git; in
// the door's snapshot there is no git, and `.rightkit-source-identity.json` names only the commit and tree ids, which cannot prove one
// file. So the qualify stage writes, OUTSIDE the snapshot (a file inside it would be an extra the snapshot verify refuses), the exact
// list of the commit's committed files with the sha256 of their committed bytes, and hands its path to the runner as
// RIGHTKIT_QA_SOURCE_LISTING. The runner treats a file as committed only when it is listed and its bytes match; anything else fails
// closed. The listing is computed from the checkout's object database for the selected commit (never from the snapshot's files).
import { createHash } from "node:crypto";
import { mkdirSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";

export const QA_SOURCE_LISTING_ENV = "RIGHTKIT_QA_SOURCE_LISTING";
export const QA_SOURCE_LISTING_SCHEMA = "rightkit.qa-source-listing/1";

const HEX40 = /^[0-9a-f]{40}$/;

function gitBytes(git, cwd, args, input) {
  const r = git("git", ["-C", cwd, ...args], { input, maxBuffer: 1024 * 1024 * 1024, windowsHide: true });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${String(r.stderr ?? "").trim().slice(0, 400)}`);
  return Buffer.isBuffer(r.stdout) ? r.stdout : Buffer.from(String(r.stdout ?? ""), "utf8");
}

/**
 * `{ path: sha256 }` for every regular file (mode 100644 or 100755) of `commit`'s tree, read from the object database of `checkout`.
 * Symlinks and submodules are not regular committed files and are left out, so the runner never counts them as committed source.
 */
export function committedFileDigests({ checkout, commit, git = spawnSync }) {
  if (!HEX40.test(String(commit))) throw new Error(`a QA source listing needs a full commit id, got ${commit}`);
  const listed = gitBytes(git, checkout, ["ls-tree", "-r", "-z", "--full-tree", commit]).toString("utf8").split("\0").filter(Boolean);
  const entries = [];
  for (const row of listed) {
    const tab = row.indexOf("\t");
    const [mode, type, oid] = row.slice(0, tab).split(" ");
    if (type === "blob" && (mode === "100644" || mode === "100755")) entries.push({ path: row.slice(tab + 1), oid });
  }
  const files = {};
  if (!entries.length) return files;
  const out = gitBytes(git, checkout, ["cat-file", "--batch"], `${entries.map((e) => e.oid).join("\n")}\n`);
  let at = 0;
  for (const entry of entries) {
    const nl = out.indexOf(0x0a, at);
    if (nl < 0) throw new Error(`git cat-file ended before ${entry.path}`);
    const [oid, type, size] = out.subarray(at, nl).toString("utf8").split(" ");
    if (oid !== entry.oid || type !== "blob") throw new Error(`git cat-file answered ${oid} ${type} for ${entry.path} (${entry.oid})`);
    const start = nl + 1;
    const end = start + Number(size);
    files[entry.path] = createHash("sha256").update(out.subarray(start, end)).digest("hex");
    at = end + 1;
  }
  return files;
}

/**
 * Writes the listing for a door source split and returns its path. `source` is release-source-identity's snapshot source
 * (`commit`, `tree`, `checkout`, `snapshot`); `dir` is a directory outside the snapshot.
 */
export function writeQaSourceListing({ source, dir, git = spawnSync }) {
  if (!source?.snapshot || !source?.checkout) throw new Error("a QA source listing is written only for a door source split");
  const files = committedFileDigests({ checkout: source.checkout, commit: source.commit, git });
  const body = { schema: QA_SOURCE_LISTING_SCHEMA, commit: source.commit, tree: source.tree, root: path.resolve(source.snapshot), files };
  mkdirSync(dir, { recursive: true });
  const file = path.join(dir, `${source.commit}.json`);
  const tmp = `${file}.${process.pid}.tmp`;
  writeFileSync(tmp, `${JSON.stringify(body)}\n`);
  renameSync(tmp, file);
  return file;
}
