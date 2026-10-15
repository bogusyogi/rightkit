import { closeSync, cpSync, existsSync, lstatSync, mkdirSync, opendirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, statSync, statfsSync, appendFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";

export const DISK_EXIT_WARNING = 3;
export const DEFAULT_POLICY_FILE = path.join(path.dirname(fileURLToPath(import.meta.url)), "disk-policy.json");

export function readDiskPolicy(file = DEFAULT_POLICY_FILE) {
  const policy = JSON.parse(readFileSync(file, "utf8"));
  if (policy.schema !== 1 || !Array.isArray(policy.writers)) throw new Error("disk policy: unsupported schema");
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
      const rx = new RegExp(`^${part.split("*").map((s) => s.replace(/[.+?^${}()|[\]\\]/g, "\\$&")).join(".*")}$`);
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
    total += st.blocks !== undefined ? st.blocks * 512 : st.size;
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

export function evaluateFloor(floor, space) {
  const thresholdBytes = floorThreshold(floor, space.totalBytes);
  return { label: floor.label, path: floor.path, ...space, thresholdBytes, warn: space.freeBytes < thresholdBytes };
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

export function writerMatches(writer, ctx) {
  const pattern = expandPolicyPath(writer.path, ctx);
  return expandGlob(pattern);
}

export function checkDisk({ policy, platform = process.platform, env = process.env, home = os.homedir(), size = (p) => pathSize(p, { platform }), space = freeSpace } = {}) {
  const key = platformKey(platform);
  const floorPolicy = policy.floors[key];
  const floor = evaluateFloor(floorPolicy, space(floorPolicy.path));
  const writers = [];
  for (const writer of policy.writers.filter((w) => w.platform === key)) {
    let matches;
    try { matches = writerMatches(writer, { env, home }); } catch (error) {
      writers.push({ id: writer.id, action: writer.action, error: error.message, bytes: 0, capBytes: writer.capBytes, over: false, matches: 0 });
      continue;
    }
    const bytes = matches.reduce((sum, m) => sum + size(m), 0);
    writers.push({ id: writer.id, action: writer.action, path: writer.path, matches: matches.length, bytes, capBytes: writer.capBytes, over: bytes > writer.capBytes });
  }
  const warnings = [];
  if (floor.warn) warnings.push(`${floor.label} free ${formatBytes(floor.freeBytes)} is below the ${formatBytes(floor.thresholdBytes)} floor`);
  for (const w of writers) if (w.over) warnings.push(`${w.id} holds ${formatBytes(w.bytes)} over its ${formatBytes(w.capBytes)} cap`);
  return { schema: 1, platform: key, floor, writers, warnings, warn: warnings.length > 0 };
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

export function planSweep({ policy, platform = process.platform, env = process.env, home = os.homedir(), now = Date.now(), staleDays = policy.staleDays, openPaths, inUse, size = (p) => pathSize(p, { platform }) } = {}) {
  const key = platformKey(platform);
  const cutoff = now - staleDays * 86400_000;
  const candidates = [];
  const skipped = [];
  const seen = new Set();
  const lockCheck = inUse ?? (key === "win" ? windowsLockProbe : (p) => inUseByOpenPaths(p, openPaths));
  for (const writer of policy.writers.filter((w) => w.platform === key && w.action === "sweep")) {
    let roots; let literal;
    try { roots = writerMatches(writer, { env, home }); literal = literalRoot(expandPolicyPath(writer.path, { env, home })); } catch { continue; }
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
      if (isProtectedPath(unit, policy)) { skipped.push({ ...base, reason: "protected" }); continue; }
      if (hasFreshContent(unit, cutoff)) { skipped.push({ ...base, reason: "fresh" }); continue; }
      if (lockCheck(unit)) { skipped.push({ ...base, reason: "in-use" }); continue; }
      candidates.push({ ...base, root, bytes: size(unit), mtime: new Date(st.mtimeMs).toISOString() });
    }
  }
  return { schema: 1, platform: key, staleDays, candidates, skipped, totalBytes: candidates.reduce((s, c) => s + c.bytes, 0) };
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

// Candidate holding bases for an item, best first: policy per-volume rules (longest prefix), then the item's own volume root.
export function holdingBases(candidate, { policy, platform = process.platform, env = process.env, home = os.homedir() } = {}) {
  const key = platformKey(platform);
  const out = [];
  const rules = (policy.holding?.[key] ?? []).flatMap((r) => {
    try { return [{ prefix: expandPolicyPath(r.prefix, { env, home }), path: expandPolicyPath(r.path, { env, home }) }]; } catch { return []; }
  }).filter((r) => isUnder(candidate, r.prefix, key === "win")).sort((a, b) => b.prefix.length - a.prefix.length);
  for (const r of rules) out.push(r.path);
  out.push(path.join(volumeRoot(path.dirname(candidate)), HOLDING_DIR));
  return out;
}

// Create (and mark) today's holding dir on the item's own volume; null when no holding dir on that volume is possible.
export function pickHolding(candidate, { policy, platform = process.platform, env = process.env, home = os.homedir(), date = new Date(), bases } = {}) {
  const dev = deviceOf(path.dirname(candidate));
  for (const base of bases ?? holdingBases(candidate, { policy, platform, env, home })) {
    try {
      if (deviceOf(base) !== dev) continue; // checked before creating anything: a base on another volume would free nothing
      mkdirSync(base, { recursive: true });
      if (deviceOf(base) !== dev || lstatSync(base).isSymbolicLink()) continue;
      const marker = path.join(base, HOLDING_MARKER);
      if (!existsSync(marker)) writeFileSync(marker, "created by right-release disk; date-named children are purged after the retention window\n");
      const dir = path.join(base, date.toISOString().slice(0, 10));
      mkdirSync(dir, { recursive: true });
      return dir;
    } catch { /* try the next base */ }
  }
  return null;
}

export function moveEntry(source, destination) {
  mkdirSync(path.dirname(destination), { recursive: true });
  try { renameSync(source, destination); return "rename"; } catch (error) {
    if (error.code !== "EXDEV") throw error;
  }
  const before = walkSize(source);
  cpSync(source, destination, { recursive: true, preserveTimestamps: true, verbatimSymlinks: true, errorOnExist: true, force: false });
  if (walkSize(destination) < before) throw new Error(`copy verification failed for ${source}; source kept`);
  rmSync(source, { recursive: true, force: true }); // completes the move after verified copy
  return "copy-verify-remove";
}

export function logEvent(logFile, event) {
  try {
    mkdirSync(path.dirname(logFile), { recursive: true });
    appendFileSync(logFile, `${JSON.stringify({ ts: new Date().toISOString(), ...event })}\n`);
  } catch { /* logging must never fail the sweep */ }
}

export function applySweep({ plan, policy, platform = process.platform, deleteMode = false, date = new Date(), env = process.env, home = os.homedir(), move = moveEntry, pick = pickHolding, remove = (p) => rmSync(p, { recursive: true, force: true }) }) {
  const logFile = path.join(policy.heavyVolume[platformKey(platform)], "disk-hygiene.jsonl");
  const results = [];
  for (const c of plan.candidates) {
    try {
      // Re-verify at apply time: still the planned unit, not a symlink, still inside its root.
      if (!c.root || lstatSync(c.path).isSymbolicLink() || !isInsideRoot(c.path, c.root)) throw new Error("refused: path is not a sweepable unit inside its target root");
      if (deleteMode) { remove(c.path); results.push({ ...c, action: "delete", ok: true }); logEvent(logFile, { event: "disk.sweep.delete", ...c }); continue; }
      // An item stays on its own volume: moving it elsewhere frees nothing on the volume that is short of space.
      const holding = pick(c.path, { policy, platform, env, home, date });
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
