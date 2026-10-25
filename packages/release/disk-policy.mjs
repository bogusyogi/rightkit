import { closeSync, cpSync, existsSync, fstatSync, lstatSync, mkdirSync, openSync, opendirSync, readFileSync, readSync, readdirSync, realpathSync, renameSync, rmSync, statSync, statfsSync, appendFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";

export const DISK_EXIT_WARNING = 3;
export const DEFAULT_POLICY_FILE = path.join(path.dirname(fileURLToPath(import.meta.url)), "disk-policy.json");

/** Floors of one platform: `floors.<mac|win>` is a list; a single legacy object is a list of one. */
export function floorList(policy, key) {
  return [].concat(policy.floors?.[key] ?? []);
}

/** Path of the floor whose volume holds the items that may be parked cross-volume (`holdingSource: true`), else the first
 * floor. Holding must not depend on how many floors a platform has or in which order they are listed. */
export function holdingSourcePath(policy, key) {
  const floors = floorList(policy, key);
  return (floors.find((f) => f.holdingSource) ?? floors[0])?.path;
}

export function readDiskPolicy(file = DEFAULT_POLICY_FILE) {
  const policy = JSON.parse(readFileSync(file, "utf8"));
  if (policy.schema !== 1 || !Array.isArray(policy.writers)) throw new Error("disk policy: unsupported schema");
  for (const key of Object.keys(policy.floors ?? {})) {
    const floors = floorList(policy, key);
    for (const f of floors) if (!f.path) throw new Error(`disk policy: a ${key} floor needs a path`);
    if (floors.filter((f) => f.holdingSource).length > 1) throw new Error(`disk policy: ${key} floors name more than one holdingSource`);
    const ids = floors.map((f) => f.id).filter(Boolean);
    if (new Set(ids).size !== ids.length) throw new Error(`disk policy: ${key} floor ids must be unique`);
  }
  for (const [key, roots] of Object.entries(policy.strayTargets ?? {})) {
    for (const r of roots) if (typeof r.path !== "string" || !(Number.isInteger(r.depth ?? 4) && (r.depth ?? 4) >= 1 && (r.depth ?? 4) <= 8)) throw new Error(`disk policy: a ${key} strayTargets entry needs a path and a depth of 1 to 8`);
  }
  for (const [prefix, cap] of Object.entries(policy.scratchPrefixCaps ?? {})) {
    if (!RUN_ROOT_NAME.test(prefix) || prefix.includes("-") || !(Number.isFinite(cap) && cap >= 0)) throw new Error(`disk policy: scratchPrefixCaps.${prefix} needs a name without "-" and a byte cap >= 0`);
  }
  for (const w of policy.writers) {
    if (w.budget !== undefined && !(Array.isArray(w.budget) && w.budget.length && w.budget.every((n) => n === "total" || BUDGET_PARTS.includes(n)))) throw new Error(`disk policy: ${w.id} budget must list parts of ${[...BUDGET_PARTS, "total"].join("|")}`);
    // `escalate` lets a report-only cache sweep its idle children once it is over cap by `factor` (see planSweep).
    if (!w.escalate) continue;
    const e = w.escalate;
    if (w.action !== "report" || w.unit !== "children" || !(e.factor >= 1) || !(e.staleDays >= 0)) throw new Error(`disk policy: ${w.id} escalate needs action report, unit children, factor >= 1, staleDays >= 0`);
  }
  return policy;
}

export function platformKey(platform = process.platform) {
  return platform === "win32" || platform === "win" ? "win" : "mac";
}

export function expandPolicyPath(value, { env = process.env, home = os.homedir() } = {}) {
  let out = value.replace(/^~(?=$|[\\/])/, home);
  out = out.replace(/\$\{(\w+)\}/g, (_, name) => {
    if (!env[name]) throw new Error(`disk policy: ${name} is not set`);
    return env[name];
  });
  return out;
}

// One path segment of a policy pattern: `*` matches any run of characters inside that segment only.
const globSegment = (part) => new RegExp(`^${part.split("*").map((s) => s.replace(/[.+?^${}()|[\]\\]/g, "\\$&")).join(".*")}$`);

// Expand a policy path whose segments may contain `*` wildcards into existing paths.
export function expandGlob(pattern, sep = path.sep) {
  const parts = pattern.split(/[\\/]/);
  let bases = [parts[0] === "" ? sep : `${parts[0]}${sep}`];
  for (const part of parts.slice(1)) {
    if (part === "") continue;
    const next = [];
    for (const base of bases) {
      if (!part.includes("*")) {
        const candidate = path.join(base, part);
        if (existsSync(candidate)) next.push(candidate);
        continue;
      }
      const rx = globSegment(part);
      let names = [];
      try { names = readdirSync(base); } catch { continue; }
      for (const name of names) if (rx.test(name)) next.push(path.join(base, name));
    }
    bases = next;
  }
  return bases;
}

export function walkSize(root) {
  let total = 0;
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop();
    let st;
    try { st = lstatSync(dir); } catch { continue; }
    // Allocated bytes; where the filesystem reports none for a non-empty file (NTFS keeps small files resident
    // in the MFT, so Windows reports 0 blocks) the logical size is the best estimate.
    total += st.blocks ? st.blocks * 512 : st.size;
    if (!st.isDirectory()) continue;
    let handle;
    try { handle = opendirSync(dir); } catch { continue; }
    try {
      for (let e = handle.readSync(); e; e = handle.readSync()) stack.push(path.join(dir, e.name));
    } finally { try { handle.closeSync(); } catch { /* ignore */ } }
  }
  return total;
}

export function pathSize(target, { platform = process.platform } = {}) {
  if (platform === "darwin") {
    const r = spawnSync("du", ["-sk", target], { encoding: "utf8" });
    const kb = Number.parseInt(String(r.stdout).split(/\s+/)[0], 10);
    if (Number.isFinite(kb)) return kb * 1024;
  }
  return walkSize(target);
}

export function freeSpace(volumePath) {
  const s = statfsSync(volumePath);
  return { freeBytes: Number(s.bavail) * Number(s.bsize), totalBytes: Number(s.blocks) * Number(s.bsize) };
}

export function floorThreshold(floor, totalBytes) {
  const pct = floor.warnFreePercent ? Math.floor((totalBytes * floor.warnFreePercent) / 100) : 0;
  return Math.max(floor.warnFreeBytes ?? 0, pct);
}

// `hardFreeBytes` is informational: the level below which RustRight refuses builds. It never makes a floor warn on its own.
export function evaluateFloor(floor, space) {
  const thresholdBytes = floorThreshold(floor, space.totalBytes);
  const hard = floor.hardFreeBytes != null ? { hardFreeBytes: floor.hardFreeBytes, hardWarn: space.freeBytes < floor.hardFreeBytes } : {};
  return { ...(floor.id ? { id: floor.id } : {}), label: floor.label, path: floor.path, ...space, thresholdBytes, warn: space.freeBytes < thresholdBytes, ...hard, ...(floor.agentVolume ? { agentVolume: true } : {}) };
}

// Literal directory prefix of a policy path: everything before the first wildcard segment
// (for a wildcard-free path, its parent). A sweep unit must resolve inside it.
export function literalRoot(expanded) {
  const parts = expanded.split(/[\\/]/);
  const firstWild = parts.findIndex((p) => p.includes("*"));
  const keep = firstWild === -1 ? parts.slice(0, -1) : parts.slice(0, firstWild);
  const joined = keep.join(path.sep);
  return joined === "" || /^[A-Za-z]:$/.test(joined) ? `${joined}${path.sep}` : joined;
}

export function isInsideRoot(candidate, root) {
  try {
    const realRoot = realpathSync(root);
    const realCandidate = realpathSync(candidate);
    const prefix = realRoot.endsWith(path.sep) ? realRoot : realRoot + path.sep;
    return realCandidate !== realRoot && realCandidate.startsWith(prefix);
  } catch { return false; }
}

