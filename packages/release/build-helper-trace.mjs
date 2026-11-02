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
// No toolchain in the key (Adrian, 2026-11-03, rightkit#1473: "why would a rust upgrade void it"): a Rust upgrade never voids a trace list.
export function buildHelperKey({ crate, version, checksum, features = [], target }) {
  return sha256(JSON.stringify({ crate, version, checksum, features: [...features].sort(), target }));
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
/** Tracers whose verdicts count: each follows the build's whole process tree. fs_usage (name-filtered) verdicts recorded before do not count. */
export const TRUSTED_TRACERS = Object.freeze(["eslogger", "strace-ff", "etw"]);
export const TRACER_OF = Object.freeze({ darwin: "eslogger", linux: "strace-ff", win32: "etw" });

export function recordTrace(file, unit, verdict, { now = () => new Date().toISOString(), fsApi = fs, tracer = TRACER_OF[process.platform] } = {}) {
  const list = readHostList(file, { readFile: (f) => fsApi.readFileSync(f, "utf8"), exists: (f) => fsApi.existsSync(f) });
  list.units[unit.key] = { crate: unit.crate, version: unit.version, checksum: unit.checksum, features: unit.features, target: unit.target, toolchain: unit.toolchain, role: unit.role, verdict: verdict.ok ? "pass" : "fail", outside: verdict.outside ?? [], tracer, tracedAt: now() };
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
  // Entries recorded under the older key (which held the toolchain) are re-keyed from their own fields; a pass wins over a fail.
  const byKey = new Map();
  for (const entry of Object.values(list?.units ?? {})) {
    if (!entry || typeof entry !== "object") continue;
    const key = buildHelperKey(entry);
    const had = byKey.get(key);
    if (!had || (had.verdict !== "pass" && entry.verdict === "pass") || !TRUSTED_TRACERS.includes(had.tracer)) byKey.set(key, entry);
  }
  for (const u of units) {
    const entry = byKey.get(u.key);
    if (!entry || !u.checksum || !TRUSTED_TRACERS.includes(entry.tracer)) untraced.push(u);
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
export function judgeTrace(writes, { allowed, platform = process.platform, mustSee = null }) {
  const mod = platform === "win32" ? path.win32 : path.posix;
  const norm = (p) => { const r = mod.resolve(p); return platform === "win32" ? r.toLowerCase() : r; };
  const roots = (allowed ?? []).filter(Boolean).map(norm);
  const inside = (p) => roots.some((r) => { const rel = mod.relative(r, p); return rel === "" || (!rel.startsWith("..") && !mod.isAbsolute(rel)); });
  const outside = [...new Set(writes.map(norm).filter((p) => !inside(p)))].sort();
  // Proof the tracer saw the build: at least one write under the build's own target. A tracer that started late or lost events sees
  // nothing, and an empty trace must never pass.
  if (mustSee) {
    const root = norm(mustSee);
    const saw = writes.map(norm).some((p) => { const rel = mod.relative(root, p); return rel === "" || (!rel.startsWith("..") && !mod.isAbsolute(rel)); });
    if (!saw) return { ok: false, outside: [...outside, `(the tracer recorded no write under ${mustSee}: the build was not observed)`] };
  }
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

/**
 * The written paths of Linux `strace -f -e trace=<file calls>` output: successful calls that create, write, rename, link or remove a file.
 * An open counts when its flags include O_WRONLY, O_RDWR, O_CREAT, O_TRUNC or O_APPEND; every other listed call mutates by definition.
 * A failed call (`= -1`) wrote nothing. Every quoted path of a mutating call is taken (rename names both); a relative path resolves
 * against `cwd` (the traced cargo's directory; a build script that changes directory is resolved approximately, a recorded limit).
 */
export function parseStraceWrites(text, { cwd = "/" } = {}) {
  const OPEN = /\b(open|openat|openat2)\((?:[^"]*?)"((?:[^"\\]|\\.)*)"[^)]*?\b(O_[A-Z_|]+)/;
  const MUTATING = /\b(creat|rename|renameat|renameat2|unlink|unlinkat|mkdir|mkdirat|rmdir|link|linkat|symlink|symlinkat|truncate|chmod|fchmodat|chown|fchownat|lchown|utimensat|utimes)\(/;
  const out = new Set();
  for (const line of String(text).split(/\r?\n/)) {
    if (/=\s*-1\s/.test(line) || /<unfinished \.\.\.>/.test(line)) continue;
    let paths = [];
    const open = OPEN.exec(line);
    if (open) { if (/O_WRONLY|O_RDWR|O_CREAT|O_TRUNC|O_APPEND/.test(open[3])) paths = [open[2]]; }
    else if (MUTATING.test(line)) paths = [...line.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]);
    for (const p of paths) if (p) out.add(path.posix.isAbsolute(p) ? p : path.posix.resolve(cwd, p));
  }
  return [...out];
}

/** The repository's committed list (`.rightkit/build-helper-trace.json`, same schema): reviewed passes every host and CI reads. */
export const repoListPath = (workspaceRoot) => path.join(workspaceRoot, ".rightkit", "build-helper-trace.json");

/** The host list and the repository list together: a unit passes when either holds a pass for its key and neither holds a fail. */
export function mergeLists(...lists) {
  const units = {};
  for (const list of lists) for (const [key, entry] of Object.entries(list?.units ?? {})) {
    if (!units[key] || entry.verdict !== "pass") units[key] = entry;
  }
  return { schema: BUILD_HELPER_LIST_SCHEMA, units };
}

/**
 * The written paths of a Windows ETW trace exported by `tracerpt -of XML` from two kernel sessions: Microsoft-Windows-Kernel-Process (process
 * starts, to follow the build's own tree) and Microsoft-Windows-Kernel-File (create, create-new-file, write, delete-path, rename-path). Only
 * events from `rootPid`'s descendants count. A Create counts as a write unless its disposition is FILE_OPEN (1); a Write event is named by the
 * FileObject its Create recorded. Kernel paths (\Device\HarddiskVolumeN\...) are mapped back to drive letters through `devices`
 * ({ "\\Device\\HarddiskVolume3": "D:" }).
 */
export function parseEtwWrites(xml, { rootPid, devices = {} }) {
  const events = String(xml).split(/<Event[\s>]/).slice(1);
  const field = (ev, name) => new RegExp(`<Data Name="${name}">([^<]*)</Data>`).exec(ev)?.[1];
  const id = (ev) => Number(/<EventID>(\d+)<\/EventID>/.exec(ev)?.[1]);
  const pidOf = (ev) => Number(/ProcessID="(\d+)"/.exec(ev)?.[1]);
  const provider = (ev) => /Provider Name="([^"]+)"/.exec(ev)?.[1] ?? "";
  const tree = new Set([Number(rootPid)]);
  for (const ev of events) {
    if (provider(ev) === "Microsoft-Windows-Kernel-Process" && id(ev) === 1) {
      const child = Number(field(ev, "ProcessID")); const parent = Number(field(ev, "ParentProcessID"));
      if (tree.has(parent)) tree.add(child);
    }
  }
  const toDos = (p) => { if (!p) return null; for (const [dev, letter] of Object.entries(devices)) if (p.toLowerCase().startsWith(dev.toLowerCase())) return letter + p.slice(dev.length); return p; };
  const names = new Map(); const out = new Set();
  for (const ev of events) {
    if (provider(ev) !== "Microsoft-Windows-Kernel-File" || !tree.has(pidOf(ev))) continue;
    const n = id(ev);
    if (n === 12) {
      const name = field(ev, "FileName"); names.set(field(ev, "FileObject"), name);
      const disposition = (Number(field(ev, "CreateOptions") ?? 0) >>> 24) & 0xff;
      if (disposition !== 1) out.add(toDos(name));
    } else if (n === 30) out.add(toDos(field(ev, "FileName")));
    else if (n === 16) { const name = names.get(field(ev, "FileObject")); if (name) out.add(toDos(name)); }
    else if (n === 26 || n === 27) { const name = field(ev, "FilePath") ?? field(ev, "FileName"); if (name) out.add(toDos(name)); }
  }
  out.delete(null);
  return [...out];
}

/**
 * The written paths of a macOS `eslogger` trace (Endpoint Security, JSON lines) restricted to `rootPid`'s process tree. Each event names
 * its process and that process's parent (`process.audit_token.pid`, `process.ppid`), so every child a build script starts (sh, cc, cmake,
 * perl) is followed, whatever its name. Writes: create, rename (both sides), unlink, link, truncate, an open with FWRITE, and a close that
 * modified the file.
 */
export function parseEsloggerWrites(text, { rootPid }) {
  const events = [];
  for (const line of String(text).split(/\r?\n/)) { if (!line.trim().startsWith("{")) continue; try { events.push(JSON.parse(line)); } catch { /* a partial last line */ } }
  const parent = new Map();
  for (const e of events) {
    const pid = e?.process?.audit_token?.pid; const ppid = e?.process?.ppid ?? e?.process?.original_ppid;
    if (Number.isInteger(pid) && Number.isInteger(ppid) && !parent.has(pid)) parent.set(pid, ppid);
    const child = e?.event?.fork?.child?.audit_token?.pid;
    if (Number.isInteger(child) && Number.isInteger(pid)) parent.set(child, pid);
  }
  const inTree = (pid) => { for (let p = pid, hops = 0; Number.isInteger(p) && hops < 64; p = parent.get(p), hops += 1) if (p === rootPid) return true; return false; };
  const joined = (np) => (np?.dir?.path && np?.filename ? `${np.dir.path.replace(/\/$/, "")}/${np.filename}` : null);
  const dest = (d) => d?.existing_file?.path ?? joined(d?.new_path) ?? null;
  const out = new Set();
  for (const e of events) {
    if (!inTree(e?.process?.audit_token?.pid)) continue;
    const ev = e?.event ?? {};
    const add = (p) => { if (typeof p === "string" && p) out.add(p); };
    if (ev.create) add(dest(ev.create.destination));
    if (ev.rename) { add(ev.rename.source?.path); add(dest(ev.rename.destination)); }
    if (ev.unlink) add(ev.unlink.target?.path);
    if (ev.link) { add(ev.link.source?.path); add(ev.link.target_dir?.path && ev.link.target_filename ? `${ev.link.target_dir.path.replace(/\/$/, "")}/${ev.link.target_filename}` : null); }
    if (ev.truncate) add(ev.truncate.target?.path);
    // std::fs::copy on macOS clones first (fclonefileat): the destination is created without an open for writing.
    if (ev.clone) add(ev.clone.target_dir?.path && ev.clone.target_name ? `${ev.clone.target_dir.path.replace(/\/$/, "")}/${ev.clone.target_name}` : null);
    if (ev.copyfile) add(ev.copyfile.target_file?.path ?? (ev.copyfile.target_dir?.path && ev.copyfile.target_name ? `${ev.copyfile.target_dir.path.replace(/\/$/, "")}/${ev.copyfile.target_name}` : null));
    if (ev.exchangedata) { add(ev.exchangedata.file1?.path); add(ev.exchangedata.file2?.path); }
    if (ev.open && (Number(ev.open.fflag) & 0x2)) add(ev.open.file?.path);
    if (ev.close && ev.close.modified) add(ev.close.target?.path);
  }
  return [...out];
}

/** The strace -ff output files (`<prefix>.<pid>`, one per process: no call is ever split across lines). */
export function readStraceFiles(prefix, { fsApi = fs } = {}) {
  const dir = path.dirname(prefix); const base = path.basename(prefix);
  return fsApi.readdirSync(dir).filter((n) => n.startsWith(`${base}.`)).map((n) => fsApi.readFileSync(path.join(dir, n), "utf8")).join("\n");
}


/** Calls `fn(record)` for every record of `file` separated by `sep`, reading 8 MiB at a time (a whole-run trace can pass Node's string limit). */
export function forEachRecord(file, sep, fn, { fsApi = fs } = {}) {
  const fd = fsApi.openSync(file, "r"); const buf = Buffer.alloc(8 << 20); let rest = "";
  try {
    for (let n = fsApi.readSync(fd, buf, 0, buf.length, null); n > 0; n = fsApi.readSync(fd, buf, 0, buf.length, null)) {
      const parts = (rest + buf.toString("utf8", 0, n)).split(sep);
      rest = parts.pop();
      for (const part of parts) fn(part);
    }
    if (rest) fn(rest);
  } finally { fsApi.closeSync(fd); }
}

/** The root (one of `roots`) a pid descends from, through `parent`, or null. */
function rootFinder(parent, roots) {
  const memo = new Map();
  return (pid) => {
    if (!Number.isInteger(pid)) return null;
    if (memo.has(pid)) return memo.get(pid);
    let found = null;
    for (let p = pid, hops = 0; Number.isInteger(p) && hops < 64; p = parent.get(p), hops += 1) if (roots.has(p)) { found = p; break; }
    memo.set(pid, found);
    return found;
  };
}

/** eslogger JSON lines from `file`, streamed: Map(root pid -> written paths) for every pid in `roots` (a Set), in two passes. */
export function esloggerWritesByRoot(file, roots) {
  const parent = new Map();
  const parse = (line) => { if (!line.trim().startsWith("{")) return null; try { return JSON.parse(line); } catch { return null; } };
  forEachRecord(file, "\n", (line) => {
    const e = parse(line); if (!e) return;
    const pid = e?.process?.audit_token?.pid; const ppid = e?.process?.ppid ?? e?.process?.original_ppid;
    if (Number.isInteger(pid) && Number.isInteger(ppid) && !parent.has(pid)) parent.set(pid, ppid);
    const child = e?.event?.fork?.child?.audit_token?.pid;
    if (Number.isInteger(child) && Number.isInteger(pid)) parent.set(child, pid);
  });
  const rootOf = rootFinder(parent, roots); const out = new Map([...roots].map((r) => [r, new Set()]));
  forEachRecord(file, "\n", (line) => {
    const e = parse(line); if (!e) return;
    const root = rootOf(e?.process?.audit_token?.pid); if (root === null) return;
    for (const w of parseEsloggerWrites(line, { rootPid: e.process.audit_token.pid })) out.get(root).add(w);
  });
  return new Map([...out].map(([r, s]) => [r, [...s]]));
}

/** tracerpt XML from `file`, streamed by `</Event>`: Map(root pid -> written paths) for every pid in `roots`, kernel paths mapped by `devices`. */
export function etwWritesByRoot(file, roots, devices = {}) {
  const field = (ev, name) => new RegExp(`<Data Name="${name}">([^<]*)</Data>`).exec(ev)?.[1];
  const id = (ev) => Number(/<EventID>(\d+)<\/EventID>/.exec(ev)?.[1]);
  const pidOf = (ev) => Number(/ProcessID="(\d+)"/.exec(ev)?.[1]);
  const provider = (ev) => /Provider Name="([^"]+)"/.exec(ev)?.[1] ?? "";
  const parent = new Map();
  forEachRecord(file, "</Event>", (ev) => {
    if (provider(ev) === "Microsoft-Windows-Kernel-Process" && id(ev) === 1) parent.set(Number(field(ev, "ProcessID")), Number(field(ev, "ParentProcessID")));
  });
  const rootOf = rootFinder(parent, roots); const out = new Map([...roots].map((r) => [r, new Set()]));
  const toDos = (p) => { if (!p) return null; for (const [dev, letter] of Object.entries(devices)) if (p.toLowerCase().startsWith(dev.toLowerCase())) return letter + p.slice(dev.length); return p; };
  const names = new Map();
  forEachRecord(file, "</Event>", (ev) => {
    if (provider(ev) !== "Microsoft-Windows-Kernel-File") return;
    const n = id(ev); const fileObject = field(ev, "FileObject");
    if (n === 12) names.set(fileObject, field(ev, "FileName"));
    const root = rootOf(pidOf(ev)); if (root === null) return;
    const add = (p) => { const d = toDos(p); if (d) out.get(root).add(d); };
    if (n === 12) { const disposition = (Number(field(ev, "CreateOptions") ?? 0) >>> 24) & 0xff; if (disposition !== 1) add(field(ev, "FileName")); }
    else if (n === 30) add(field(ev, "FileName"));
    else if (n === 16) add(names.get(fileObject));
    else if (n === 26 || n === 27) add(field(ev, "FilePath") ?? field(ev, "FileName"));
  });
  return new Map([...out].map(([r, s]) => [r, [...s]]));
}
