// Executing scans follow the selected unit's committed dependency closure. File-only scans stay repository-wide.
// A Cargo scan (metadata, package list, deny, audit) executes cargo, through the broker on a broker host: it is an executing scan,
// never a file-only one, and follows the Rust closure (no Rust unit in the closure, no Cargo scan).
// Unknown local dependency edges refuse; they never silently omit checks or authorize every repository unit.
import path from "node:path";
import { createHash } from "node:crypto";
import { parse } from "smol-toml";
import { digestScanSet } from "./scan-digest.mjs";

const posix = (p) => String(p).replaceAll("\\", "/");
const within = (dir, base) => base === "." || dir === base || dir.startsWith(`${base}/`);
const manifestAt = (dir, file) => path.posix.join(dir, file);
const problem = (message) => new Error(`RK_SCOPE_CLOSURE: ${message}`);
const tableNames = ["dependencies", "dev-dependencies", "build-dependencies"];
const dependencyTables = (m) => [m, ...Object.values(m.target ?? {})].flatMap((holder) => tableNames.map((name) => holder?.[name] ?? {}));
const toml = (source, file = "Cargo.toml") => { try { return parse(String(source)); } catch (e) { throw problem(`${file} cannot be parsed: ${e.message}`); } };
const localDir = (base, relative) => {
  if (typeof relative !== "string" || !relative || path.posix.isAbsolute(posix(relative)) || /^[A-Za-z]:/.test(relative)) throw problem(`invalid local dependency path ${JSON.stringify(relative)} in ${base}`);
  const dir = path.posix.normalize(path.posix.join(base, posix(relative)));
  if (dir === ".." || dir.startsWith("../")) throw problem(`dependency ${relative} in ${base} leaves the committed repository`);
  return dir;
};

export function manifestDependencyFacts(source) {
  const m = toml(source); const paths = []; let inherits = false;
  for (const table of dependencyTables(m)) for (const spec of Object.values(table)) {
    if (spec && typeof spec === "object") {
      if (spec.path !== undefined) { if (typeof spec.path !== "string") throw problem("dependency path must be a string"); paths.push(spec.path); }
      if (spec.workspace === true) inherits = true;
    }
  }
  return { paths, inherits };
}

