// rightkit#1450 (Adrian, 2026-11-02: "can't we run every new crate with a helper, see exactly what it does, where it builds, ONCE and then add it
// to the list?" ... "yes, record it and implment it asap"). Replaces a per-job sandbox (rustright#197).
//
// A build-helper unit is a crates.io package that runs code at build time: a build script (`custom-build` target) or a proc macro. It is keyed
// exactly as cargo keys the unit it compiles: crate + version + checksum (Cargo.lock) + enabled features + target triple + toolchain. Class A
// keeps a HOST list of traced units. A unit not on the list was never seen doing its build-time work on this host: Class A refuses it, naming
// it, until it is traced once (built alone with every file write recorded: macOS fs_usage, Windows ETW). A trace passes when every write lies
// inside the unit's OUT_DIR, the build's target directory, CARGO_HOME or the temp directory; a pass is recorded and never traced again; a
// fail is recorded with the paths it wrote and keeps refusing. Any key change (a new version, checksum, feature set, target or toolchain) is a
// new unit and traces once more. Limit (accepted in #1450): a trace proves what the helper did with those inputs, not what a deliberately
// malicious crate could do later.
//
// Tracing needs admin rights on the host (fs_usage / ETW); enabling that is Adrian's host step. Everything here is pure except the list I/O.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

export const BUILD_HELPER_LIST_SCHEMA = "rightkit.build-helper-trace/1";
export const BUILD_HELPER_UNTRACED = "RK_BUILD_HELPER_UNTRACED";
export const BUILD_HELPER_WROTE_OUTSIDE = "RK_BUILD_HELPER_WROTE_OUTSIDE";
const CRATES_IO = new Set(["registry+https://github.com/rust-lang/crates.io-index", "sparse+https://index.crates.io/"]);

const sha256 = (text) => createHash("sha256").update(text).digest("hex");

/** The unit key: sha256 of the canonical {crate, version, checksum, features (sorted), target, toolchain}. */
export function buildHelperKey({ crate, version, checksum, features = [], target, toolchain }) {
  return sha256(JSON.stringify({ crate, version, checksum, features: [...features].sort(), target, toolchain }));
}

/** Checksums of crates.io packages from a Cargo.lock text: Map "name version" -> checksum. */
export function lockChecksums(lockText) {
  const out = new Map();
  for (const block of String(lockText).split(/\n\[\[package\]\]\n/)) {
    const name = /\nname = "([^"]+)"/.exec(`\n${block}`)?.[1];
    const version = /\nversion = "([^"]+)"/.exec(`\n${block}`)?.[1];
    const checksum = /\nchecksum = "([0-9a-f]{64})"/.exec(`\n${block}`)?.[1];
    if (name && version && checksum) out.set(`${name} ${version}`, checksum);
  }
  return out;
}

/**
 * Every build-helper unit of a workspace, from `cargo metadata --format-version 1` (with `resolve`) and its Cargo.lock: crates.io packages
 * in the resolved graph that have a `custom-build` target or are a proc macro, with the features cargo enabled for them.
 */
export function buildHelperUnits(metadata, lockText, { target, toolchain }) {
  if (typeof target !== "string" || !target || typeof toolchain !== "string" || !toolchain) throw new TypeError("buildHelperUnits: target and toolchain are required");
  const checksums = lockChecksums(lockText);
  const features = new Map((metadata?.resolve?.nodes ?? []).map((n) => [n.id, Array.isArray(n.features) ? n.features : []]));
  const inGraph = new Set((metadata?.resolve?.nodes ?? []).map((n) => n.id));
  const units = [];
  for (const pkg of metadata?.packages ?? []) {
    if (!CRATES_IO.has(pkg.source) || !inGraph.has(pkg.id)) continue;
    const kinds = new Set((pkg.targets ?? []).flatMap((t) => t.kind ?? []));
    const role = kinds.has("custom-build") ? "build-script" : kinds.has("proc-macro") ? "proc-macro" : null;
    if (!role) continue;
    const checksum = checksums.get(`${pkg.name} ${pkg.version}`) ?? null;
    const unit = { crate: pkg.name, version: pkg.version, checksum, features: [...(features.get(pkg.id) ?? [])].sort(), target, toolchain, role };
    units.push({ ...unit, key: buildHelperKey(unit) });
  }
  return units.sort((a, b) => (a.crate === b.crate ? (a.version < b.version ? -1 : 1) : a.crate < b.crate ? -1 : 1));
}

/** The host list file: <managed data root>/build-helper-trace/<platform>.json. */
export const hostListPath = (dataRoot, platform = process.platform) => path.join(dataRoot, "build-helper-trace", `${platform}.json`);