// True when a directory strictly between `literal` (the writer's literal root) and `target` is a symlink: a cache reached
// through a symlinked cache dir (some Mac caches now live on /Volumes/D) is never swept.
export function symlinkAncestor(target, literal) {
  const rel = path.relative(literal, target);
  if (!rel || rel.startsWith("..") || path.isAbsolute(rel)) return false;
  const parts = rel.split(path.sep).slice(0, -1);
  let cur = literal;
  for (const part of parts) {
    cur = path.join(cur, part);
    try { if (lstatSync(cur).isSymbolicLink()) return true; } catch { return false; }
  }
  return false;
}

export function writerMatches(writer, ctx) {
  const pattern = expandPolicyPath(writer.path, ctx);
  return expandGlob(pattern);
}

// How a writer's path pattern relates to `target`, compared as text (nothing is read from disk, symlinks and redirected
// environment variables are not resolved): "covers" when the pattern is the target or one of its ancestors, "within" when
// the target is an ancestor of the pattern, null when they diverge. A pattern that needs an unset variable (`${TEMP}`)
// proves nothing and relates to nothing.
function writerPathRelation(writer, target, { platform = process.platform, home = "/home/x", env = {} } = {}) {
  let pattern;
  try { pattern = expandPolicyPath(writer.path, { env, home }); } catch { return null; }
  const key = platformKey(platform);
  const split = (value) => value.split(/[\\/]/).filter(Boolean).map((part) => fold(part, key));
  const patternParts = split(pattern);
  const targetParts = split(target);
  for (let i = 0; i < Math.min(patternParts.length, targetParts.length); i += 1) if (!globSegment(patternParts[i]).test(targetParts[i])) return null;
  return patternParts.length <= targetParts.length ? "covers" : "within";
}

/** True when the writer's path is `target` or an ancestor of it, so a write to `target` belongs to that writer. */
export const writerCoversPath = (writer, target, options) => writerPathRelation(writer, target, options) === "covers";

/** True when the writer's path is `target`, an ancestor of it, or lies below it: the entry is accounted for by some writer. */
export const writerTouchesPath = (writer, target, options) => writerPathRelation(writer, target, options) !== null;


// ---- Workspace roots (allowlist) and scratch roots ------------------------------------------------------------------

const isWin = (key) => key === "win";
const fold = (value, key) => (isWin(key) ? value.toLowerCase() : value);

/** Top-level names git tracks in the repository rooted exactly at `root`; null when that cannot be proven
 * (not a repository root, git missing or failing). Callers must then leave the root alone. */
export function workspaceTracked(root, { platform = process.platform, git = spawnSync } = {}) {
  const key = platformKey(platform);
  const top = git("git", ["-C", root, "rev-parse", "--show-toplevel"], { encoding: "utf8" });
  if (top.status !== 0) return null;
  try {
    // The native form resolves Windows 8.3 short names (C:\Users\RUNNER~1) to the long names git prints.
    if (fold(realpathSync.native(String(top.stdout).trim()), key) !== fold(realpathSync.native(root), key)) return null;
  } catch { return null; }
  const listed = git("git", ["-C", root, "ls-files", "-z"], { encoding: "utf8", maxBuffer: 1 << 28 });
  if (listed.status !== 0) return null;
  const names = new Set();
  for (const file of String(listed.stdout).split("\0")) if (file) names.add(fold(file.split("/")[0], key));
  return names;
}

