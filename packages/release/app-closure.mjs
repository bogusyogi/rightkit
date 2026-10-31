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

/** Thrown when a `path` key of a dependency table cannot be read exactly; the closure widens on it, never omits the path. */
export class ManifestParseError extends Error {
  constructor(message, details = {}) { super(message); this.name = "ManifestParseError"; this.code = "APP_CLOSURE_MANIFEST_UNPARSEABLE"; Object.assign(this, details); }
}

const BASIC_ESCAPES = { b: "\b", t: "\t", n: "\n", f: "\f", r: "\r", '"': '"', "\\": "\\" };

/** The end index (exclusive) of the TOML single-line string opening at `start`, and its value; null when it is unterminated, multi-line or has a bad escape. */
function tomlString(text, start) {
  const quote = text[start];
  if (text.startsWith(quote.repeat(3), start)) return null; // multi-line string forms are not read
  let value = "";
  for (let i = start + 1; i < text.length; i += 1) {
    const ch = text[i];
    if (ch === quote) return { end: i + 1, value };
    if (quote === "'") { value += ch; continue; }
    if (ch !== "\\") { value += ch; continue; }
    const next = text[i + 1];
    if (next in BASIC_ESCAPES) { value += BASIC_ESCAPES[next]; i += 1; continue; }
    const width = next === "u" ? 4 : next === "U" ? 8 : 0;
    const hex = width ? text.slice(i + 2, i + 2 + width) : "";
    if (width && hex.length === width && /^[0-9a-fA-F]+$/.test(hex)) { try { value += String.fromCodePoint(Number.parseInt(hex, 16)); } catch { return null; } i += 1 + width; continue; }
    return null;
  }
  return null;
}

/** The line without its `#` comment (a `#` inside a string is not one). */
function stripComment(line) {
  for (let i = 0; i < line.length; i += 1) {
    const ch = line[i];
    if (ch === "#") return line.slice(0, i);
    if (ch === '"' || ch === "'") { const str = tomlString(line, i); if (!str) return line; i = str.end - 1; }
  }
  return line;
}

/** Every `path = <string>` key of one dependency-table line (inline tables, dotted and quoted keys included); throws for a `path` key whose value is not a single-line string. */
function pathsOfLine(line, where) {
  const found = [];
  const unparseable = (why) => new ManifestParseError(`${where}: cannot read the dependency path (${why}): ${line.trim()}`, { line: where, text: line.trim() });
  for (let i = 0; i < line.length; i += 1) {
    const ch = line[i];
    const boundary = i === 0 || /[\s{,.]/.test(line[i - 1]);
    if (boundary) {
      const key = /^(?:path|"path"|'path')\s*=\s*/.exec(line.slice(i));
      if (key) {
        const at = i + key[0].length;
        if (line[at] !== '"' && line[at] !== "'") throw unparseable("the value is not a quoted string");
        const str = tomlString(line, at);
        if (!str) throw unparseable("an unterminated, multi-line or badly escaped string");
        found.push(str.value); i = str.end - 1; continue;
      }
    }
    if (ch === '"' || ch === "'") { const str = tomlString(line, i); if (!str) { if (/^\s*(?:path|"path"|'path')\s*=/.test(line)) break; throw unparseable("an unterminated or multi-line string"); } i = str.end - 1; }
  }
  return found;
}

/** { paths: [relative path dependency], inherits: true when a dependency is taken from the workspace table } for one Cargo.toml. Throws ManifestParseError for a `path` key it cannot read. */
export function manifestDependencyFacts(source) {
  const paths = []; let inherits = false; let table = "";
  const lines = String(source).split(/\r?\n/);
  for (let index = 0; index < lines.length; index += 1) {
    const line = stripComment(lines[index]).trim();
    if (!line) continue;
    if (line.startsWith("[")) { table = line; continue; }
    if (!(DEP_TABLE.test(table) || WORKSPACE_DEPS_TABLE.test(table))) continue;
    paths.push(...pathsOfLine(line, `Cargo.toml line ${index + 1}`));
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
    try { facts = manifestDependencyFacts(readFile(path.posix.join(u.dir === "." ? "" : u.dir, "Cargo.toml"))); } catch (error) { widenWorkspace(u, error instanceof ManifestParseError ? `${error.message}; its whole Cargo workspace is in scope` : "Cargo.toml unreadable; its whole Cargo workspace is in scope"); continue; }
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