/** The host list ({ schema, units: { [key]: entry } }); an absent file is an empty list, a malformed one throws. */
export function readHostList(file, { readFile = (f) => fs.readFileSync(f, "utf8"), exists = fs.existsSync } = {}) {
  if (!exists(file)) return { schema: BUILD_HELPER_LIST_SCHEMA, units: {} };
  const doc = JSON.parse(readFile(file));
  if (doc?.schema !== BUILD_HELPER_LIST_SCHEMA || !doc.units || typeof doc.units !== "object") throw new Error(`${file} is not a ${BUILD_HELPER_LIST_SCHEMA} list`);
  return doc;
}

/** Record one trace verdict in the list (atomic write). */
export function recordTrace(file, unit, verdict, { now = () => new Date().toISOString(), fsApi = fs } = {}) {
  const list = readHostList(file, { readFile: (f) => fsApi.readFileSync(f, "utf8"), exists: (f) => fsApi.existsSync(f) });
  list.units[unit.key] = { crate: unit.crate, version: unit.version, checksum: unit.checksum, features: unit.features, target: unit.target, toolchain: unit.toolchain, role: unit.role, verdict: verdict.ok ? "pass" : "fail", outside: verdict.outside ?? [], tracedAt: now() };
  fsApi.mkdirSync(path.dirname(file), { recursive: true });
  const tmp = `${file}.tmp-${process.pid}`;
  fsApi.writeFileSync(tmp, `${JSON.stringify(list, null, 2)}\n`);
  fsApi.renameSync(tmp, file);
  return list;
}

/**
 * Class A's verdict: `{ ok, untraced: [unit], failed: [{unit, outside}] }`. A unit with no checksum (not from crates.io's lock) is untraced by
 * definition. Nothing listed as a pass under the same key ever runs a trace again.
 */
export function checkBuildHelpers(units, list) {
  const untraced = []; const failed = [];
  for (const u of units) {
    const entry = list?.units?.[u.key];
    if (!entry || !u.checksum) untraced.push(u);
    else if (entry.verdict !== "pass") failed.push({ unit: u, outside: entry.outside ?? [] });
  }
  return { ok: untraced.length === 0 && failed.length === 0, untraced, failed };
}

/** The refusal text Class A prints for a failing verdict, naming every unit (and the paths a failed one wrote). */
export function buildHelperRefusal(verdict) {
  const lines = [];
  for (const u of verdict.untraced) lines.push(`${BUILD_HELPER_UNTRACED}: ${u.crate} ${u.version} (${u.role}; features ${u.features.join(",") || "none"}; ${u.target}; ${u.toolchain}) has never been traced on this host; trace it once (rightkit-gate trace-build-helpers)`);
  for (const f of verdict.failed) lines.push(`${BUILD_HELPER_WROTE_OUTSIDE}: ${f.unit.crate} ${f.unit.version} wrote outside its build directories when traced: ${f.outside.slice(0, 10).join(", ")}${f.outside.length > 10 ? ` (+${f.outside.length - 10} more)` : ""}`);
  return lines;
}

/**
 * Judge one trace: every written path must lie inside one of the allowed roots (the unit's OUT_DIR, the build's target directory,
 * CARGO_HOME, the temp directory). Returns `{ ok, outside }` with the offending paths, sorted and unique.
 */
export function judgeTrace(writes, { allowed, platform = process.platform }) {
  const mod = platform === "win32" ? path.win32 : path.posix;
  const norm = (p) => { const r = mod.resolve(p); return platform === "win32" ? r.toLowerCase() : r; };
  const roots = (allowed ?? []).filter(Boolean).map(norm);
  const inside = (p) => roots.some((r) => { const rel = mod.relative(r, p); return rel === "" || (!rel.startsWith("..") && !mod.isAbsolute(rel)); });
  const outside = [...new Set(writes.map(norm).filter((p) => !inside(p)))].sort();
  return { ok: outside.length === 0, outside };
}

/**
 * The written paths of macOS `fs_usage -w -f filesys <pid...>` output: lines whose call creates, writes, renames, links or removes a file,
 * the path being the last absolute path on the line. Reads are ignored.
 */
export function parseFsUsageWrites(text) {
  // fs_usage prints an open's mode as a flag group, e.g. "(R_____)" read, "(_W____)" write, "(_WC___)" create, "(_W_T__)" truncate.
  const OPEN = /\bopen[a-z_]*\b[^(]*\(([A-Z_]{4,})\)/;
  const MUTATING = /\b(creat|write|pwrite|writev|rename|renameat|renamex_np|link|linkat|symlink|unlink|unlinkat|mkdir|mkdirat|rmdir|truncate|ftruncate|clonefile|clonefileat|copyfile|exchangedata|setattrlist|chmod|fchmod|chown|fchown|utimes|futimes)\b/;
  const out = new Set();
  for (const line of String(text).split(/\r?\n/)) {
    const open = OPEN.exec(line);
    const writes = open ? /[WCTA]/.test(open[1]) : MUTATING.test(line);
    if (!writes) continue;
    const paths = line.match(/\/[^\s]+/g);
    if (paths?.length) out.add(paths.at(-1));
  }
  return [...out];
}
