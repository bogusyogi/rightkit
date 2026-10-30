// GB-15 (GATE 9/86, E17): a stage for one app or unit runs the Class A scans that EXECUTE repository code (tests, typecheck, lint,
// compile checks) for that app's dependency closure only; an unrelated unit's red test cannot block it. Everything file-only (secret
// scan, license, path policy, shellcheck, deny, audit, changelog ...) stays repository-wide. The closure is read from the committed
// manifests; whatever cannot be read exactly is widened (the whole Cargo workspace, every unit), never narrowed.
import path from "node:path";
import { rustUnitOfScanId } from "./bin/rightkit-gate.mjs";

const posix = (p) => String(p).split(path.sep).join("/");
const within = (dir, base) => base === "." || base === "" || dir === base || dir.startsWith(`${base}/`);
const DEP_TABLE = /^\[(?:target\.[^\]]+\.)?(?:dev-|build-)?dependencies(?:\.[^\]]+)?\]$/;
const WORKSPACE_DEPS_TABLE = /^\[workspace\.dependencies(?:\.[^\]]+)?\]$/;

/** { paths: [relative path dependency], inherits: true when a dependency is taken from the workspace table } for one Cargo.toml. */
export function manifestDependencyFacts(source) {
  const paths = []; let inherits = false; let table = "";
  for (const raw of String(source).split(/\r?\n/)) {
    const line = raw.replace(/#.*$/, "").trim();
    if (!line) continue;
    if (line.startsWith("[")) { table = line; continue; }
    if (!(DEP_TABLE.test(table) || WORKSPACE_DEPS_TABLE.test(table))) continue;
    for (const m of line.matchAll(/\bpath\s*=\s*"([^"]+)"/g)) paths.push(m[1]);
    if (/\bworkspace\s*=\s*true\b/.test(line)) inherits = true;
  }
  return { paths, inherits };
}

/**
 * The units a stage for the units under `scopeDirs` reads: { npm: Set<unit name>, rust: Set<unit name>, widened: [reason] }.
 * npmUnits: [{ name, dir, root }], rustUnits: [{ name, dir, cargoDir }]; `readFile(repoRelative)` returns text (throws when absent).
 */
export function closureOf({ scopeDirs, npmUnits = [], rustUnits = [], readFile }) {
  const dirs = scopeDirs.map((d) => posix(d).replace(/^\.\//, "").replace(/\/$/, "") || ".");
  const widened = [];
  const npm = new Set(); const rust = new Set();
  const manifest = (u) => { try { return JSON.parse(String(readFile(path.posix.join(u.dir === "." ? "" : u.dir, "package.json"))).replace(/^﻿/, "")); } catch { return null; } };
  const byName = new Map();
  for (const u of npmUnits) { const m = manifest(u); if (m?.name) byName.set(`${u.root ?? "."}\0${m.name}`, u); }
  const stack = npmUnits.filter((u) => dirs.some((d) => within(u.dir, d)));
  for (const u of stack) npm.add(u.name);
  while (stack.length) {
    const u = stack.pop(); const m = manifest(u);
    if (!m) { widened.push(`${u.name}: package.json unreadable; every npm unit is in scope`); for (const x of npmUnits) npm.add(x.name); break; }
    for (const section of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) {
      for (const name of Object.keys(m[section] ?? {})) {
        const dep = byName.get(`${u.root ?? "."}\0${name}`);
        if (dep && !npm.has(dep.name)) { npm.add(dep.name); stack.push(dep); }
      }
    }
  }
  const byDir = new Map(rustUnits.map((u) => [u.dir, u]));
  const queue = rustUnits.filter((u) => dirs.some((d) => within(u.dir, d)));
  for (const u of queue) rust.add(u.name);
  const widenWorkspace = (u, why) => { widened.push(`${u.name}: ${why}`); for (const x of rustUnits) if ((x.cargoDir ?? ".") === (u.cargoDir ?? ".") && !rust.has(x.name)) { rust.add(x.name); queue.push(x); } };
  const seen = new Set();
  while (queue.length) {
    const u = queue.pop();
    if (seen.has(u.name)) continue; seen.add(u.name);
    let facts;
    try { facts = manifestDependencyFacts(readFile(path.posix.join(u.dir === "." ? "" : u.dir, "Cargo.toml"))); } catch { widenWorkspace(u, "Cargo.toml unreadable; its whole Cargo workspace is in scope"); continue; }
    if (facts.inherits) widenWorkspace(u, "takes dependencies from [workspace.dependencies]; its whole Cargo workspace is in scope");
    for (const p of facts.paths) {
      const target = byDir.get(path.posix.normalize(path.posix.join(u.dir, posix(p))));
      if (target && !rust.has(target.name)) { rust.add(target.name); queue.push(target); }
    }
  }
  return { npm, rust, widened };
}

const NPM_SCAN = /^(tsc|eslint|biome|test:npm):(.+)$/;

/** The plan's scans minus the executing ones that belong to a unit outside the closure. Returns { scans, dropped }. */
export function scopeScans(scans, closure, { rustUnits = [] } = {}) {
  const known = new Set(rustUnits.map((u) => u.name));
  const kept = []; const dropped = [];
  for (const s of scans) {
    const npm = NPM_SCAN.exec(s.id);
    if (npm) { (closure.npm.has(npm[2]) ? kept : dropped).push(s); continue; }
    const crate = rustUnitOfScanId(s.id, known);
    if (crate) { (closure.rust.has(crate) ? kept : dropped).push(s); continue; }
    kept.push(s);
  }
  return { scans: kept, dropped };
}
