// Pure selection logic for the RightKit CI gate: which crates, packages and Swift packages a set
// of changed paths touches. No process, no filesystem: everything arrives as arguments, so the
// same functions back the gate, its `--plan` mode and the unit test.

/** Paths that change what every Rust unit means. */
export const RUST_GLOBAL = [
  /^Cargo\.toml$/, /^rust-toolchain(\.toml)?$/, /^\.cargo\//, /^\.rightgit\.json$/,
  /^\.github\/workflows\//, /^scripts\/gate\.sh$/, /^scripts\/ci\/(gate|select)\.mjs$/,
];
/** Paths that change what every npm unit means. */
export const NPM_GLOBAL = [
  /^package\.json$/, /^pnpm-lock\.yaml$/, /^pnpm-workspace\.yaml$/, /^\.rightgit\.json$/,
  /^\.github\/workflows\//, /^scripts\/gate\.sh$/, /^scripts\/ci\/(gate|select)\.mjs$/,
];
/** Files inside a unit that cannot change its behaviour. */
const INERT = [/(^|\/)CHANGELOG\.md$/, /(^|\/)runtime-budget\.evidence\//];

const inert = (p) => INERT.some((re) => re.test(p));
const under = (p, dir) => p === dir || p.startsWith(`${dir}/`);

/**
 * Parse Cargo.lock into name -> { signatures: string[], deps: Set<name> }.
 * A package's signature covers version, source, checksum and its dependency list, so any change
 * to what gets compiled changes it. Dependency edges are by name: over-approximate, never under.
 */
export function parseLock(text) {
  const packages = new Map();
  for (const block of String(text ?? "").split(/^\[\[package\]\]\s*$/m).slice(1)) {
    const name = /^name = "([^"]+)"/m.exec(block)?.[1];
    if (!name) continue;
    const field = (key) => new RegExp(`^${key} = "([^"]*)"`, "m").exec(block)?.[1] ?? "";
    const depsRaw = /^dependencies = \[([^\]]*)\]/m.exec(block)?.[1] ?? "";
    const deps = [...depsRaw.matchAll(/"([^"\s]+)[^"]*"/g)].map((m) => m[1]);
    const entry = packages.get(name) ?? { signatures: [], deps: new Set() };
    entry.signatures.push([field("version"), field("source"), field("checksum"), deps.join(",")].join("|"));
    for (const dep of deps) entry.deps.add(dep);
    packages.set(name, entry);
  }
  for (const entry of packages.values()) entry.signatures.sort();
  return packages;
}

/** Names whose lock entry is new or different in `next`. */
export function changedLockNames(previous, next) {
  const changed = new Set();
  for (const [name, entry] of next) {
    const before = previous.get(name);
    if (!before || before.signatures.join("\n") !== entry.signatures.join("\n")) changed.add(name);
  }
  return changed;
}

/** Every package name reachable from `name` through the lock graph, `name` excluded. */
export function reach(lock, name, memo = new Map()) {
  if (memo.has(name)) return memo.get(name);
  const seen = new Set();
  const stack = [...(lock.get(name)?.deps ?? [])];
  while (stack.length) {
    const next = stack.pop();
    if (seen.has(next)) continue;
    seen.add(next);
    for (const dep of lock.get(next)?.deps ?? []) stack.push(dep);
  }
  seen.delete(name);
  memo.set(name, seen);
  return seen;
}

/**
 * members: [{ name, dir, extraPaths?: string[] }]. Returns { all, test: name[], check: name[] }.
 * `test`: the crate itself changed, a third-party package it compiles changed, or its gate config
 * changed. `check`: it depends on a `test` crate and must still compile.
 */
export function selectRust({ changedPaths, members, lock, lockChanged = new Set(), configChanged = new Set(), all = false }) {
  const names = members.map((m) => m.name).sort();
  if (all || changedPaths.some((p) => RUST_GLOBAL.some((re) => re.test(p)))) {
    return { all: true, test: names, check: [] };
  }
  const memberNames = new Set(names);
  const thirdParty = new Set([...lockChanged].filter((n) => !memberNames.has(n)));
  const memo = new Map();
  const test = new Set();
  for (const member of members) {
    const dirs = [member.dir, ...(member.extraPaths ?? []).map((p) => p.replace(/\/$/, ""))];
    const touched = changedPaths.some((p) => !inert(p) && dirs.some((dir) => under(p, dir)));
    const closure = reach(lock, member.name, memo);
    if (touched || configChanged.has(member.name) || [...thirdParty].some((n) => closure.has(n))) test.add(member.name);
  }
  const check = names.filter((name) => !test.has(name) && [...test].some((t) => reach(lock, name, memo).has(t)));
  return { all: false, test: [...test].sort(), check };
}

/**
 * packages: [{ name, dir, deps: string[] }] (deps: workspace package names of any kind).
 * A changed package selects itself and everything that depends on it, transitively.
 */
export function selectNpm({ changedPaths, packages, all = false }) {
  const dirs = packages.map((p) => p.dir).sort();
  if (all || changedPaths.some((p) => NPM_GLOBAL.some((re) => re.test(p)))) return { all: true, test: dirs };
  const selected = new Set(packages.filter((pkg) => changedPaths.some((p) => !inert(p) && under(p, pkg.dir))).map((p) => p.name));
  for (let grew = true; grew;) {
    grew = false;
    for (const pkg of packages) {
      if (!selected.has(pkg.name) && pkg.deps.some((dep) => selected.has(dep))) { selected.add(pkg.name); grew = true; }
    }
  }
  return { all: false, test: packages.filter((p) => selected.has(p.name)).map((p) => p.dir).sort() };
}

/** suites: [{ id, paths: string[] }]. Selected when any of its paths changed, or on a full run. */
export function selectByPaths({ changedPaths, suites, all = false }) {
  return suites.filter((s) => all || changedPaths.some((p) => !inert(p) && s.paths.some((dir) => under(p, dir.replace(/\/$/, ""))))).map((s) => s.id);
}

/** Crate names whose entry differs between two gate configs. */
export function changedConfigCrates(previous, next) {
  const a = previous?.rust ?? {}; const b = next?.rust ?? {};
  return new Set([...new Set([...Object.keys(a), ...Object.keys(b)])].filter((k) => JSON.stringify(a[k]) !== JSON.stringify(b[k])));
}