export function scratchRoots(policy, { platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  const out = [];
  for (const p of policy.scratchRoots?.[platformKey(platform)] ?? []) { try { out.push(expandPolicyPath(p, { env, home })); } catch { /* unset variable */ } }
  return out;
}

/** Create missing scratch roots (only when their parent volume exists). Returns the roots that exist afterwards. */
export function ensureScratchRoots({ policy, platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  const ready = [];
  for (const root of scratchRoots(policy, { platform, env, home })) {
    try {
      if (!existsSync(root) && existsSync(path.dirname(root))) mkdirSync(root);
      if (existsSync(root)) ready.push(root);
    } catch { /* volume not writable: the next run retries */ }
  }
  return ready;
}

/** Top-level entries of each workspace root that are neither tracked by the workspace repo, nested git checkouts,
 * nor on the allowlist. Symlinks are listed (`symlink: true`) but sweep never touches them. */
// Names that are always known at a workspace root, whatever the policy file says: agent instructions and agent state.
export const WORKSPACE_ALWAYS_ALLOWED = ["CLAUDE.md", "AGENTS.md", "CLAUDE.local.md", ".claude", ".codex", ".agent", ".agents", ".membrane", "cockpit", "contributions"];
const PROJECT_MARKERS = [".git", "package.json", "Cargo.toml"];

/** Host-local opt-in for moving workspace-unknown entries. Without it the target is report-only on every host. */
export function workspaceSweepFlagPath({ platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  if (platform === "win32" || platform === "win") {
    if (!env.LOCALAPPDATA) return null;
    return path.win32.join(env.LOCALAPPDATA, "Orthic Labs", "rightkit-release", "workspace-sweep-allowed");
  }
  return path.posix.join(home, "Library", "Application Support", "Orthic Labs", "rightkit-release", "workspace-sweep-allowed");
}
export function workspaceSweepAllowed(options = {}) {
  const file = workspaceSweepFlagPath(options);
  return Boolean(file) && existsSync(file);
}
export function setWorkspaceSweepAllowed(on, options = {}) {
  const file = workspaceSweepFlagPath(options);
  if (!file) throw new Error("disk: cannot place the host-local flag (LOCALAPPDATA is not set)");
  if (on) { mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, `workspace-unknown sweep allowed on this host ${new Date().toISOString()}\n`); }
  else renameSync(file, `${file}.revoked`);
  return file;
}

export function scanWorkspaceRoots({ policy, platform = process.platform, env = process.env, home = os.homedir(), tracked = (r) => workspaceTracked(r, { platform }) } = {}) {
  const key = platformKey(platform);
  const allow = new Set([...WORKSPACE_ALWAYS_ALLOWED, ...(policy.workspaceAllowlist ?? [])].map((n) => fold(n, key)));
  const unknown = [];
  const skipped = [];
  for (const entry of policy.workspaceRoots?.[key] ?? []) {
    let root;
    try { root = expandPolicyPath(entry.path, { env, home }); } catch { continue; }
    if (!existsSync(root)) continue;
    const trackedNames = tracked(root);
    if (!trackedNames) { skipped.push({ writer: "workspace-unknown", path: root, reason: "git-unavailable" }); continue; }
    let names = [];
    try { names = readdirSync(root); } catch { continue; }
    const extra = new Set((entry.allow ?? []).map((n) => fold(n, key)));
    for (const name of names) {
      const folded = fold(name, key);
      if (allow.has(folded) || extra.has(folded) || trackedNames.has(folded)) continue;
      if (/\.md$/i.test(name)) continue; // any markdown file at the root is documentation
      const full = path.join(root, name);
      let st;
      try { st = lstatSync(full); } catch { continue; }
      // a nested git checkout or any project directory (own package.json / Cargo.toml) is known
      if (!st.isSymbolicLink() && st.isDirectory() && PROJECT_MARKERS.some((m) => existsSync(path.join(full, m)))) continue;
      unknown.push({ root, name, path: full, symlink: st.isSymbolicLink(), mtimeMs: st.mtimeMs });
    }
  }
  return { unknown, skipped };
}

// Top-level names of an agent volume that are the operating system's, not data: never reported as uncovered.
export const DEFAULT_VOLUME_IGNORE = [".Trashes", ".fseventsd", ".Spotlight-V100", ".TemporaryItems", ".DocumentRevisions-V100", ".DS_Store", ".VolumeIcon.icns", "$RECYCLE.BIN", "System Volume Information", "lost+found"];

/** What an agent volume holds beyond its writers: used bytes minus the bytes of the outermost writer paths on that volume
 * (a writer nested inside another, or two writers on one path, count once), and the top-level entries no writer names or
 * contains. An estimate: statfs counts allocated blocks, snapshots and clones included. Null when the volume is not there. */
function unmanagedOnVolume(floor, { measured, policy, platform, env, home, size, readdir }) {
  const key = platformKey(platform);
  const dev = deviceOf(floor.path);
  if (dev === null) return null;
  const byPath = new Map();
  for (const m of measured) if (deviceOf(m.path) === dev && !byPath.has(fold(m.path, key))) byPath.set(fold(m.path, key), m);
  const onVolume = [...byPath.entries()];
  const outer = onVolume.filter(([p]) => !onVolume.some(([o]) => o !== p && p.startsWith(o + path.sep)));
  const writerBytes = outer.reduce((sum, [, m]) => sum + m.bytes, 0);
  const usedBytes = floor.totalBytes - floor.freeBytes;
  const ignore = new Set((policy.volumeIgnore ?? DEFAULT_VOLUME_IGNORE).map((n) => fold(n, key)));
  const writersHere = policy.writers.filter((w) => w.platform === key);
  const uncoveredTopLevel = [];
  let names = [];
  try { names = readdir(floor.path); } catch { /* unreadable volume root: nothing to list */ }
  for (const name of [...names].sort()) {
    if (ignore.has(fold(name, key))) continue;
    const full = path.join(floor.path, name);
    if (writersHere.some((w) => writerTouchesPath(w, full, { platform, home, env }))) continue;
    let symlink = false;
    try { symlink = lstatSync(full).isSymbolicLink(); } catch { continue; }
    uncoveredTopLevel.push({ name, path: full, symlink, bytes: symlink ? 0 : size(full) });
  }
  // `writers` is what the writer total is made of (the report's categories), so used = sum(writers) + unmanaged holds by construction.
  return { floor: floor.id ?? floor.label, usedBytes, writerBytes, unmanagedBytes: Math.max(0, usedBytes - writerBytes), writers: outer.map(([, m]) => ({ id: m.id, category: m.category, bytes: m.bytes })), uncoveredTopLevel };
}

/** Free space of every floor of a platform, statfs only (no directory is read). A volume that cannot be read (an unmounted
 * /Volumes/D) is a warning on its own floor, never a crash that hides the rest. */
export function checkFloors({ policy, platform = process.platform, space = freeSpace } = {}) {
  const key = platformKey(platform);
  const floorPolicies = floorList(policy, key);
  if (!floorPolicies.length) throw new Error(`disk policy: no floor for ${key}`);
  return floorPolicies.map((f) => {
    try { return evaluateFloor(f, space(f.path)); } catch (error) {
      return { ...(f.id ? { id: f.id } : {}), label: f.label, path: f.path, absent: true, error: error.message, warn: true, ...(f.agentVolume ? { agentVolume: true } : {}) };
    }
  });
}

// The parts of RustRight's build budget (`rustright doctor --json` -> `buildBudget`) a writer's `budget` list may name; `total` is
// the whole budget.
export const BUDGET_PARTS = ["targets", "sccache", "cargoHome", "toolchains", "trash", "slack"];

/** RustRight's build budget (`buildBudget` of `rustright doctor --json --no-scan`, all parts in bytes), or null when RustRight is
 * missing, fails, times out or answers with anything else. Nothing is cached and no shell is used. */
export function readRustrightBudget({ run = spawnSync } = {}) {
  try {
    const r = run("rustright", ["doctor", "--json", "--no-scan"], { encoding: "utf8", timeout: 20_000, shell: false, maxBuffer: 1 << 24 });
    if (!r || r.error || r.status !== 0) return null;
    const budget = JSON.parse(String(r.stdout)).buildBudget;
    if (!budget || typeof budget !== "object") return null;
    for (const part of [...BUDGET_PARTS, "totalBytes"]) if (!Number.isFinite(budget[part]) || budget[part] < 0) return null;
    return budget;
  } catch { return null; }
}

// A writer's cap: the sum of its `budget` parts when RustRight's budget is readable (`rustright`), else the `capBytes` of the
// policy (`policy`), so a host without RustRight keeps the committed number.
function writerCap(writer, budget) {
  if (!Array.isArray(writer.budget) || !budget) return { capBytes: writer.capBytes, capSource: "policy" };
  const parts = writer.budget.map((name) => (name === "total" ? budget.totalBytes : budget[name]));
  if (!parts.every(Number.isFinite)) return { capBytes: writer.capBytes, capSource: "policy" };
  return { capBytes: parts.reduce((a, b) => a + b, 0), capSource: "rustright" };
}

export function checkDisk({ policy, platform = process.platform, env = process.env, home = os.homedir(), size = (p) => pathSize(p, { platform }), space = freeSpace, now = Date.now(), scan = scanWorkspaceRoots, readdir = readdirSync, strays = findStrayTargets, readBudget = readRustrightBudget } = {}) {
  const key = platformKey(platform);
  const floors = checkFloors({ policy, platform, space });
  const floor = floors[0];
  const writers = [];
  const measured = [];
  const platformWriters = policy.writers.filter((w) => w.platform === key);
  // RustRight is asked only when a row follows its budget, and once.
  const budget = platformWriters.some((w) => Array.isArray(w.budget)) ? readBudget() : null;
  for (const writer of platformWriters) {
    const category = writer.category ?? writer.id;
    const { capBytes, capSource } = writerCap(writer, budget);
    let matches;
    try { matches = writerMatches(writer, { env, home }); } catch (error) {
      writers.push({ id: writer.id, category, action: writer.action, error: error.message, bytes: 0, capBytes, capSource, over: false, matches: 0 });
      continue;
    }
    const sized = matches.map((m) => ({ path: m, bytes: size(m), id: writer.id, category }));
    const bytes = sized.reduce((sum, m) => sum + m.bytes, 0);
    measured.push(...sized);
    writers.push({ id: writer.id, category, action: writer.action, path: writer.path, matches: matches.length, bytes, capBytes, capSource, over: bytes > capBytes });
  }
  const warnings = [];
  for (const f of floors) {
    if (f.absent) warnings.push(`floor volume ${f.path} is not mounted or cannot be read (${f.error})`);
    else if (f.warn) warnings.push(`${f.label} free ${formatBytes(f.freeBytes)} is below the ${formatBytes(f.thresholdBytes)} floor`);
  }
  for (const w of writers) if (w.over) warnings.push(`${w.id} holds ${formatBytes(w.bytes)} over its ${formatBytes(w.capBytes)} cap`);
  const unmanaged = [];
  for (const f of floors.filter((x) => x.agentVolume && !x.absent)) {
    const entry = unmanagedOnVolume(f, { measured, policy, platform, env, home, size, readdir });
    if (entry) unmanaged.push(entry);
  }
  for (const u of unmanaged) for (const e of u.uncoveredTopLevel) warnings.push(`no writer covers ${e.path} (${e.symlink ? "symlink" : formatBytes(e.bytes)}): add one to disk-policy.json`);
  const scanned = scan({ policy, platform, env, home });
  const unknown = scanned.unknown.map((u) => ({ root: u.root, name: u.name, path: u.path, symlink: u.symlink, bytes: u.symlink ? 0 : size(u.path), ageDays: Math.max(0, Math.floor((now - u.mtimeMs) / 86400_000)) }));
  for (const u of unknown) warnings.push(`unknown top-level entry ${u.path} (${u.symlink ? "symlink" : formatBytes(u.bytes)}, ${u.ageDays}d old): track it or add it to workspaceAllowlist (sweep only moves it on a host that ran \`disk allow-workspace-sweep\`)`);
  for (const s of scanned.skipped) warnings.push(`workspace root ${s.path}: tracked paths cannot be read from git, so unknown entries are not evaluated or swept`);
  // RustRight owns cargo targets, so a tagged target elsewhere is a stray that something built outside it.
  const strayList = [];
  for (const entry of policy.strayTargets?.[key] ?? []) {
    let root;
    try { root = expandPolicyPath(entry.path, { env, home }); } catch { continue; }
    for (const p of strays(root, { depth: entry.depth ?? 4, protectedSegments: policy.protectedSegments })) {
      let mtimeMs = now;
      try { mtimeMs = lstatSync(p).mtimeMs; } catch { /* vanished */ }
      strayList.push({ path: p, bytes: size(p), ageDays: Math.max(0, Math.floor((now - mtimeMs) / 86400_000)) });
    }
  }
  for (const t of strayList) warnings.push(`stray cargo target ${t.path} (${formatBytes(t.bytes)}, ${t.ageDays}d old): RustRight owns cargo targets; sweep moves it once idle for ${policy.strayTargetDays ?? 1} day(s)`);
  return { schema: 1, platform: key, floor, floors, writers, unmanaged, unknown, strays: strayList, warnings, warn: warnings.length > 0 };
}

export function formatBytes(n) {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(1)} GB`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${Math.round(n / 1024)} KB`;
}

// True when anything under `root` was modified after `cutoffMs` (early exit).
export function hasFreshContent(root, cutoffMs) {
  const stack = [root];
  while (stack.length) {
    const entry = stack.pop();
    let st;
    try { st = lstatSync(entry); } catch { continue; }
    if (st.mtimeMs > cutoffMs) return true;
    if (!st.isDirectory()) continue;
    let handle;
    try { handle = opendirSync(entry); } catch { continue; }
    try {
      for (let e = handle.readSync(); e; e = handle.readSync()) stack.push(path.join(entry, e.name));
    } finally { try { handle.closeSync(); } catch { /* ignore */ } }
  }
  return false;
}

// A directory the agent wants to keep carries a `.keep` file; it protects the directory for `keepMarkerDays` (default 14) after
// its last touch. An expired marker is ignored, so the unit ages out by the normal rules; a symlink named .keep proves nothing.
// Directory units only: a marker beside a file would itself be swept as a scratch child.
export const KEEP_MARKER = ".keep";
export function keptByMarker(unit, now, days, lstat = lstatSync) {
  try {
    const st = lstat(path.join(unit, KEEP_MARKER));
    return st.isFile() && !st.isSymbolicLink() && st.mtimeMs > now - days * 86400_000;
  } catch { return false; }
}

// ---- Run-root leases ------------------------------------------------------------------------------------------------------
// A run root is a directory directly under a scratch root that an agent leased with `disk run-root <name>`: it holds
// `.rightkit-run.json` = {"schema":1,"name","createdAt","leaseUntil"} (ISO times). The sweep leaves a unit with a valid lease alone,
// and the write-time guard (build-control-policy) lets clones and downloads land only inside a run root. Keep the format: the guard
// reads exactly this file.

export const RUN_LEASE_FILE = ".rightkit-run.json";
export const DEFAULT_LEASE_DAYS = 3;
/** The longest lease `disk run-root --days` writes. The write-time guard (build-control-policy) carries the same number and treats a lease
 * that claims more than this (plus one day) from its `createdAt`, or one created in the future, as no lease: a forged file cannot say 2999. */
export const MAX_LEASE_DAYS = 30;
const DAY_MS = 86_400_000;
const RUN_ROOT_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$/;

/** Owner of a scratch run: the name before its first `-` (legion-..., rk-..., pulse-..., hr-...). */
export const scratchOwner = (name) => name.split("-")[0];

/** The lease of a run-root directory, or null when it has none or the file is not a regular, well-formed lease (a symlink or an
 * oversized file proves nothing). A lease belongs to its own directory (`name` is the directory's name, so a copied lease file counts
 * nowhere else) and spans at most MAX_LEASE_DAYS (plus one day) from its creation, as `createRunRoot` writes it. */
export function readRunLease(unit, { lstat = lstatSync, readFile = readFileSync } = {}) {
  try {
    const file = path.join(unit, RUN_LEASE_FILE);
    const st = lstat(file);
    if (!st.isFile() || st.isSymbolicLink() || st.size > 4096) return null;
    const lease = JSON.parse(readFile(file, "utf8"));
    if (lease?.schema !== 1 || typeof lease.name !== "string" || !Number.isFinite(Date.parse(lease.createdAt)) || !Number.isFinite(Date.parse(lease.leaseUntil))) return null;
    if (lease.name !== path.basename(unit)) return null;
    if (Date.parse(lease.leaseUntil) - Date.parse(lease.createdAt) > (MAX_LEASE_DAYS + 1) * DAY_MS) return null;
    return lease;
  } catch { return null; }
}

/** True while the unit's lease has not run out. A lease created more than a day after `now` is not one either. */
export function leasedByRunRoot(unit, now, options) {
  const lease = readRunLease(unit, options);
  return lease !== null && Date.parse(lease.createdAt) <= now + DAY_MS && Date.parse(lease.leaseUntil) > now;
}

function runRootsBase({ policy, platform, env, home }) {
  const roots = scratchRoots(policy, { platform, env, home });
  if (!roots.length) throw new Error(`disk run-root: the policy has no scratch root for ${platformKey(platform)}`);
  return roots[0];
}

function checkRunRootName(name) {
  if (typeof name !== "string" || !RUN_ROOT_NAME.test(name) || name.endsWith(".")) throw new Error(`disk run-root: name ${JSON.stringify(name)} must be 1 to 80 letters, digits, dots, dashes or underscores, starting with a letter or digit (the part before the first "-" is the owner)`);
}

/** Lease `<scratch root>/<name>` for `days`: creates the directory and writes its lease file (renewing keeps `createdAt`). Refused,
 * with nothing written, while the scratch root as a whole is over the `scratch-total` writer's cap or the name's owner prefix is over
 * its `scratchPrefixCaps` entry: the agent has to release or remove finished run roots first. */
export function createRunRoot({ policy, platform = process.platform, env = process.env, home = os.homedir(), name, days = DEFAULT_LEASE_DAYS, now = Date.now(), size = (p) => pathSize(p, { platform }) } = {}) {
  checkRunRootName(name);
  if (!(Number.isFinite(days) && days > 0 && days <= MAX_LEASE_DAYS)) throw new Error(`disk run-root: --days must be a positive number of at most ${MAX_LEASE_DAYS}`);
  const base = runRootsBase({ policy, platform, env, home });
  if (!ensureScratchRoots({ policy, platform, env, home }).includes(base)) throw new Error(`disk run-root: scratch root ${base} does not exist and cannot be created (is the volume mounted?)`);
  if (lstatSync(base).isSymbolicLink()) throw new Error(`disk run-root: scratch root ${base} is a symlink`);
  const key = platformKey(platform);
  const total = policy.writers.find((w) => w.id === "scratch-total" && w.platform === key);
  if (total) {
    const bytes = size(base);
    if (bytes > total.capBytes) throw new Error(`disk run-root: refused, scratch holds ${formatBytes(bytes)} over its ${formatBytes(total.capBytes)} total cap; release or remove finished run roots in ${base} first`);
  }
  const owner = scratchOwner(name);
  const prefixCap = policy.scratchPrefixCaps?.[owner];
  if (prefixCap !== undefined) {
    let names = [];
    try { names = readdirSync(base); } catch { /* unreadable: nothing counted */ }
    const bytes = names.filter((n) => scratchOwner(n) === owner).reduce((sum, n) => sum + size(path.join(base, n)), 0);
    if (bytes > prefixCap) throw new Error(`disk run-root: refused, "${owner}" run roots hold ${formatBytes(bytes)} over their ${formatBytes(prefixCap)} cap; release or remove finished ${owner}-* run roots in ${base} first`);
  }
  const dir = path.join(base, name);
  mkdirSync(dir, { recursive: true });
  if (lstatSync(dir).isSymbolicLink() || !lstatSync(dir).isDirectory()) throw new Error(`disk run-root: ${dir} is not a plain directory`);
  const file = path.join(dir, RUN_LEASE_FILE);
  try { if (!lstatSync(file).isFile()) throw new Error(`disk run-root: ${file} is not a regular file`); } catch (error) { if (error.code !== "ENOENT") throw error; }
  const existing = readRunLease(dir);
  const leaseUntil = now + days * DAY_MS;
  // A renewal keeps the original createdAt only while the lease still fits MAX_LEASE_DAYS from it; a longer-running run root starts a new window.
  const keepCreated = existing !== null && leaseUntil - Date.parse(existing.createdAt) <= MAX_LEASE_DAYS * DAY_MS && Date.parse(existing.createdAt) <= now;
  const lease = { schema: 1, name, createdAt: keepCreated ? existing.createdAt : new Date(now).toISOString(), leaseUntil: new Date(leaseUntil).toISOString() };
  const temp = `${file}.tmp-${process.pid}`;
  writeFileSync(temp, `${JSON.stringify(lease)}\n`);
  renameSync(temp, file);
  return { path: dir, lease, renewed: existing !== null };
}

/** End a run root's lease by removing its lease file; the directory stays and ages out by the normal scratch rules. Says whether a
 * lease was there; throws when the run root does not exist. */
export function releaseRunRoot({ policy, platform = process.platform, env = process.env, home = os.homedir(), name } = {}) {
  checkRunRootName(name);
  const dir = path.join(runRootsBase({ policy, platform, env, home }), name);
  let st;
  try { st = lstatSync(dir); } catch { throw new Error(`disk run-root: no run root ${dir}`); }
  if (st.isSymbolicLink() || !st.isDirectory()) throw new Error(`disk run-root: ${dir} is not a plain directory`);
  const file = path.join(dir, RUN_LEASE_FILE);
  let had = false;
  try { had = lstatSync(file).isFile(); if (had) rmSync(file); } catch { /* no lease file */ }
  return { path: dir, released: had };
}

// ---- Stray cargo targets --------------------------------------------------------------------------------------------------
// RustRight owns cargo targets (under .rightkit-managed). A `target` or `.cache/cargo-target` directory that cargo tagged
// (CACHEDIR.TAG) anywhere else in a workspace or scratch root is a stray. The writer rows have no `**` and no content test,
// so strays are found by a bounded walk instead.

export const CACHEDIR_SIGNATURE = "Signature: 8a477f597d28d172789f06886806bc55";
// Never entered while looking for strays: repositories' own state, dependency trees and the package store (all huge, none cargo's).
const STRAY_WALK_SKIP = new Set([".git", "node_modules", ".pnpm-store"]);

function cargoTagged(dir) {
  let fd;
  try {
    const tag = path.join(dir, "CACHEDIR.TAG");
    if (!lstatSync(tag).isFile()) return false; // lstat: a symlinked tag proves nothing
    fd = openSync(tag, "r");
    const buf = Buffer.alloc(CACHEDIR_SIGNATURE.length);
    return readSync(fd, buf, 0, buf.length, 0) === buf.length && buf.toString("utf8") === CACHEDIR_SIGNATURE;
  } catch { return false; } finally { if (fd !== undefined) { try { closeSync(fd); } catch { /* ignore */ } } }
}

/** Cargo target directories under `root`, at most `depth` levels below it. Walks real directories only (a symlink is never
 * followed), skips `.git`, `node_modules`, `.pnpm-store` and any directory named in `protectedSegments`, and never enters a
 * target directory, tagged or not. A stray is a directory named `target`, or `cargo-target` inside a `.cache`, whose
 * CACHEDIR.TAG starts with cargo's signature. */
export function findStrayTargets(root, { depth = 4, protectedSegments = [] } = {}) {
  const protectedNames = new Set(protectedSegments);
  if (root.split(/[\\/]/).some((seg) => protectedNames.has(seg))) return [];
  const found = [];
  const stack = [{ dir: root, level: 0 }];
  while (stack.length) {
    const { dir, level } = stack.pop();
    let entries;
    try { entries = readdirSync(dir, { withFileTypes: true }); } catch { continue; }
    for (const entry of entries) {
      if (!entry.isDirectory() || entry.isSymbolicLink() || STRAY_WALK_SKIP.has(entry.name) || protectedNames.has(entry.name)) continue;
      const full = path.join(dir, entry.name);
      if (entry.name === "target" || (entry.name === "cargo-target" && path.basename(dir) === ".cache")) {
        if (cargoTagged(full)) found.push(full);
        continue;
      }
      if (level + 1 < depth) stack.push({ dir: full, level: level + 1 });
    }
  }
  return found.sort();
}

export function isProtectedPath(candidate, policy) {
  const segments = candidate.split(/[\\/]/);
  return (policy.protectedSegments ?? []).some((p) => segments.includes(p));
}

// Open-file listing for the whole machine on macOS; paths a live process holds.
export function listOpenPaths({ platform = process.platform } = {}) {
  if (platform === "win32") return null;
  const r = spawnSync("lsof", ["-n", "-P", "-w", "-F", "n"], { encoding: "utf8", maxBuffer: 1 << 28 });
  if (r.error || (r.status !== 0 && !r.stdout)) return undefined;
  return String(r.stdout).split("\n").filter((l) => l.startsWith("n/")).map((l) => l.slice(1));
}

export function inUseByOpenPaths(candidate, openPaths) {
  if (openPaths === undefined) return true; // cannot prove idle: treat as in use
  if (openPaths === null) return false;
  const prefix = candidate.endsWith(path.sep) ? candidate : candidate + path.sep;
  return openPaths.some((p) => p === candidate || p.startsWith(prefix));
}

// Windows has no lsof; a locked directory cannot be renamed. Probe by renaming away and back.
export function windowsLockProbe(candidate) {
  const probe = `${candidate}.rk-probe`;
  try { renameSync(candidate, probe); } catch { return true; }
  try { renameSync(probe, candidate); } catch { return true; }
  return false;
}

// True when git tracks anything at or under `target` (a tracked file is never swept). Not in a repository: not tracked.
export function gitTracksPath(target) {
  const top = spawnSync("git", ["-C", path.dirname(target), "rev-parse", "--show-toplevel"], { encoding: "utf8" });
  if (top.status !== 0) return false;
  const listed = spawnSync("git", ["-C", String(top.stdout).trim(), "ls-files", "-z", "--", target], { encoding: "utf8" });
  if (listed.status !== 0) return true; // cannot prove untracked: keep it
  return String(listed.stdout).length > 0;
}

// A `.keep` or a live run-root lease on any directory between `root` and a stray target (a scratch run, a checkout) keeps what is
// beneath it: "kept", "leased" or null. Asked again when the move is applied.
function heldAbove(unit, root, now, keepDays) {
  let held = null;
  for (let dir = path.dirname(unit); dir !== root && dir.startsWith(root); dir = path.dirname(dir)) held = held ?? (keptByMarker(dir, now, keepDays) ? "kept" : leasedByRunRoot(dir, now) ? "leased" : null);
  return held;
}

export function planSweep({ policy, platform = process.platform, env = process.env, home = os.homedir(), now = Date.now(), staleDays, openPaths, inUse, tracked = gitTracksPath, size = (p) => pathSize(p, { platform }), scan = scanWorkspaceRoots, strays = findStrayTargets, workspaceSweep = workspaceSweepAllowed({ platform, env, home }), light = false } = {}) {
  const key = platformKey(platform);
  const effectiveDays = staleDays ?? policy.staleDays;
  const candidates = [];
  const skipped = [];
  const seen = new Set();
  const keepDays = policy.keepMarkerDays ?? 14;
  const lockCheck = inUse ?? (key === "win" ? windowsLockProbe : (p) => inUseByOpenPaths(p, openPaths));
  // Sweep writers, plus report writers with `escalate` when they are over cap by its factor (pure caches: idle children go).
  // The light (hourly) pass is stat-only: just the cheap temp-like writers marked `light`, no escalation (it sizes a whole
  // root), no workspace scan and no stray walk.
  const sweepable = (w) => (light ? w.action === "sweep" && w.light === true : w.action === "sweep" || w.escalate);
  for (const writer of policy.writers.filter((w) => w.platform === key && sweepable(w))) {
    let roots; let literal;
    try { roots = writerMatches(writer, { env, home }); literal = literalRoot(expandPolicyPath(writer.path, { env, home })); } catch { continue; }
    // `--days N` overrides every writer; otherwise a writer may carry its own window (QA evidence: 3 days, caches: 2).
    let window = staleDays ?? writer.staleDays ?? policy.staleDays;
    if (writer.action !== "sweep") {
      const total = roots.reduce((sum, r) => sum + size(r), 0);
      if (!(total > writer.capBytes * writer.escalate.factor)) continue;
      window = staleDays ?? writer.escalate.staleDays;
    }
    const cutoff = now - window * 86400_000;
    const units = [];
    for (const r of roots) {
      if (writer.unit !== "children") { units.push({ unit: r, root: literal }); continue; }
      let rst;
      try { rst = lstatSync(r); } catch { continue; }
      if (rst.isSymbolicLink()) { skipped.push({ writer: writer.id, path: r, reason: "symlink-root" }); continue; }
      let names = [];
      try { names = readdirSync(r); } catch { continue; }
      for (const n of names) units.push({ unit: path.join(r, n), root: r });
    }
    for (const { unit, root } of units) {
      if (seen.has(unit)) continue;
      seen.add(unit);
      const base = { writer: writer.id, path: unit };
      let st;
      try { st = lstatSync(unit); } catch { continue; }
      if (st.isSymbolicLink()) { skipped.push({ ...base, reason: "symlink" }); continue; }
      if (!isInsideRoot(unit, root)) { skipped.push({ ...base, reason: "outside-root" }); continue; }
      if (symlinkAncestor(unit, literal)) { skipped.push({ ...base, reason: "symlink-ancestor" }); continue; }
      if (isProtectedPath(unit, policy)) { skipped.push({ ...base, reason: "protected" }); continue; }
      if (writer.skipTracked && tracked(unit)) { skipped.push({ ...base, reason: "tracked" }); continue; }
      if (keptByMarker(unit, now, keepDays)) { skipped.push({ ...base, reason: "kept" }); continue; }
      if (leasedByRunRoot(unit, now)) { skipped.push({ ...base, reason: "leased" }); continue; }
      if (hasFreshContent(unit, cutoff)) { skipped.push({ ...base, reason: "fresh" }); continue; }
      if (lockCheck(unit)) { skipped.push({ ...base, reason: "in-use" }); continue; }
      candidates.push({ ...base, root, bytes: size(unit), mtime: new Date(st.mtimeMs).toISOString(), freshCutoffMs: cutoff });
    }
  }
  // Allowlisted workspace roots: unknown top-level entries older than 3 days and idle.
  const scanned = light ? { unknown: [], skipped: [] } : scan({ policy, platform, env, home });
  skipped.push(...scanned.skipped);
  const unknownCutoff = now - (staleDays ?? policy.unknownStaleDays ?? 3) * 86400_000;
  // Scratch is run output the agent owns: it ages out on its own, shorter window, `--days N` still overriding both.
  const scratchCutoff = now - (staleDays ?? policy.scratchStaleDays ?? policy.unknownStaleDays ?? 3) * 86400_000;
  const consider = (writerId, unit, root, cutoff, { skipTracked = false } = {}) => {
    if (seen.has(unit)) return;
    seen.add(unit);
    const base = { writer: writerId, path: unit };
    let st;
    try { st = lstatSync(unit); } catch { return; }
    if (st.isSymbolicLink()) { skipped.push({ ...base, reason: "symlink" }); return; }
    if (!isInsideRoot(unit, root)) { skipped.push({ ...base, reason: "outside-root" }); return; }
    if (isProtectedPath(unit, policy)) { skipped.push({ ...base, reason: "protected" }); return; }
    if (skipTracked && tracked(unit)) { skipped.push({ ...base, reason: "tracked" }); return; }
    if (keptByMarker(unit, now, keepDays)) { skipped.push({ ...base, reason: "kept" }); return; }
    if (leasedByRunRoot(unit, now)) { skipped.push({ ...base, reason: "leased" }); return; }
    if (hasFreshContent(unit, cutoff)) { skipped.push({ ...base, reason: "fresh" }); return; }
    if (lockCheck(unit)) { skipped.push({ ...base, reason: "in-use" }); return; }
    candidates.push({ ...base, root, bytes: size(unit), mtime: new Date(st.mtimeMs).toISOString(), freshCutoffMs: cutoff });
  };
  // Report-only unless this host opted in (`disk allow-workspace-sweep`): unknown entries are warned about, never moved.
  for (const u of scanned.unknown) {
    if (!workspaceSweep) { skipped.push({ writer: "workspace-unknown", path: u.path, reason: "report-only" }); continue; }
    consider("workspace-unknown", u.path, u.root, unknownCutoff);
  }
  // Scratch roots: every child idle for `scratchStaleDays`.
  for (const root of scratchRoots(policy, { platform, env, home })) {
    let rst;
    try { rst = lstatSync(root); } catch { continue; }
    if (rst.isSymbolicLink()) { skipped.push({ writer: "scratch-root", path: root, reason: "symlink-root" }); continue; }
    let names = [];
    try { names = readdirSync(root); } catch { continue; }
    for (const n of names) consider("scratch-root", path.join(root, n), root, scratchCutoff);
  }
  // Stray cargo targets (tagged, outside RustRight's roots): idle `strayTargetDays`, never when git tracks anything in them.
  const strayCutoff = now - (staleDays ?? policy.strayTargetDays ?? 1) * 86400_000;
  for (const entry of light ? [] : policy.strayTargets?.[key] ?? []) {
    let root;
    try { root = expandPolicyPath(entry.path, { env, home }); } catch { continue; }
    for (const unit of strays(root, { depth: entry.depth ?? 4, protectedSegments: policy.protectedSegments })) {
      const held = heldAbove(unit, root, now, keepDays);
      if (held) { skipped.push({ writer: "stray-cargo-target", path: unit, reason: held }); continue; }
      consider("stray-cargo-target", unit, root, strayCutoff, { skipTracked: true });
    }
  }
  // Overlapping units (a whole cache dir and one of its children, a scratch run and a target inside it): the outer unit wins,
  // the nested one is not moved twice.
  const nested = candidates.filter((c) => candidates.some((o) => o !== c && c.path.startsWith(o.path + path.sep)));
  for (const c of nested) skipped.push({ writer: c.writer, path: c.path, reason: "nested" });
  if (nested.length) candidates.splice(0, candidates.length, ...candidates.filter((c) => !nested.includes(c)));
  return { schema: 1, platform: key, staleDays: effectiveDays, candidates, skipped, totalBytes: candidates.reduce((s, c) => s + c.bytes, 0) };
}

export const HOLDING_DIR = ".rightkit-swept";
export const HOLDING_MARKER = ".rightkit-release-holding";
const DATE_DIR = /^\d{4}-\d{2}-\d{2}$/;

// Device id of a path, or of its nearest existing ancestor (null when none can be read).
export function deviceOf(target) {
  let cur = target;
  for (;;) {
    try { return statSync(cur).dev; } catch {
      const up = path.dirname(cur);
      if (up === cur) return null;
      cur = up;
    }
  }
}

// Highest ancestor of `target` still on the same device: the mount point (or drive root) holding it.
export function volumeRoot(target) {
  const dev = deviceOf(target);
  let cur = target;
  for (;;) {
    const up = path.dirname(cur);
    if (up === cur || deviceOf(up) !== dev) return cur;
    cur = up;
  }
}

const isUnder = (candidate, prefix, win) => {
  const norm = (v) => (win ? v.toLowerCase() : v);
  const c = norm(candidate); const p = norm(prefix);
  const withSep = p.endsWith(path.sep) || p.endsWith("/") || p.endsWith("\\") ? p : p + path.sep;
  return c === p || c.startsWith(withSep);
};

// True when a volume is mounted at `mount` and is not the item's own volume (`itemDev`).
export function defaultMounted(mount, itemDev) {
  try {
    const dev = deviceOf(mount);
    return existsSync(mount) && dev !== null && dev !== itemDev;
  } catch { return false; }
}

// Candidate holding bases for an item, best first: policy per-volume rules (longest prefix), then the item's own volume root.
// A rule with `mount` is a cross-volume base: it applies only to items on the volume of the floor flagged `holdingSource`
// (the Mac internal drive), and only while that mount is present (checked at pick time). Entries are `{ path, mount? }`.
export function holdingBases(candidate, { policy, platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  const key = platformKey(platform);
  const out = [];
  let floorDev;
  const rules = (policy.holding?.[key] ?? []).flatMap((r) => {
    try {
      return [{ prefix: expandPolicyPath(r.prefix, { env, home }), path: expandPolicyPath(r.path, { env, home }), mount: r.mount ? expandPolicyPath(r.mount, { env, home }) : undefined }];
    } catch { return []; }
  }).filter((r) => isUnder(candidate, r.prefix, key === "win")).sort((a, b) => b.prefix.length - a.prefix.length);
  for (const r of rules) {
    if (r.mount) {
      if (floorDev === undefined) {
        const floorPath = holdingSourcePath(policy, key);
        floorDev = floorPath ? deviceOf(expandPolicyPath(floorPath, { env, home })) : null;
      }
      if (floorDev === null || deviceOf(path.dirname(candidate)) !== floorDev) continue;
    }
    out.push({ path: r.path, ...(r.mount ? { mount: r.mount } : {}) });
  }
  out.push({ path: path.join(volumeRoot(path.dirname(candidate)), HOLDING_DIR) });
  return out;
}

// Create (and mark) today's holding dir; null when no holding dir is possible. A same-volume base must sit on the item's
// own volume; a mount base must sit on a mounted volume other than the item's (`isMounted`, injectable for tests).
export function pickHolding(candidate, { policy, platform = process.platform, env = process.env, home = os.homedir(), date = new Date(), bases, isMounted = defaultMounted } = {}) {
  const dev = deviceOf(path.dirname(candidate));
  for (const entry of bases ?? holdingBases(candidate, { policy, platform, env, home })) {
    const base = typeof entry === "string" ? entry : entry.path;
    const mount = typeof entry === "string" ? undefined : entry.mount;
    try {
      // checked before creating anything: a same-volume base on another volume would free nothing
      if (mount ? !isMounted(mount, dev) : deviceOf(base) !== dev) continue;
      mkdirSync(base, { recursive: true });
      if (lstatSync(base).isSymbolicLink() || (!mount && deviceOf(base) !== dev)) continue;
      const marker = path.join(base, HOLDING_MARKER);
      if (!existsSync(marker)) writeFileSync(marker, "created by right-release disk; date-named children are purged after the retention window\n");
      const dir = path.join(base, date.toISOString().slice(0, 10));
      mkdirSync(dir, { recursive: true });
      return dir;
    } catch { /* try the next base */ }
  }
  return null;
}

// Entries (lstat-visited, symlinks not followed) and allocated bytes under `root`.
export function treeStats(root) {
  let entries = 0;
  let bytes = 0;
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop();
    let st;
    try { st = lstatSync(dir); } catch { continue; }
    entries += 1;
    bytes += st.blocks ? st.blocks * 512 : st.size;
    if (!st.isDirectory()) continue;
    let names = [];
    try { names = readdirSync(dir); } catch { continue; }
    for (const name of names) stack.push(path.join(dir, name));
  }
  return { entries, bytes };
}

const pathExists = (p) => { try { lstatSync(p); return true; } catch { return false; } };

export function moveEntry(source, destination) {
  mkdirSync(path.dirname(destination), { recursive: true });
  try { renameSync(source, destination); return "rename"; } catch (error) {
    if (error.code !== "EXDEV") throw error;
  }
  // Cross-volume: copy, verify the copy against the source, and only then remove the source. Never overwrite.
  if (pathExists(destination)) throw new Error(`destination exists: ${destination}; source kept`);
  const before = treeStats(source);
  try {
    cpSync(source, destination, { recursive: true, preserveTimestamps: true, verbatimSymlinks: true, errorOnExist: true, force: false });
    const after = treeStats(destination);
    if (after.entries !== before.entries || after.bytes < before.bytes) {
      throw new Error(`copy verification failed for ${source} (entries ${before.entries} -> ${after.entries}, bytes ${before.bytes} -> ${after.bytes}); source kept`);
    }
  } catch (error) {
    rmSync(destination, { recursive: true, force: true }); // our partial copy only: the source is untouched
    throw error;
  }
  rmSync(source, { recursive: true, force: true }); // completes the move after verified copy
  return "copy-verify-remove";
}

// The tail of the log read by `readLogTail`, then more of it, until an event is found: a busy day appends thousands of sweep lines
// after the last full pass, so the newest full-pass line can lie further back than any one window. 4, 16, 64 and 256 MiB.
export const EVENT_SCAN_WINDOWS = [1 << 22, 1 << 24, 1 << 26, 1 << 28];

/** Parsed events of the log's last `maxBytes`, oldest first, and whether that reached the start of the file. A partial first line and
 * any unparsable line are skipped. Throws what the filesystem throws (a missing log is ENOENT). */
function readLogTail(logFile, maxBytes) {
  let fd;
  try {
    fd = openSync(logFile, "r");
    const st = fstatSync(fd);
    // A directory reports size 0 on Windows (and under /proc), so reading it would look like an empty log rather than fail.
    if (!st.isFile()) throw Object.assign(new Error(`EISDIR: ${logFile} is not a regular file`), { code: "EISDIR" });
    const { size } = st;
    const start = Math.max(0, size - maxBytes);
    const buf = Buffer.alloc(size - start);
    readSync(fd, buf, 0, buf.length, start);
    const lines = buf.toString("utf8").split("\n");
    if (start > 0) lines.shift(); // starts mid-line
    const events = [];
    for (const line of lines) { try { const e = JSON.parse(line); if (e && typeof e === "object" && !Array.isArray(e)) events.push(e); } catch { /* blank or damaged line */ } }
    return { events, whole: start === 0 };
  } finally { if (fd !== undefined) { try { closeSync(fd); } catch { /* ignore */ } } }
}

/** Parsed events of the log's last `maxBytes`, oldest first. A partial first line and any unparsable line are skipped; a log
 * that cannot be read is an empty list. */
export function readRecentEvents(logFile, { maxBytes = 1 << 22 } = {}) {
  try { return readLogTail(logFile, maxBytes).events; } catch { return []; }
}

/** Newest event of the log whose `event` is one of `names` (and that `accept`s), or null. The log is read backwards in growing
 * windows (EVENT_SCAN_WINDOWS) until a match turns up or the whole file has been read. A log that does not exist has no event (null:
 * the job never ran); any other read error (EIO, ENOTDIR, a volume that went away) is thrown, because "nothing found" would then
 * be a guess. `maxBytes` limits the scan to one window of that size. */
export function lastEvent(logFile, names, { maxBytes, accept = () => true, windows = EVENT_SCAN_WINDOWS } = {}) {
  for (const window of maxBytes ? [maxBytes] : windows) {
    let tail;
    try { tail = readLogTail(logFile, window); } catch (error) { if (error?.code === "ENOENT") return null; throw error; }
    for (let i = tail.events.length - 1; i >= 0; i -= 1) if (names.includes(tail.events[i].event) && accept(tail.events[i])) return tail.events[i];
    if (tail.whole) return null;
  }
  return null;
}

export function logEvent(logFile, event) {
  try {
    mkdirSync(path.dirname(logFile), { recursive: true });
    appendFileSync(logFile, `${JSON.stringify({ ts: new Date().toISOString(), ...event })}\n`);
  } catch { /* logging must never fail the sweep */ }
}

// Why an item the plan listed must not be moved now, or null. The plan can be minutes old (a full pass sizes everything in between)
// and an agent may have kept, leased or touched the unit since: the checks of planSweep run again, per candidate, right before the move.
function recheckSweep(c, { policy, now }) {
  const keepDays = policy.keepMarkerDays ?? 14;
  if (keptByMarker(c.path, now, keepDays)) return "rechecked-keep";
  if (leasedByRunRoot(c.path, now)) return "rechecked-leased";
  const above = c.writer === "stray-cargo-target" && c.root ? heldAbove(c.path, c.root, now, keepDays) : null;
  if (above) return above === "kept" ? "rechecked-keep" : "rechecked-leased";
  if (Number.isFinite(c.freshCutoffMs) && hasFreshContent(c.path, c.freshCutoffMs)) return "rechecked-fresh";
  return null;
}

// Where `source` goes below `holding/<writer>`: its own name, else `<name>.1`, `<name>.2`, ... The same name recurs within one
// sweep (a `target` in every checkout, one child name under several parents), so the first free suffix is looked up each time.
function freeDestination(holding, writer, source) {
  const first = path.join(holding, writer, path.basename(source));
  let destination = first;
  for (let n = 1; pathExists(destination); n += 1) destination = `${first}.${n}`;
  return destination;
}

export function applySweep({ plan, policy, platform = process.platform, deleteMode = false, date = new Date(), now = Date.now(), env = process.env, home = os.homedir(), move = moveEntry, pick = pickHolding, isMounted, remove = (p) => rmSync(p, { recursive: true, force: true }) }) {
  const logFile = path.join(policy.heavyVolume[platformKey(platform)], "disk-hygiene.jsonl");
  const results = [];
  const skipped = [];
  for (const c of plan.candidates) {
    try {
      // Re-verify at apply time: still the planned unit, not a symlink, still inside its root.
      if (!c.root || lstatSync(c.path).isSymbolicLink() || !isInsideRoot(c.path, c.root)) throw new Error("refused: path is not a sweepable unit inside its target root");
      if (c.writer === "workspace-unknown" && PROJECT_MARKERS.some((m) => existsSync(path.join(c.path, m)))) throw new Error("refused: a nested git checkout or project directory is never swept");
      const held = recheckSweep(c, { policy, now });
      if (held) { skipped.push({ ...c, reason: held }); logEvent(logFile, { event: "disk.sweep.skip", reason: held, ...c }); continue; }
      if (deleteMode) { remove(c.path); results.push({ ...c, action: "delete", ok: true }); logEvent(logFile, { event: "disk.sweep.delete", ...c }); continue; }
      // An item stays on its own volume: moving it elsewhere frees nothing on the volume that is short of space.
      const holding = pick(c.path, { policy, platform, env, home, date, isMounted });
      if (!holding) {
        remove(c.path);
        results.push({ ...c, action: "delete", how: "no-holding-on-volume", ok: true });
        logEvent(logFile, { event: "disk.sweep.delete", reason: "no holding dir possible on the item's volume", ...c });
        continue;
      }
      const destination = freeDestination(holding, c.writer, c.path);
      const how = move(c.path, destination);
      results.push({ ...c, action: "move", destination, how, ok: true });
      logEvent(logFile, { event: "disk.sweep.move", ...c, sourceParent: path.dirname(c.path), destination, how });
    } catch (error) {
      results.push({ ...c, action: deleteMode ? "delete" : "move", ok: false, error: error.message });
      logEvent(logFile, { event: "disk.sweep.error", ...c, error: error.message });
    }
  }
  return { logFile, results, skipped, movedBytes: results.filter((r) => r.ok).reduce((s, r) => s + r.bytes, 0) };
}

// Holding bases that may exist: policy rule paths, each drive root (Windows) or /Volumes/* and the legacy heavy dir (Mac).
export function knownHoldingBases({ policy, platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  const key = platformKey(platform);
  const bases = new Set();
  for (const r of policy.holding?.[key] ?? []) { try { bases.add(expandPolicyPath(r.path, { env, home })); } catch { /* unset variable */ } }
  if (key === "win") for (const letter of "ABCDEFGHIJKLMNOPQRSTUVWXYZ") { const root = `${letter}:\\`; if (existsSync(root)) bases.add(path.join(root, HOLDING_DIR)); }
  else { bases.add(path.join(path.sep, HOLDING_DIR)); try { for (const v of readdirSync("/Volumes")) bases.add(path.join("/Volumes", v, HOLDING_DIR)); } catch { /* no /Volumes */ } }
  return [...bases];
}

/** The holding bases (every known one plus the legacy heavy dir) that live on the volume holding `volumePath`: the ones whose purge
 * frees that volume. An emergency on one volume must not empty another volume's undo window. None when that volume cannot be read. */
export function holdingBasesOnVolume(volumePath, { policy, platform = process.platform, env = process.env, home = os.homedir(), deviceOf: deviceFor = deviceOf } = {}) {
  const key = platformKey(platform);
  const legacy = policy.heavyVolume?.[key];
  const device = deviceFor(volumePath);
  if (device === null || device === undefined) return [];
  return [...new Set([...knownHoldingBases({ policy, platform, env, home }), ...(legacy ? [legacy] : [])])].filter((base) => deviceFor(base) === device);
}

// Newest modification time (ms) of a holding dir and of its direct children, symlinks not followed. A move into a dated
// folder updates the mtime of the folder and of the writer dir inside it, though the moved item keeps its own age. Infinity
// when the dir cannot be read: what cannot be inspected is not purged.
function newestWriteMs(dir) {
  let newest;
  let names;
  try { newest = lstatSync(dir).mtimeMs; names = readdirSync(dir); } catch { return Infinity; }
  for (const name of names) { try { newest = Math.max(newest, lstatSync(path.join(dir, name)).mtimeMs); } catch { /* vanished */ } }
  return newest;
}

/** Remove date-named holding dirs a full `days` after their date (`date 00:00Z + days <= now`), and only when nothing was
 * moved into or written in the dir within the last `days` either, so every item is held at least `days` even though the
 * folder name carries only a date. Only dirs this tool created: the base must carry the holding marker (or, for the legacy
 * heavy dir, the tool's own log), the child must be a real directory named YYYY-MM-DD, never a symlink. */
export function purgeHoldings({ policy, platform = process.platform, env = process.env, home = os.homedir(), now = Date.now(), days = policy.holdingDays ?? 3, dryRun = false, bases, remove = (p) => rmSync(p, { recursive: true, force: true }), size = walkSize } = {}) {
  const key = platformKey(platform);
  const legacy = policy.heavyVolume?.[key];
  const list = bases ?? [...knownHoldingBases({ policy, platform, env, home }), ...(legacy ? [legacy] : [])];
  const logFile = legacy ? path.join(legacy, "disk-hygiene.jsonl") : null;
  const cutoff = now - days * 86400_000;
  const results = [];
  for (const base of new Set(list)) {
    let st;
    try { st = lstatSync(base); } catch { continue; }
    if (!st.isDirectory() || st.isSymbolicLink()) continue;
    const owned = existsSync(path.join(base, HOLDING_MARKER)) || (base === legacy && existsSync(path.join(base, "disk-hygiene.jsonl")));
    if (!owned) continue;
    let names = [];
    try { names = readdirSync(base); } catch { continue; }
    for (const name of names) {
      if (!DATE_DIR.test(name)) continue;
      const dir = path.join(base, name);
      let dst;
      try { dst = lstatSync(dir); } catch { continue; }
      if (!dst.isDirectory() || dst.isSymbolicLink()) continue;
      const stamp = Date.parse(`${name}T00:00:00Z`);
      if (!Number.isFinite(stamp) || stamp > cutoff) continue; // the folder's date is a full window old
      if (newestWriteMs(dir) > cutoff) continue; // but something was moved in (or written) within the window
      const bytes = size(dir);
      try {
        if (!dryRun) { remove(dir); if (logFile) logEvent(logFile, { event: "disk.purge", path: dir, bytes }); }
        results.push({ path: dir, bytes, ok: true, dryRun });
      } catch (error) { results.push({ path: dir, bytes, ok: false, error: error.message }); }
    }
  }
  return { results, purgedBytes: results.filter((r) => r.ok && !r.dryRun).reduce((a, r) => a + r.bytes, 0) };
}
