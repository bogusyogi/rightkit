// Publishing leaves no copies behind (rightkit#763). Adrian, 2026-10-18: "trash them but we also need rightkit to not make those copies in the first place,
// kill the problem at the source". After a publish whose remote bytes were verified, `retainAfterPublish` reduces what right-release keeps under
// `<repo>/.right-release/{sealed,state}` to a bounded set, and refuses loudly when what is left is still too big:
//
//   the published release   kept whole (a release is published once per tier, patch and update, from the same sealed folder, so the second tier still needs
//                           its bytes); with `reducePublished: true` (a release known to be fully published) its sealed folder keeps only small evidence
//                           (every file at or under SMALL_FILE_BYTES) and every large file (installers, DMGs, zips, bundles) is deleted, because the verified
//                           remote objects are the copy of record. Its state folder (the verified marker, the receipts) is always kept whole.
//   other releases          the newest `maxReleases - 1` others (by modification time) are kept, and nothing older than `maxAgeDays`; the rest are deleted whole.
//   a repository copy       a `.git` anywhere in the first two levels of a release folder means a copy of a whole repository was made per release (the retired
//                           v*/w* flow, ~72 GB): that is refused, never cleaned silently.
//   size                    if, after all of that, the two folders hold more than `maxBytes`, the run fails and names the largest entries.
// Nothing here copies, moves or uploads anything; it only deletes under the two folders. Defaults: 3 releases, 14 days, 2 GiB; the environment
// (RIGHT_RELEASE_RETAIN_RELEASES, RIGHT_RELEASE_RETAIN_DAYS, RIGHT_RELEASE_RETAIN_MAX_BYTES) can only TIGHTEN them (a larger value is refused).
import { existsSync, lstatSync, readdirSync, rmSync, statSync } from "node:fs";
import path from "node:path";

export const RETENTION_DEFAULTS = Object.freeze({ maxReleases: 3, maxAgeDays: 14, maxBytes: 2 * 1024 ** 3 });
export const SMALL_FILE_BYTES = 1024 * 1024;
const DAY_MS = 24 * 60 * 60 * 1000;
const FOLDERS = ["sealed", "state"];

export class RetentionError extends Error {
  constructor(message, details = {}) { super(message); this.name = "RetentionError"; this.code = "RK_PUBLISH_RETENTION"; Object.assign(this, details); }
}
const fail = (message, details) => { throw new RetentionError(message, details); };

/** The limits in force: the defaults, tightened (never loosened) by the environment. */
export function retentionLimits(env = process.env) {
  const read = (name, fallback) => {
    const raw = env[name];
    if (raw === undefined || raw === "") return fallback;
    const value = Number(raw);
    if (!Number.isInteger(value) || value < 1) fail(`${name} must be a positive integer`);
    if (value > fallback) fail(`${name}=${value} is above the limit ${fallback}: the environment can tighten the retention bound, never loosen it`);
    return value;
  };
  return {
    maxReleases: read("RIGHT_RELEASE_RETAIN_RELEASES", RETENTION_DEFAULTS.maxReleases),
    maxAgeDays: read("RIGHT_RELEASE_RETAIN_DAYS", RETENTION_DEFAULTS.maxAgeDays),
    maxBytes: read("RIGHT_RELEASE_RETAIN_MAX_BYTES", RETENTION_DEFAULTS.maxBytes),
  };
}

/** Total bytes of regular files under `target` (symlinks are counted as their own size and never followed). */
export function treeBytes(target, fs = { lstatSync, readdirSync }) {
  const info = fs.lstatSync(target);
  if (!info.isDirectory()) return info.size;
  return fs.readdirSync(target).reduce((sum, name) => sum + treeBytes(path.join(target, name), fs), 0);
}

function filesUnder(dir, fs, prefix = "") {
  return fs.readdirSync(path.join(dir, prefix)).flatMap((name) => {
    const relative = prefix ? `${prefix}/${name}` : name;
    const info = fs.lstatSync(path.join(dir, relative));
    return info.isDirectory() ? filesUnder(dir, fs, relative) : [{ relative, size: info.size }];
  });
}

