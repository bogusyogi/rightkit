import { closeSync, cpSync, existsSync, lstatSync, mkdirSync, opendirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, statfsSync, appendFileSync } from "node:fs";
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
    let roots;
    try { roots = writerMatches(writer, { env, home }); } catch { continue; }
    const units = writer.unit === "children"
      ? roots.flatMap((r) => { try { return readdirSync(r).map((n) => path.join(r, n)); } catch { return []; } })
      : roots;
    for (const unit of units) {
      if (seen.has(unit)) continue;
      seen.add(unit);
      const base = { writer: writer.id, path: unit };
      let st;
      try { st = lstatSync(unit); } catch { continue; }
      if (st.isSymbolicLink()) { skipped.push({ ...base, reason: "symlink" }); continue; }
      if (isProtectedPath(unit, policy)) { skipped.push({ ...base, reason: "protected" }); continue; }
      if (hasFreshContent(unit, cutoff)) { skipped.push({ ...base, reason: "fresh" }); continue; }
      if (lockCheck(unit)) { skipped.push({ ...base, reason: "in-use" }); continue; }
      candidates.push({ ...base, bytes: size(unit), mtime: new Date(st.mtimeMs).toISOString() });
    }
  }
  return { schema: 1, platform: key, staleDays, candidates, skipped, totalBytes: candidates.reduce((s, c) => s + c.bytes, 0) };
}

export function sweptDestination({ policy, platform = process.platform, date = new Date() }) {
  const key = platformKey(platform);
  return path.join(policy.heavyVolume[key], date.toISOString().slice(0, 10));
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

export function applySweep({ plan, policy, platform = process.platform, deleteMode = false, date = new Date(), move = moveEntry, remove = (p) => rmSync(p, { recursive: true, force: true }) }) {
  const destRoot = sweptDestination({ policy, platform, date });
  const logFile = path.join(policy.heavyVolume[platformKey(platform)], "disk-hygiene.jsonl");
  const results = [];
  for (const c of plan.candidates) {
    const rel = path.join(c.writer, path.basename(c.path));
    let destination = path.join(destRoot, rel);
    if (existsSync(destination)) destination = `${destination}.${date.getTime()}`;
    try {
      if (deleteMode) { remove(c.path); results.push({ ...c, action: "delete", ok: true }); logEvent(logFile, { event: "disk.sweep.delete", ...c }); }
      else { const how = move(c.path, destination); results.push({ ...c, action: "move", destination, how, ok: true }); logEvent(logFile, { event: "disk.sweep.move", ...c, destination, how }); }
    } catch (error) {
      results.push({ ...c, action: deleteMode ? "delete" : "move", ok: false, error: error.message });
      logEvent(logFile, { event: "disk.sweep.error", ...c, error: error.message });
    }
  }
  return { logFile, results, movedBytes: results.filter((r) => r.ok).reduce((s, r) => s + r.bytes, 0) };
}