/** Source-only workspace membership for discovery. No Cargo process is needed for unrelated workspaces. */
export function cargoMetadataFromManifests({ manifests, readFile }) {
  const docs = new Map();
  for (const file of manifests) {
    let source;
    try { source = readFile(file); } catch (e) { throw problem(`${file} cannot be read: ${e.message}`); }
    docs.set(path.posix.dirname(file), toml(source, file));
  }
  const glob = (pattern, value) => {
    let re = "";
    const p = posix(pattern).replace(/^\.\//, "").replace(/\/$/, "");
    for (let i = 0; i < p.length; i += 1) {
      if (p[i] === "*") { if (p[i + 1] === "*") { re += ".*"; i += 1; } else re += "[^/]*"; }
      else if (p[i] === "?") re += "[^/]";
      else re += p[i].replace(/[.+^${}()|[\]\\]/g, "\\$&");
    }
    return new RegExp(`^${re}$`).test(value);
  };
  const owner = (dir) => {
    const m = docs.get(dir);
    if (m?.workspace) return dir;
    if (typeof m?.package?.workspace === "string") {
      const ws = localDir(dir, m.package.workspace);
      if (!docs.get(ws)?.workspace) throw problem(`${dir}/Cargo.toml names unknown workspace ${ws}`);
      return ws;
    }
    for (let at = path.posix.dirname(dir); at !== dir;) {
      const ws = docs.get(at)?.workspace;
      if (ws) {
        const rel = path.posix.relative(at, dir);
        if (!(ws.exclude ?? []).some((p) => glob(p, rel))) {
          // Cargo automatically includes path dependencies under the workspace root. An exact owner is confirmed by Cargo
          // for every selected workspace before scans; unrelated workspaces need only their source inventory for file-only checks.
          if ((ws.members ?? []).some((p) => glob(p, rel))) return at;
        }
      }
      if (at === ".") break;
      at = path.posix.dirname(at);
    }
    return dir;
  };
  return (dir) => {
    if (!docs.has(dir)) throw problem(`unknown Cargo manifest ${dir}/Cargo.toml`);
    const cargoDir = owner(dir);
    const members = [...docs].filter(([d, m]) => m.package && owner(d) === cargoDir).map(([d, m]) => {
      if (typeof m.package.name !== "string" || !m.package.name) throw problem(`${d}/Cargo.toml has no package name`);
      return { name: m.package.name, dir: d, manifest: manifestAt(d, "Cargo.toml"), publish: !(Array.isArray(m.package.publish) && m.package.publish.length === 0) };
    });
    return { cargoDir, members };
  };
}

/** npm/Rust unit names plus local dependency directories. readFile takes a repository-relative committed path. */
export function closureOf({ scopeDirs, npmUnits = [], rustUnits = [], readFile }) {
  const dirs = [...new Set(scopeDirs.map((d) => posix(d).replace(/^\.\//, "").replace(/\/$/, "") || "."))].sort();
  const npm = new Set(); const rust = new Set(); const dependencyDirs = new Set();
  const read = (dir, file, parser) => {
    const at = manifestAt(dir, file);
    try { return parser(String(readFile(at)).replace(/^\uFEFF/, "")); }
    catch (e) { throw problem(`${at} cannot resolve dependency closure: ${e.message}`); }
  };
  const npmDocs = new Map();
  const npmManifest = (u) => { if (!npmDocs.has(u.dir)) npmDocs.set(u.dir, read(u.dir, "package.json", JSON.parse)); return npmDocs.get(u.dir); };
  const byName = new Map(); const npmByDir = new Map(npmUnits.map((u) => [u.dir, u]));
  for (const u of npmUnits) { const m = npmManifest(u); if (m.name) byName.set(`${u.root ?? "."}\0${m.name}`, u); }
  const stack = npmUnits.filter((u) => dirs.some((d) => within(u.dir, d)));
  for (const u of stack) npm.add(u.name);
  while (stack.length) {
    const u = stack.pop(); dependencyDirs.add(u.dir);
    const m = npmManifest(u);
    for (const section of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) for (const [name, spec] of Object.entries(m[section] ?? {})) {
      if (typeof spec !== "string") throw problem(`${u.dir}/package.json dependency ${name} is not a string`);
      let dep = byName.get(`${u.root ?? "."}\0${name}`);
      if (/^(?:file|link):/.test(spec)) dep = npmByDir.get(localDir(u.dir, spec.slice(spec.indexOf(":") + 1)));
      else if (/^workspace:\.\.?\//.test(spec)) dep = npmByDir.get(localDir(u.dir, spec.slice("workspace:".length)));
      else if (spec.startsWith("workspace:")) {
        const alias = /^workspace:([^@~^*/][^@]*)@/.exec(spec);
        if (alias) dep = byName.get(`${u.root ?? "."}\0${alias[1]}`);
      }
      if (!dep && /^(?:workspace|file|link):/.test(spec)) throw problem(`${u.dir}/package.json dependency ${name} (${spec}) has no committed npm unit`);
      if (dep && !npm.has(dep.name)) { npm.add(dep.name); stack.push(dep); }
    }
  }
  const byDir = new Map(rustUnits.map((u) => [u.dir, u]));
  // A local npm dependency can own native Cargo sources too. Its executing checks & output binding share that same closure.
  const rustScopes = [...new Set([...dirs, ...dependencyDirs])];
  const queue = rustUnits.filter((u) => rustScopes.some((d) => within(u.dir, d)));
  for (const u of queue) rust.add(u.name);
  const seen = new Set();
  while (queue.length) {
    const u = queue.pop();
    if (seen.has(u.dir)) continue; seen.add(u.dir); dependencyDirs.add(u.dir);
    const m = read(u.dir, "Cargo.toml", parse);
    const workspaceDir = u.cargoDir ?? ".";
    let workspace;
    const workspaceManifest = () => workspace ??= (workspaceDir === u.dir ? m : read(workspaceDir, "Cargo.toml", parse));
    const local = (base, spec, name) => {
      if (!spec || typeof spec !== "object" || spec.path === undefined) return;
      const dir = localDir(base, spec.path); dependencyDirs.add(dir);
      const dep = byDir.get(dir);
      if (!dep) {
        // Excluded/vendor crates are valid dependency sources, not standalone scan units. Read their committed manifests &
        // follow their own edges; missing/unreadable paths still refuse before execution.
        const vendored = read(dir, "Cargo.toml", parse);
        if (typeof vendored.package?.name !== "string") throw problem(`${u.dir}/Cargo.toml local dependency ${name} at ${dir} has no committed package`);
        queue.push({ dir, cargoDir: vendored.workspace ? dir : workspaceDir, name: null });
      } else if (!rust.has(dep.name)) { rust.add(dep.name); queue.push(dep); }
    };
    for (const table of dependencyTables(m)) for (const [name, spec] of Object.entries(table)) {
      if (spec && typeof spec === "object" && spec.workspace === true) {
        const inherited = workspaceManifest().workspace?.dependencies?.[name];
        if (inherited === undefined) throw problem(`${u.dir}/Cargo.toml dependency ${name} has no [workspace.dependencies] entry in ${workspaceDir}`);
        local(workspaceDir, inherited, name);
      } else local(u.dir, spec, name);
    }
    // Workspace source overrides are compile inputs too, including renamed & target-qualified dependencies.
    const ws = workspaceManifest();
    for (const table of [...Object.values(ws.patch ?? {}), ws.replace ?? {}]) for (const [name, spec] of Object.entries(table)) local(workspaceDir, spec, name);
  }
  if (!npm.size && !rust.size) throw problem(`scope ${dirs.join(", ")} contains no committed npm or Rust unit`);
  return { npm, rust, dirs, dependencyDirs: [...dependencyDirs].sort(), widened: [] };
}

// The Class A Cargo scans deterministic-scans.mjs plans: `cargo-metadata|cargo-deny|cargo-audit[:<workspace label>]` run in the Cargo workspace
// named by the scan's `cwd` (none = the repository root workspace); `cargo-package-list:<unit name>` runs for one unit. The workspace label
// in an id is not always the directory (a crate-per-workspace layout labels by crate), so the workspace is read from `cwd`, never the id.
const CARGO_WORKSPACE_SCAN = /^cargo-(?:metadata|deny|audit)(?::.+)?$/;
const CARGO_PACKAGE_LIST = /^cargo-package-list:(.+)$/;
const cleanWorkspace = (dir) => posix(dir ?? ".").replace(/^\.\//, "").replace(/\/+$/, "") || ".";

/** A Class A scan that runs cargo (resolves metadata, lists a package, or runs cargo-deny / cargo-audit). */
export const isCargoScan = (s) => CARGO_WORKSPACE_SCAN.test(String(s.id)) || CARGO_PACKAGE_LIST.test(String(s.id));

/** A scan that runs repository code or cargo: tests, executing rows (tsc, eslint, biome, generators), the suite API contract, and every Cargo scan. */
export const isExecutingScan = (s) => s.executes === true || s.kind === "test" || s.id === "suite-api" || isCargoScan(s);

/** Keep file-only checks; filter every executing unit check (Cargo scans included) & refuse unclassified repository execution. */
export function scopeScans(scans, closure, { rustUnits = [] } = {}) {
  const names = rustUnits.map((u) => u.name).sort((a, b) => b.length - a.length);
  // The Cargo workspaces that hold a unit of the closure: a workspace-level Cargo scan (metadata, deny, audit) runs only for these.
  const closureWorkspaces = new Set(rustUnits.filter((u) => closure.rust.has(u.name)).map((u) => cleanWorkspace(u.cargoDir)));
  const kept = []; const dropped = [];
  for (const s of scans) {
    const npm = /^(?:tsc|eslint|biome|test:npm):(.+)$/.exec(s.id);
    const rust = /^(?:clippy|check|check-features|check-dependent|doc|test:rust|test:rust-serial):(.+)$/.exec(s.id);
    const cargoList = CARGO_PACKAGE_LIST.exec(s.id);
    let include = true;
    if (npm) include = closure.npm.has(npm[1]);
    else if (cargoList) {
      if (!names.includes(cargoList[1])) throw problem(`scan ${s.id} has no known Rust owner`);
      include = closure.rust.has(cargoList[1]);
    }
    else if (CARGO_WORKSPACE_SCAN.test(s.id)) include = closureWorkspaces.has(cleanWorkspace(s.cwd));
    else if (/^cargo-/.test(s.id)) throw problem(`executing scan ${s.id} has no exact scope owner`);
    else if (Array.isArray(s.batch?.units)) {
      const selected = s.batch.units.filter((n) => closure.rust.has(n));
      if (selected.length && selected.length !== s.batch.units.length && s.class !== "B") throw problem(`mixed executing batch ${s.id} cannot be scoped exactly`);
      // --scope accepts Class A snapshot checks only. A mixed Class B batch never runs in this route; retain no partial command
      // that could compile unrelated units. Scoped B execution is refused by the gate before planning.
      include = selected.length === s.batch.units.length;
    }
    else if (rust) { const name = names.find((n) => rust[1] === n || rust[1].startsWith(`${n}:`)); if (!name) throw problem(`scan ${s.id} has no known Rust owner`); include = closure.rust.has(name); }
    else if (s.class === "B" || ["suite-api", "generated-docs", "test:scripts-ci"].includes(s.id)) include = closure.dirs.includes(".");
    else if (s.executes === true || s.kind === "test") {
      if (closure.dirs.includes(".")) include = true;
      else if (typeof s.cwd === "string") include = closure.dependencyDirs.includes(s.cwd);
      else throw problem(`executing scan ${s.id} has no exact scope owner`);
    }
    const row = include && Array.isArray(s.tscWorkspace) ? { ...s, tscWorkspace: s.tscWorkspace.filter((d) => closure.dependencyDirs.includes(d)) } : s;
    (include ? kept : dropped).push(row);
  }
  return { scans: kept, dropped };
}

/** Scope identity also binds scan selection so prior Class A evidence cannot admit another scope. */
export function scopedPlan(plan, scans, closure) {
  const digest = (rows) => createHash("sha256").update(JSON.stringify(rows)).digest("hex");
  const scopeDigest = digest({ dirs: closure.dirs, npm: [...closure.npm].sort(), rust: [...closure.rust].sort(), dependencyDirs: closure.dependencyDirs });
  return { ...plan, scans, scopeDigest, deferred: plan.deferred.filter((id) => scans.some((s) => s.id === id)), inputDigest: digest([plan.inputDigest, scopeDigest]), scanSetDigest: digestScanSet(scans), classDigests: { A: digestScanSet(scans.filter((s) => s.class === "A")), B: digestScanSet(scans.filter((s) => s.class === "B")) } };
}
