// Cargo manifest helpers for the release gates: package identity, workspace path-dependency
// closure and Cargo.lock dependency closure. Pure TOML parsing (smol-toml); nothing shells out to cargo.
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { parse } from "smol-toml";

const DEP_TABLES = ["dependencies", "build-dependencies", "dev-dependencies"];

/** Nearest ancestor (or self) directory whose Cargo.toml has a [workspace] table, or null. */
export function findWorkspaceRoot(dir, { exists = existsSync, readFile = readFileSync } = {}) {
  let current = path.resolve(dir);
  for (;;) {
    const manifest = path.join(current, "Cargo.toml");
    if (exists(manifest)) {
      try { if (parse(readFile(manifest, "utf8")).workspace) return current; } catch { /* unreadable: keep climbing */ }
    }
    const parent = path.dirname(current);
    if (parent === current) return null;
    current = parent;
  }
}

/**
 * `{ name, version, manifest }` from a crate's Cargo.toml, parsed (not pattern-matched).
 * `version.workspace = true` resolves through the workspace root's `[workspace.package].version`.
 * `version` is null when it cannot be resolved to a string.
 */
export function readCargoPackage(crateDir, deps = {}) {
  const { exists = existsSync, readFile = readFileSync } = deps;
  const manifest = parse(readFile(path.join(crateDir, "Cargo.toml"), "utf8"));
  const pkg = manifest.package ?? {};
  let version = pkg.version;
  if (version && typeof version === "object" && version.workspace === true) {
    const root = findWorkspaceRoot(crateDir, { exists, readFile });
    version = root ? parse(readFile(path.join(root, "Cargo.toml"), "utf8")).workspace?.package?.version : undefined;
  }
  return { name: pkg.name ?? null, version: typeof version === "string" && version.trim() ? version : null, manifest };
}

function depTables(manifest, includeDev) {
  const tables = [];
  const take = (holder) => {
    for (const key of DEP_TABLES) {
      if (key === "dev-dependencies" && !includeDev) continue;
      if (holder?.[key]) tables.push(holder[key]);
    }
  };
  take(manifest);
  for (const target of Object.values(manifest.target ?? {})) take(target);
  return tables;
}

/**
 * Absolute directories of the workspace crates `crateDir` depends on through `path` dependencies,
 * transitively. Dev-dependencies count for the crate itself (benches and tests use them) and not for
 * its dependencies. `{ workspace = true }` entries resolve through `[workspace.dependencies]`.
 */
export function pathDependencyDirs(crateDir, deps = {}) {
  const { exists = existsSync, readFile = readFileSync } = deps;
  const root = findWorkspaceRoot(crateDir, { exists, readFile });
  const workspaceDeps = root ? (parse(readFile(path.join(root, "Cargo.toml"), "utf8")).workspace?.dependencies ?? {}) : {};
  const found = new Set();
  const visit = (dir, includeDev) => {
    const manifestPath = path.join(dir, "Cargo.toml");
    if (!exists(manifestPath)) return;
    const manifest = parse(readFile(manifestPath, "utf8"));
    for (const table of depTables(manifest, includeDev)) {
      for (const spec of Object.values(table)) {
        if (!spec || typeof spec !== "object" || spec.path === undefined) continue;
        const target = path.resolve(dir, spec.path);
        if (found.has(target) || target === path.resolve(crateDir)) continue;
        found.add(target);
        visit(target, false);
      }
      // Workspace-inherited path dependencies, keyed by dependency name.
      for (const [name, spec] of Object.entries(table)) {
        if (spec && typeof spec === "object" && spec.workspace === true && workspaceDeps[name]?.path !== undefined) {
          const target = path.resolve(root, workspaceDeps[name].path);
          if (found.has(target) || target === path.resolve(crateDir)) continue;
          found.add(target);
          visit(target, false);
        }
      }
    }
  };
  visit(path.resolve(crateDir), true);
  return [...found].sort();
}

const depName = (entry) => String(entry).split(" ")[0];

/**
 * Canonical [[package]] entries of Cargo.lock reachable from `name`: the crate, every workspace crate
 * and every external crate in its dependency graph, with version, source, checksum and edges. Two
 * lock files give equal closures exactly when nothing in that graph changed.
 */
export function lockClosure(lockText, name) {
  const packages = parse(lockText).package ?? [];
  const byName = new Map();
  for (const entry of packages) {
    if (!byName.has(entry.name)) byName.set(entry.name, []);
    byName.get(entry.name).push(entry);
  }
  const seen = new Set();
  const out = [];
  const queue = [...(byName.get(name) ?? [])];
  while (queue.length) {
    const entry = queue.pop();
    const key = `${entry.name}@${entry.version}@${entry.source ?? ""}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ name: entry.name, version: entry.version, source: entry.source ?? null, checksum: entry.checksum ?? null, dependencies: [...(entry.dependencies ?? [])].sort() });
    for (const dependency of entry.dependencies ?? []) {
      const parts = String(dependency).split(" ");
      const candidates = byName.get(depName(dependency)) ?? [];
      const matching = parts[1] ? candidates.filter((c) => c.version === parts[1]) : candidates;
      queue.push(...(matching.length ? matching : candidates));
    }
  }
  return out.sort((a, b) => `${a.name}@${a.version}@${a.source}`.localeCompare(`${b.name}@${b.version}@${b.source}`));
}
