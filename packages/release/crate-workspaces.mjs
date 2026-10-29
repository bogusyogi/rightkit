// One Cargo workspace and lock per RightKit crate (Adrian, 2026-10-29: "I want you to have 1 cargo dot lock per component";
// rightkit#1285). Every `crates/<x>` is its own workspace; crates that use each other link by path dependency across
// workspaces. This module reads manifests only (no cargo) and answers which crates exist, which sibling crates each one
// path-depends on, and which crates a change reaches: the changed crate plus every crate that path-depends on it, transitively.
// It imports nothing outside node: CI's plan job runs it before any package install, so it reads the few manifest keys it needs
// (the package name and dependency `path` values) line by line instead of through a TOML library.
import fs from "node:fs";
import path from "node:path";

const posix = (p) => p.split(path.sep).join("/");
const defaults = { readFile: (f) => fs.readFileSync(f, "utf8"), readdir: (d) => fs.readdirSync(d), exists: fs.existsSync };

/** [{ name, dir: "crates/<x>", manifest, lock }] for every crates/<x>/Cargo.toml, sorted by dir. */
export function crateWorkspaces(root, io = {}) {
  const { readFile, readdir, exists } = { ...defaults, ...io };
  const cratesDir = path.join(root, "crates");
  if (!exists(cratesDir)) return [];
  const out = [];
  for (const entry of [...readdir(cratesDir)].map(String).sort()) {
    if (entry.startsWith(".")) continue;
    const manifest = `crates/${entry}/Cargo.toml`;
    if (!exists(path.join(root, manifest))) continue;
    const name = manifestFacts(readFile(path.join(root, manifest))).name;
    if (typeof name !== "string") continue;
    out.push({ name, dir: `crates/${entry}`, manifest, lock: `crates/${entry}/Cargo.lock` });
  }
  return out;
}

const DEPENDENCY_TABLE = /^(?:target\.(?:'[^']*'|"[^"]*"|[^.\]]+)\.)?(?:dev-|build-)?dependencies$/;
const unquote = (v) => v.replace(/^(["'])(.*)\1$/, "$2");

/** { name, paths }: the [package] name and every dependency `path` value (inline tables and `[dependencies.<x>]` sections). */
export function manifestFacts(text) {
  let section = ""; let name; const paths = [];
  for (const raw of String(text).split(/\r?\n/)) {
    const line = raw.replace(/\s+#.*$/, "").trim();
    if (!line || line.startsWith("#")) continue;
    const header = line.match(/^\[\[?([^\[\]]+)\]\]?$/);
    if (header) { section = header[1].trim(); continue; }
    if (section === "package") { const m = line.match(/^name\s*=\s*("[^"]*"|'[^']*')/); if (m) name = unquote(m[1]); continue; }
    if (DEPENDENCY_TABLE.test(section)) {
      const m = line.match(/^[A-Za-z0-9_.-]+\s*=\s*\{(.*)\}$/);
      const p = m && m[1].match(/(?:^|,)\s*path\s*=\s*("[^"]*"|'[^']*')/);
      if (p) paths.push(unquote(p[1]));
      continue;
    }
    const dotted = section.match(/^(.*)\.([^.]+)$/);
    if (dotted && DEPENDENCY_TABLE.test(dotted[1])) { const m = line.match(/^path\s*=\s*("[^"]*"|'[^']*')/); if (m) paths.push(unquote(m[1])); }
  }
  return { name, paths };
}

/** Map<name, Set<name>>: each crate's direct path dependencies on sibling crates (normal, dev, build, any target). */
export function pathDependencies(root, io = {}, workspaces = crateWorkspaces(root, io)) {
  const { readFile } = { ...defaults, ...io };
  const byDir = new Map(workspaces.map((w) => [w.dir, w.name]));
  const deps = new Map(workspaces.map((w) => [w.name, new Set()]));
  for (const w of workspaces) {
    for (const dep of manifestFacts(readFile(path.join(root, w.manifest))).paths) {
      const target = byDir.get(posix(path.posix.normalize(path.posix.join(w.dir, posix(dep)))));
      if (target && target !== w.name) deps.get(w.name).add(target);
    }
  }
  return deps;
}

/** Set<name>: every crate that path-depends on any of `names`, transitively; `names` themselves are excluded. */
export function reverseDependents(names, deps) {
  const reverse = new Map();
  for (const [from, tos] of deps) for (const to of tos) (reverse.get(to) ?? reverse.set(to, new Set()).get(to)).add(from);
  const seen = new Set(); const queue = [...names];
  while (queue.length) {
    for (const dependent of reverse.get(queue.shift()) ?? []) {
      if (!seen.has(dependent)) { seen.add(dependent); queue.push(dependent); }
    }
  }
  for (const n of names) seen.delete(n);
  return seen;
}

/** The crate whose workspace holds `file` (repository-relative, posix), or null. */
export function crateOfFile(file, workspaces) {
  let best = null;
  for (const w of workspaces) if (file.startsWith(`${w.dir}/`) && (!best || w.dir.length > best.dir.length)) best = w;
  return best?.name ?? null;
}
