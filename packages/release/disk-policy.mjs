import { closeSync, cpSync, existsSync, lstatSync, mkdirSync, opendirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, statSync, statfsSync, appendFileSync, writeFileSync } from "node:fs";
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
  for (const w of policy.writers) {
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
  for (const m of measured) if (deviceOf(m.path) === dev) byPath.set(fold(m.path, key), m);
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
  return { floor: floor.id ?? floor.label, usedBytes, writerBytes, unmanagedBytes: Math.max(0, usedBytes - writerBytes), uncoveredTopLevel };
}

export function checkDisk({ policy, platform = process.platform, env = process.env, home = os.homedir(), size = (p) => pathSize(p, { platform }), space = freeSpace, now = Date.now(), scan = scanWorkspaceRoots, readdir = readdirSync } = {}) {
  const key = platformKey(platform);
  const floorPolicies = floorList(policy, key);
  if (!floorPolicies.length) throw new Error(`disk policy: no floor for ${key}`);
  // A volume that cannot be read (an unmounted /Volumes/D) is a warning on its own floor, never a crash that hides the rest.
  const floors = floorPolicies.map((f) => {
    try { return evaluateFloor(f, space(f.path)); } catch (error) {
      return { ...(f.id ? { id: f.id } : {}), label: f.label, path: f.path, absent: true, error: error.message, warn: true, ...(f.agentVolume ? { agentVolume: true } : {}) };
    }
  });
  const floor = floors[0];
  const writers = [];
  const measured = [];
  for (const writer of policy.writers.filter((w) => w.platform === key)) {
    const category = writer.category ?? writer.id;
    let matches;
    try { matches = writerMatches(writer, { env, home }); } catch (error) {
      writers.push({ id: writer.id, category, action: writer.action, error: error.message, bytes: 0, capBytes: writer.capBytes, over: false, matches: 0 });
      continue;
    }
    const sized = matches.map((m) => ({ path: m, bytes: size(m) }));
    const bytes = sized.reduce((sum, m) => sum + m.bytes, 0);
    measured.push(...sized);
    writers.push({ id: writer.id, category, action: writer.action, path: writer.path, matches: matches.length, bytes, capBytes: writer.capBytes, over: bytes > writer.capBytes });
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
  return { schema: 1, platform: key, floor, floors, writers, unmanaged, unknown, warnings, warn: warnings.length > 0 };
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

export function planSweep({ policy, platform = process.platform, env = process.env, home = os.homedir(), now = Date.now(), staleDays, openPaths, inUse, tracked = gitTracksPath, size = (p) => pathSize(p, { platform }), scan = scanWorkspaceRoots, workspaceSweep = workspaceSweepAllowed({ platform, env, home }) } = {}) {
  const key = platformKey(platform);
  const effectiveDays = staleDays ?? policy.staleDays;
  const candidates = [];
  const skipped = [];
  const seen = new Set();
  const lockCheck = inUse ?? (key === "win" ? windowsLockProbe : (p) => inUseByOpenPaths(p, openPaths));
  // Sweep writers, plus report writers with `escalate` when they are over cap by its factor (pure caches: idle children go).
  for (const writer of policy.writers.filter((w) => w.platform === key && (w.action === "sweep" || w.escalate))) {
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
      if (hasFreshContent(unit, cutoff)) { skipped.push({ ...base, reason: "fresh" }); continue; }
      if (lockCheck(unit)) { skipped.push({ ...base, reason: "in-use" }); continue; }
      candidates.push({ ...base, root, bytes: size(unit), mtime: new Date(st.mtimeMs).toISOString() });
    }
  }
  // Overlapping writers (a whole cache dir and one of its children): the outer unit wins, the nested one is not moved twice.
  const nested = candidates.filter((c) => candidates.some((o) => o !== c && c.path.startsWith(o.path + path.sep)));
  for (const c of nested) skipped.push({ writer: c.writer, path: c.path, reason: "nested" });
  if (nested.length) candidates.splice(0, candidates.length, ...candidates.filter((c) => !nested.includes(c)));
  // Allowlisted workspace roots: unknown top-level entries older than 3 days and idle.
  const scanned = scan({ policy, platform, env, home });
  skipped.push(...scanned.skipped);
  const unknownCutoff = now - (staleDays ?? policy.unknownStaleDays ?? 3) * 86400_000;
  const consider = (writerId, unit, root, cutoff) => {
    if (seen.has(unit)) return;
    seen.add(unit);
    const base = { writer: writerId, path: unit };
    let st;
    try { st = lstatSync(unit); } catch { return; }
    if (st.isSymbolicLink()) { skipped.push({ ...base, reason: "symlink" }); return; }
    if (!isInsideRoot(unit, root)) { skipped.push({ ...base, reason: "outside-root" }); return; }
    if (isProtectedPath(unit, policy)) { skipped.push({ ...base, reason: "protected" }); return; }
    if (hasFreshContent(unit, cutoff)) { skipped.push({ ...base, reason: "fresh" }); return; }
    if (lockCheck(unit)) { skipped.push({ ...base, reason: "in-use" }); return; }
    candidates.push({ ...base, root, bytes: size(unit), mtime: new Date(st.mtimeMs).toISOString() });
  };
  // Report-only unless this host opted in (`disk allow-workspace-sweep`): unknown entries are warned about, never moved.
  for (const u of scanned.unknown) {
    if (!workspaceSweep) { skipped.push({ writer: "workspace-unknown", path: u.path, reason: "report-only" }); continue; }
    consider("workspace-unknown", u.path, u.root, unknownCutoff);
  }
  // Scratch roots: every child older than 3 days and idle (the same window).
  for (const root of scratchRoots(policy, { platform, env, home })) {
    let rst;
    try { rst = lstatSync(root); } catch { continue; }
    if (rst.isSymbolicLink()) { skipped.push({ writer: "scratch-root", path: root, reason: "symlink-root" }); continue; }
    let names = [];
    try { names = readdirSync(root); } catch { continue; }
    for (const n of names) consider("scratch-root", path.join(root, n), root, unknownCutoff);
  }
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

export function logEvent(logFile, event) {
  try {
    mkdirSync(path.dirname(logFile), { recursive: true });
    appendFileSync(logFile, `${JSON.stringify({ ts: new Date().toISOString(), ...event })}\n`);
  } catch { /* logging must never fail the sweep */ }
}

export function applySweep({ plan, policy, platform = process.platform, deleteMode = false, date = new Date(), env = process.env, home = os.homedir(), move = moveEntry, pick = pickHolding, isMounted, remove = (p) => rmSync(p, { recursive: true, force: true }) }) {
  const logFile = path.join(policy.heavyVolume[platformKey(platform)], "disk-hygiene.jsonl");
  const results = [];
  for (const c of plan.candidates) {
    try {
      // Re-verify at apply time: still the planned unit, not a symlink, still inside its root.
      if (!c.root || lstatSync(c.path).isSymbolicLink() || !isInsideRoot(c.path, c.root)) throw new Error("refused: path is not a sweepable unit inside its target root");
      if (c.writer === "workspace-unknown" && PROJECT_MARKERS.some((m) => existsSync(path.join(c.path, m)))) throw new Error("refused: a nested git checkout or project directory is never swept");
      if (deleteMode) { remove(c.path); results.push({ ...c, action: "delete", ok: true }); logEvent(logFile, { event: "disk.sweep.delete", ...c }); continue; }
      // An item stays on its own volume: moving it elsewhere frees nothing on the volume that is short of space.
      const holding = pick(c.path, { policy, platform, env, home, date, isMounted });
      if (!holding) {
        remove(c.path);
        results.push({ ...c, action: "delete", how: "no-holding-on-volume", ok: true });
        logEvent(logFile, { event: "disk.sweep.delete", reason: "no holding dir possible on the item's volume", ...c });
        continue;
      }
      let destination = path.join(holding, c.writer, path.basename(c.path));
      if (existsSync(destination)) destination = `${destination}.${date.getTime()}`;
      const how = move(c.path, destination);
      results.push({ ...c, action: "move", destination, how, ok: true });
      logEvent(logFile, { event: "disk.sweep.move", ...c, destination, how });
    } catch (error) {
      results.push({ ...c, action: deleteMode ? "delete" : "move", ok: false, error: error.message });
      logEvent(logFile, { event: "disk.sweep.error", ...c, error: error.message });
    }
  }
  return { logFile, results, movedBytes: results.filter((r) => r.ok).reduce((s, r) => s + r.bytes, 0) };
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

/** Remove date-named holding dirs older than `days`. Only dirs this tool created: the base must carry the holding marker
 * (or, for the legacy heavy dir, the tool's own log), the child must be a real directory named YYYY-MM-DD, never a symlink. */
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
      if (!Number.isFinite(stamp) || stamp + 86400_000 > cutoff) continue; // the whole day must be older than the window
      const bytes = size(dir);
      try {
        if (!dryRun) { remove(dir); if (logFile) logEvent(logFile, { event: "disk.purge", path: dir, bytes }); }
        results.push({ path: dir, bytes, ok: true, dryRun });
      } catch (error) { results.push({ path: dir, bytes, ok: false, error: error.message }); }
    }
  }
  return { results, purgedBytes: results.filter((r) => r.ok && !r.dryRun).reduce((a, r) => a + r.bytes, 0) };
}