function assertNoRepositoryCopy(dir, fs, depth = 2) {
  if (depth < 0) return;
  for (const name of fs.readdirSync(dir)) {
    if (name === ".git") fail(`${dir} holds a .git: a copy of a whole repository was made for this release. That is the retired per-release copy flow; remove it by hand and find what made it (rightkit#763)`, { dir });
    const full = path.join(dir, name);
    if (fs.lstatSync(full).isDirectory()) assertNoRepositoryCopy(full, fs, depth - 1);
  }
}

/**
 * Apply the retention. `keep` is { releaseId } of the release just published (its folders are reduced, never removed whole).
 * Returns { removed: [paths], keptReleases: [ids], bytes }. `dryRun` deletes nothing and returns what it would. Throws RetentionError on a repository copy or on size.
 */
export function retainAfterPublish({ root, keep, limits = retentionLimits(), dryRun = false, reducePublished = false, now = Date.now(), fs = { existsSync, lstatSync, readdirSync, rmSync, statSync }, log = () => {} }) {
  if (typeof keep?.releaseId !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._+-]{0,159}$/.test(keep.releaseId)) fail("retainAfterPublish needs the id of the release just published");
  const base = path.join(path.resolve(root), ".right-release");
  const removed = [];
  const drop = (target) => { removed.push(target); if (!dryRun) fs.rmSync(target, { recursive: true, force: true }); };
  const releasesOf = (folder) => (fs.existsSync(path.join(base, folder)) ? fs.readdirSync(path.join(base, folder)).filter((name) => fs.lstatSync(path.join(base, folder, name)).isDirectory()) : []);
  for (const folder of FOLDERS) for (const id of releasesOf(folder)) assertNoRepositoryCopy(path.join(base, folder, id), fs);

  // The published release: sealed keeps small evidence only.
  const sealedKeep = path.join(base, "sealed", keep.releaseId);
  if (reducePublished && fs.existsSync(sealedKeep)) {
    for (const file of filesUnder(sealedKeep, fs)) if (file.size > SMALL_FILE_BYTES) drop(path.join(sealedKeep, ...file.relative.split("/")));
  }

  // Every other release: newest (maxReleases - 1) and younger than maxAgeDays survive.
  const ids = [...new Set(FOLDERS.flatMap(releasesOf))].filter((id) => id !== keep.releaseId);
  const modified = (id) => Math.max(...FOLDERS.filter((folder) => fs.existsSync(path.join(base, folder, id))).map((folder) => fs.statSync(path.join(base, folder, id)).mtimeMs));
  const ordered = ids.map((id) => ({ id, at: modified(id) })).sort((a, b) => b.at - a.at);
  const kept = [];
  for (const [index, item] of ordered.entries()) {
    const old = now - item.at > limits.maxAgeDays * DAY_MS;
    if (index >= limits.maxReleases - 1 || old) for (const folder of FOLDERS) { const target = path.join(base, folder, item.id); if (fs.existsSync(target)) drop(target); }
    else kept.push(item.id);
  }

  // Size of what is left (a dry run subtracts what it would have removed only by not counting it: it measures the tree as it is, then says so).
  const bytes = FOLDERS.reduce((sum, folder) => (fs.existsSync(path.join(base, folder)) ? sum + treeBytes(path.join(base, folder), fs) : sum), 0);
  if (!dryRun && bytes > limits.maxBytes) {
    const largest = FOLDERS.flatMap((folder) => releasesOf(folder).map((id) => ({ path: path.join(base, folder, id), bytes: treeBytes(path.join(base, folder, id), fs) }))).sort((a, b) => b.bytes - a.bytes).slice(0, 5);
    fail(`after publishing ${keep.releaseId}, ${base} still holds ${bytes} bytes (limit ${limits.maxBytes}). Largest: ${largest.map((item) => `${item.path} (${item.bytes})`).join(", ")}`, { bytes, largest });
  }
  log(`right-release: retention ${dryRun ? "would remove" : "removed"} ${removed.length} path(s); ${kept.length + 1} release(s) kept; ${bytes} bytes left under ${base}`);
  return { removed, keptReleases: [keep.releaseId, ...kept], bytes };
}
