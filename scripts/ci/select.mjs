// Pure selection logic for the RightKit CI gate: which crates, packages and Swift packages a set
// of changed paths touches. No process, no filesystem: everything arrives as arguments, so the
// same functions back the gate, its `--plan` mode and the unit test.

/** Paths that change what every Rust unit means. */
export const RUST_GLOBAL = [
  /^Cargo\.toml$/, /^rust-toolchain(\.toml)?$/, /^\.cargo\//, /^\.rightgit\.json$/,
  /^\.github\/workflows\/ci\.yml$/, /^\.github\/actions\/rk-setup\//, /^scripts\/gate\.sh$/, /^scripts\/ci\/(gate|select|ci-plan|ci-summary|shards|secret-scan|test-results|provision-chrome|provision-ort)\.mjs$/, /^ci\/(shards\.json|chrome-for-testing\.json)$/, /^crates\/rightkit-ffmpeg\/catalog\/native-libraries\.json$/,
];
/** Paths that change what every npm unit means. */
export const NPM_GLOBAL = [
  /^package\.json$/, /^pnpm-lock\.yaml$/, /^pnpm-workspace\.yaml$/, /^\.rightgit\.json$/,
  /^\.github\/workflows\/ci\.yml$/, /^\.github\/actions\/rk-setup\//, /^scripts\/gate\.sh$/, /^scripts\/ci\/(gate|select|ci-plan|ci-summary|shards|secret-scan|test-results|provision-chrome|provision-ort)\.mjs$/, /^ci\/(shards\.json|chrome-for-testing\.json)$/, /^crates\/rightkit-ffmpeg\/catalog\/native-libraries\.json$/,
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

/**
 * Explicit unit selection for a publish verification run: named units are tested in the order
 * given; a parsed Cargo.lock graph is required, and reverse workspace dependents of tested crates
 * are added to `check`. `registry` is "crates" (units are crate names, `members` as for selectRust) or "npm" (units are package names, `packages` as
 * for selectNpm, each with `scripts`). Returns the shapes selectRust and selectNpm return, so the
 * gate runs them unchanged, plus `excluded` and `noTest`: the units the gate will NOT run on `host`
 * and has been explicitly allowed not to.
 *
 * Nothing is skipped silently. A crate whose `config.rust[name].os` omits `host` throws unless
 * `allowHostExclusion` names it; an npm package without a `test` script throws unless `allowNoTest`
 * names it; an allow entry naming a unit that is not in `units` throws.
 */
export function selectUnits({ registry, units, members = [], packages = [], lock = null, host = null, config = {}, allowHostExclusion = [], allowNoTest = [], allowIgnored = [], allowMissingAssets = [], assetGated = {} }) {
  if (!Array.isArray(units) || units.length === 0) throw new Error("no units named");
  const unique = [...new Set(units)];
  for (const [label, list] of [["allow_host_exclusion", allowHostExclusion], ["allow_no_test", allowNoTest], ["allow_ignored", allowIgnored], ["allow_missing_assets", allowMissingAssets]]) {
    const stray = list.filter((name) => !unique.includes(name));
    if (stray.length) throw new Error(`${label} names ${stray.join(", ")}, which are not units of this request`);
  }
  // A unit whose tests need assets this workflow does not provision (ci/asset-gated.json) runs only when
  // the request names it in allow_missing_assets; naming any other unit there is an error.
  const gated = assetGated?.[registry] ?? {};
  const unnamed = unique.filter((name) => Object.hasOwn(gated, name) && !allowMissingAssets.includes(name));
  if (unnamed.length) throw new Error(`${unnamed.join(", ")} need assets the publish workflow does not provision (ci/asset-gated.json); name each in allow_missing_assets`);
  const notGated = allowMissingAssets.filter((name) => !Object.hasOwn(gated, name));
  if (notGated.length) throw new Error(`allow_missing_assets names ${notGated.join(", ")}, which ci/asset-gated.json does not list for ${registry}`);
  if (registry === "crates") {
    if (!(lock instanceof Map)) throw new Error("crates selection requires the parsed Cargo.lock workspace graph to check reverse dependents");
    const incomplete = members.filter((member) => !lock.has(member.name)).map((member) => member.name);
    if (incomplete.length) throw new Error(`Cargo.lock workspace graph is incomplete; missing member(s): ${incomplete.join(", ")}`);
    const known = new Set(members.map((m) => m.name));
    const unknown = unique.filter((name) => !known.has(name));
    if (unknown.length) throw new Error(`unknown crate(s): ${unknown.join(", ")}`);
    if (allowNoTest.length) throw new Error("allow_no_test applies to npm packages only");
    const excluded = []; const refused = [];
    for (const name of unique) {
      const os = config.rust?.[name]?.os;
      if (!host || !os || os.includes(host)) continue;
      if (allowHostExclusion.includes(name)) excluded.push(name); else refused.push(`${name} (restricted to ${os.join(", ")})`);
    }
    if (refused.length) throw new Error(`${refused.join(", ")} would be skipped on ${host}; name each in allow_host_exclusion or lift the restriction`);
    const test = unique.filter((n) => !excluded.includes(n));
    const memo = new Map();
    const reverse = members.map((member) => member.name).filter((name) => !unique.includes(name) && test.some((target) => reach(lock, name, memo).has(target)));
    const restricted = reverse.filter((name) => {
      const os = config.rust?.[name]?.os;
      return host && os && !os.includes(host);
    });
    if (restricted.length) {
      const detail = restricted.map((name) => `${name} (restricted to ${config.rust[name].os.join(", ")})`).join(", ");
      throw new Error(`reverse-dependent check would be skipped on ${host}: ${detail}; lift the host restriction or select an allowed native verification path`);
    }
    return { rust: { all: false, test, check: reverse.sort() }, npm: { all: false, test: [] }, excluded, noTest: [] };
  }
  if (registry === "npm") {
    const byName = new Map(packages.map((p) => [p.name, p]));
    const unknown = unique.filter((name) => !byName.has(name));
    if (unknown.length) throw new Error(`unknown npm package(s): ${unknown.join(", ")}`);
    if (allowHostExclusion.length) throw new Error("allow_host_exclusion applies to crates only");
    const noTest = unique.filter((name) => !byName.get(name).scripts?.test);
    const refused = noTest.filter((name) => !allowNoTest.includes(name));
    if (refused.length) throw new Error(`${refused.join(", ")} has no test script, so nothing would verify it; name each in allow_no_test`);
    const unused = allowNoTest.filter((name) => !noTest.includes(name));
    if (unused.length) throw new Error(`allow_no_test names ${unused.join(", ")}, which has a test script`);
    return { rust: { all: false, test: [], check: [] }, npm: { all: false, test: unique.filter((n) => !noTest.includes(n)).map((name) => byName.get(name).dir) }, excluded: [], noTest };
  }
  throw new Error(`registry must be npm or crates, got ${JSON.stringify(registry)}`);
}

const BUILD_NAME = /^(?:@[a-z0-9][a-z0-9._-]{0,63}\/)?[a-z0-9][a-z0-9._-]{0,63}$/;

/**
 * pnpm arguments that build exactly the named workspace packages plus everything they depend on (`name...`
 * selects the package and its workspace dependency closure), in dependency order, one at a time. Nothing
 * outside that closure is built, so an unrelated package that does not build cannot block a publish.
 *
 * `--if-present` lets a selected package without a `build` script pass (a plain-JS unit such as @rightkit/release,
 * whose closure may hold no `build` script at all, otherwise fails with ERR_PNPM_RECURSIVE_RUN_NO_SCRIPT and
 * nothing can be packed); a `build` script that exists and fails still fails the run. It goes before `run`:
 * pnpm forwards options placed after the script name to the script itself.
 */
export function pnpmBuildClosureArgs(names) {
  const unique = [...new Set(names ?? [])];
  if (unique.length === 0) throw new Error("no packages to build");
  // ".." is refused as well: pnpm reads a trailing or leading "..." as a dependency/dependent selector.
  const bad = unique.filter((name) => typeof name !== "string" || !BUILD_NAME.test(name) || name.includes(".."));
  if (bad.length) throw new Error(`refusing to build unsafe package name(s): ${bad.map(String).join(", ")}`);
  return ["--workspace-concurrency=1", "--if-present", ...unique.flatMap((name) => ["--filter", `${name}...`]), "run", "build"];
}
