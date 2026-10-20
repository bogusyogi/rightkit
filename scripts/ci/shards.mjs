import { pnpmBuildClosureArgs } from "./select.mjs";
import { TEST_COMMAND } from "./test-results.mjs";

// Pure shard planning for the RightKit CI workflow (.github/workflows/ci.yml): which GitHub Actions jobs a
// push needs, per runner OS, and which gate tokens each job runs. No process, no filesystem: everything
// arrives as arguments, so the same functions back scripts/ci/ci-plan.mjs, the gate's `--select` parser and
// scripts/ci/shards.test.mjs.
//
// Rules the plan keeps:
//  - A crate or package is never split: all of its feature rows (ci/gate.config.json featureSets, the serial
//    rerun, clippy, the checkFeatureSets rows) run in ONE job, so one target directory and one cache entry
//    serve all of them.
//  - Groups are computed from the FULL list of units on the OS (not from the selection), so a unit lands in
//    the same job, with the same cache key, on every push. A push only chooses WHICH groups run and which
//    members of a group run inside it.
//  - Dependents that only have to keep compiling (`check` in select.mjs) are separate "closure" jobs.
//  - Root suites (node --test suites, the Swift root manifest describe) are one job; each Swift package is one
//    job on the macOS runner.

/** Defaults; ci/shards.json (optional) overrides any key. */
export const DEFAULT_SHARDS = Object.freeze({
  /** Target weight of one Rust job: about one cargo command per point (see rustWeight). A crate at or above it runs alone. */
  rustTargetWeight: 12,
  /** Target number of npm packages in one job (each package is weight 1). */
  npmTargetWeight: 6,
  /** Dependents compiled by one closure job (until closureMaxJobs is reached). */
  closureChunkSize: 10,
  /** At most this many closure jobs per runner OS: a big dependent set shares jobs rather than paying setup in many. */
  closureMaxJobs: 4,
  /** Units that always get a job of their own: crate names and npm directories. */
  solo: [],
});

export const HOSTS = Object.freeze(["windows", "macos", "linux"]);
export const STEPS = Object.freeze(["lock", "fmt", "test", "closure"]);
export const SWIFT_ROOT_MANIFEST = "swift/root-manifest";

/** Lower-case [a-z0-9-] text, safe in an artifact name, a cache key and a job name. */
export function sanitizeId(text) {
  return String(text).toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "x";
}

/**
 * How many cargo commands the gate runs for a crate on a host that applies to it: every test feature set
 * (doubled when the crate also reruns serially), clippy, and the feature-check rows. Mirrors rustGate in gate.mjs.
 */
export function rustWeight(entry = {}) {
  const featureSets = entry.featureSets ?? [[]];
  const tests = featureSets.length * (entry.alsoSerial ? 2 : 1);
  const checks = featureSets.some((features) => features.includes("--all-features")) ? 0 : (entry.checkFeatureSets ?? [["--all-features"]]).length;
  return tests + 1 + checks;
}

/**
 * Deterministic first-fit-decreasing packing. units: [{ name, weight }]. A unit that is `solo`, or whose weight
 * reaches `target`, gets its own group. Returns groups in creation order: [{ anchor, members, weight }], where
 * `anchor` (the heaviest member, ties by name) names the group, and `members` are sorted by name.
 */
export function groupUnits(units, target, solo = []) {
  const alone = new Set(solo);
  const ordered = [...units].sort((a, b) => b.weight - a.weight || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  const groups = [];
  for (const unit of ordered) {
    const fit = alone.has(unit.name) || unit.weight >= target ? undefined : groups.find((g) => !g.sealed && g.weight + unit.weight <= target);
    if (fit) { fit.members.push(unit.name); fit.weight += unit.weight; continue; }
    groups.push({ anchor: unit.name, members: [unit.name], weight: unit.weight, sealed: alone.has(unit.name) || unit.weight >= target });
  }
  return groups.map(({ anchor, members, weight }) => ({ anchor, members: members.sort(), weight }));
}

const idOfCrate = (name) => sanitizeId(name.replace(/^rightkit-/, ""));
const idOfDir = (dir) => sanitizeId(dir.replace(/^packages\//, ""));

/** Config rows with an `os` list apply only on those hosts (gate.mjs `applies`). */
export const appliesOn = (config, host, name) => { const os = config?.rust?.[name]?.os; return !os || os.includes(host); };

/** Shard config: defaults, then ci/shards.json, validated. */
export function shardsConfig(overrides = {}) {
  const merged = { ...DEFAULT_SHARDS, ...overrides };
  for (const key of ["rustTargetWeight", "npmTargetWeight", "closureChunkSize", "closureMaxJobs"]) {
    if (!Number.isInteger(merged[key]) || merged[key] < 1) throw new Error(`ci/shards.json: ${key} must be a positive integer`);
  }
  if (!Array.isArray(merged.solo) || merged.solo.some((s) => typeof s !== "string")) throw new Error("ci/shards.json: solo must be a list of crate names and npm directories");
  return merged;
}

/**
 * The jobs one runner OS needs.
 *   host:      "windows" | "macos"
 *   label:     the runner label, used in cache keys and job ids ("windows-2025", "xcode-27")
 *   universe:  { rust: [crate names], npm: [package dirs] }   (every unit, from `gate.mjs --plan --all`)
 *   selection: { rust: { test, check }, npm: { test }, root: [suite ids], swift: [package ids] }   (gate --plan for this host)
 *   config:    ci/gate.config.json
 * Returns { shards, closure }: arrays of { id, kind, tokens, units, rust, swift, cacheKey } sorted by id.
 */
export function planHost({ host, label, universe, selection, config = {}, shards = DEFAULT_SHARDS }) {
  const settings = shardsConfig(shards);
  const out = [];
  const closure = [];
  const applies = (name) => appliesOn(config, host, name);

  // Rust: static groups over every crate that applies on this host.
  const rustUnits = (universe.rust ?? []).filter(applies).map((name) => ({ name, weight: rustWeight(config.rust?.[name]) }));
  const rustGroups = groupUnits(rustUnits, settings.rustTargetWeight, settings.solo);
  const tested = new Set((selection.rust?.test ?? []).filter(applies));
  for (const group of rustGroups) {
    const members = group.members.filter((name) => tested.has(name));
    if (!members.length) continue;
    const id = `rust-${idOfCrate(group.anchor)}`;
    out.push({ id, kind: "rust", tokens: members.map((n) => `rust:${n}`).join(" "), units: members.length, rust: true, swift: false, cacheKey: `rk-${label}-${id}` });
  }

  // Dependents that must still compile: the sorted list is dealt round-robin into ceil(n / chunk size) jobs, at most
  // closureMaxJobs. Their artifacts are mostly shared third-party crates, so a cache keyed by job number is reused.
  const dependents = [...new Set((selection.rust?.check ?? []).filter(applies))].sort();
  const jobs = Math.min(settings.closureMaxJobs, Math.ceil(dependents.length / settings.closureChunkSize));
  for (let index = 0; index < jobs; index += 1) {
    const members = dependents.filter((_, i) => i % jobs === index);
    const id = `closure-${index + 1}`;
    closure.push({ id, kind: "closure", tokens: members.map((n) => `rust-check:${n}`).join(" "), units: members.length, rust: true, swift: false, cacheKey: `rk-${label}-${id}` });
  }

  // npm: static groups of packages; each job builds only its packages' workspace dependency closure.
  const npmGroups = groupUnits((universe.npm ?? []).map((dir) => ({ name: dir, weight: 1 })), settings.npmTargetWeight, settings.solo);
  const npmTested = new Set(selection.npm?.test ?? []);
  for (const group of npmGroups) {
    const members = group.members.filter((dir) => npmTested.has(dir));
    if (!members.length) continue;
    const id = `npm-${idOfDir(group.anchor)}`;
    out.push({ id, kind: "npm", tokens: members.map((d) => `npm:${d}`).join(" "), units: members.length, rust: false, swift: false, cacheKey: "" });
  }

  // Root suites, and the Swift root-manifest describe (a few seconds, macOS only).
  const rootTokens = [...(selection.root ?? []).map((id) => `root:${id}`), ...((selection.swift ?? []).includes(SWIFT_ROOT_MANIFEST) ? [`swift:${SWIFT_ROOT_MANIFEST}`] : [])];
  if (rootTokens.length) out.push({ id: "root", kind: "root", tokens: rootTokens.join(" "), units: rootTokens.length, rust: false, swift: false, cacheKey: "" });

  // Each Swift package builds and tests in its own job (macOS only; DerivedData cache).
  for (const dir of (selection.swift ?? []).filter((d) => d !== SWIFT_ROOT_MANIFEST)) {
    out.push({ id: `swift-${sanitizeId(dir.replace(/^swift\//, ""))}`, kind: "swift", tokens: `swift:${dir}`, units: 1, rust: false, swift: true, cacheKey: "" });
  }

  const byId = (a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
  // Never silently drop a selected unit because its universe entry or group is missing.
  const expected = [...tested].map((name) => `rust:${name}`).concat(dependents.map((name) => `rust-check:${name}`), [...npmTested].map((dir) => `npm:${dir}`), rootTokens, (selection.swift ?? []).filter((dir) => dir !== SWIFT_ROOT_MANIFEST).map((dir) => `swift:${dir}`)).sort();
  const actual = [...out, ...closure].flatMap((job) => job.tokens.split(" ")).sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${host}: shard plan does not cover every selected unit exactly once`);
  if (new Set([...out, ...closure].map((job) => job.id)).size !== out.length + closure.length) throw new Error(`${host}: duplicate shard id`);
  if (out.length > 256 || closure.length > 256) throw new Error(`${host}: matrix exceeds 256 jobs`);
  return { shards: out.sort(byId), closure: closure.sort(byId) };
}

/** Hygiene (one job, every selected crate on any OS): the union of both hosts' tested crates and npm packages. */
export function planHygiene(selections) {
  const crates = [...new Set(selections.flatMap((s) => s.rust?.test ?? []))].sort();
  const packages = [...new Set(selections.flatMap((s) => s.npm?.test ?? []))].sort();
  return { crates, packages, tokens: crates.map((n) => `rust:${n}`).join(" ") };
}

/**
 * Parse the gate's `--select` list. Tokens (space separated):
 *   rust:<crate>        test, clippy and feature rows of a crate        rust-check:<crate>   compile a dependent
 *   npm:<dir>           npm package tests (dir as in packages/<name>)    root:<suite id>      root node suites
 *   swift:<package id>  Swift package, or swift/root-manifest
 * `known` holds the allowed values: { rust: Set|array, npm, root, swift }. Throws on an unknown token or value.
 */
export function parseSelectTokens(text, known) {
  const sets = Object.fromEntries(["rust", "npm", "root", "swift"].map((k) => [k, new Set(known?.[k] ?? [])]));
  const selection = { rust: { all: false, test: [], check: [] }, npm: { all: false, test: [] }, root: [], swift: [] };
  const seen = new Set();
  for (const token of String(text ?? "").split(/\s+/).filter(Boolean)) {
    if (seen.has(token)) continue;
    seen.add(token);
    const colon = token.indexOf(":");
    const kind = colon > 0 ? token.slice(0, colon) : "";
    const value = colon > 0 ? token.slice(colon + 1) : "";
    const family = kind === "rust-check" ? "rust" : kind;
    if (!["rust", "rust-check", "npm", "root", "swift"].includes(kind) || !value) throw new Error(`unknown selection token ${JSON.stringify(token)}`);
    if (!sets[family].has(value)) throw new Error(`${kind} selection names ${JSON.stringify(value)}, which is not a known ${family} unit`);
    if (kind === "rust") selection.rust.test.push(value);
    else if (kind === "rust-check") selection.rust.check.push(value);
    else if (kind === "npm") selection.npm.test.push(value);
    else if (kind === "root") selection.root.push(value);
    else selection.swift.push(value);
  }
  const clash = selection.rust.test.filter((n) => selection.rust.check.includes(n));
  if (clash.length) throw new Error(`${clash.join(", ")} selected both as rust: and rust-check:`);
  return selection;
}

/** Parse `--steps` ("lock,fmt,test,closure"); default is every step. */
export function parseSteps(text) {
  if (text === undefined || text === "") return new Set(STEPS);
  const steps = String(text).split(/[\s,]+/).filter(Boolean);
  const bad = steps.filter((s) => !STEPS.includes(s));
  if (bad.length || steps.length === 0) throw new Error(`unknown step(s) ${bad.map((s) => JSON.stringify(s)).join(", ") || "(none)"}; steps are ${STEPS.join(", ")}`);
  return new Set(steps);
}

/** Final aggregate: skips are permitted only for explicitly empty matrices. */
export function summaryVerdict(needs, { required = ["plan"], counts = {} } = {}) {
  const failures = [];
  for (const name of new Set([...required, ...Object.keys(counts), ...Object.keys(needs ?? {})])) {
    const result = needs?.[name]?.result;
    if (Object.hasOwn(counts, name)) {
      const count = counts[name];
      if (!Number.isInteger(count) || count < 0) failures.push(`${name}: invalid planned count`);
      else if (result !== (count === 0 ? "skipped" : "success")) failures.push(`${name}: ${result ?? "missing"} (planned ${count})`);
    } else if (result !== "success") failures.push(`${name}: ${result ?? "missing"}`);
  }
  return { ok: failures.length === 0, failures };
}

/** Freeze every gate command at planning time, including serial, feature & dependency-build rows. */
export function requiredShardCommands(tokens, { config = {}, packages = [], swiftTests = [] } = {}) {
  const commands = [];
  const add = (unit, executable, args) => commands.push({ unit, command: `${executable} ${args.join(" ")}` });
  const parts = tokens.split(/\s+/).filter(Boolean).map((token) => { const colon = token.indexOf(":"); return [token.slice(0, colon), token.slice(colon + 1)]; });
  const npm = parts.filter(([kind]) => kind === "npm").map(([, dir]) => {
    const pkg = packages.find((entry) => entry.dir === dir);
    if (!pkg) throw new Error(`no package manifest for ${dir}`);
    return pkg;
  });
  if (npm.length) add("npm:build", "pnpm", pnpmBuildClosureArgs(npm.map((pkg) => pkg.name)));
  for (const [kind, name] of parts) {
    const unit = kind === "rust-check" ? `rust:${name}` : `${kind}:${name}`;
    const entry = config.rust?.[name] ?? {};
    if (kind === "rust") {
      const sets = entry.featureSets ?? [[]];
      if (!sets.length) throw new Error(`${name}: empty featureSets`);
      for (const features of sets) {
        add(unit, "cargo", ["test", "-p", name, ...features, "--no-fail-fast", "--", "--show-output"]);
        if (entry.alsoSerial) add(unit, "cargo", ["test", "-p", name, ...features, "--no-fail-fast", "--", "--test-threads=1", "--show-output"]);
      }
      add(unit, "cargo", ["clippy", "-p", name, "--no-deps", "--all-targets", ...sets[0], "--", "-D", "warnings"]);
      for (const features of sets.some((features) => features.includes("--all-features")) ? [] : entry.checkFeatureSets ?? [["--all-features"]]) add(unit, "cargo", ["check", "-p", name, "--all-targets", ...features]);
    } else if (kind === "rust-check") {
      add(unit, "cargo", ["check", "-p", name, "--all-targets"]);
      for (const features of entry.checkFeatureSets ?? [["--all-features"]]) add(unit, "cargo", ["check", "-p", name, "--all-targets", ...features]);
    } else if (kind === "npm") {
      if (npm.find((pkg) => pkg.dir === name).scripts?.test) add(unit, "pnpm", ["--dir", name, "run", "test"]);
    } else if (kind === "root") {
      if (name === "scripts") add(unit, "node", ["--test", "scripts/**/*.test.mjs"]);
      else add(unit, "pnpm", ["run", name]);
    } else if (kind === "swift") {
      if (name === SWIFT_ROOT_MANIFEST) add(unit, "swift", ["package", "describe", "--type", "json"]);
      else { add(unit, "swift", ["build"]); if (swiftTests.includes(name)) add(unit, "swift", ["test"]); }
    } else throw new Error(`unknown shard token kind ${kind}`);
  }
  if (!commands.length) throw new Error("nonempty shard has no required commands");
  return commands;
}

/** Reports must prove exactly every frozen command, plus internally consistent aggregate rows. */
export function evidenceVerdict(expected, reports, sha) {
  const failures = [];
  if (!Array.isArray(expected)) return { ok: false, failures: ["missing evidence plan"] };
  const artifacts = new Set();
  const count = (value) => Number.isSafeInteger(value) && value >= 0;
  for (const job of expected) {
    if (!job?.artifact || artifacts.has(job.artifact)) { failures.push("duplicate or invalid evidence job"); continue; }
    artifacts.add(job.artifact);
    const fail = (reason) => failures.push(`${job.artifact}: ${reason}`);
    const report = reports[job.artifact];
    if (!report) { fail("missing shard report"); continue; }
    if (report.sha !== sha || report.host !== job.host || report.selection !== job.tokens || report.steps !== job.steps) fail("report does not match planned commit, host, selection & steps");
    if (!Array.isArray(job.commands) || !job.commands.length || job.commands.some((command) => typeof command?.unit !== "string" || !command.unit || typeof command?.command !== "string" || !command.command) || !Array.isArray(report.results) || !report.results.length || !Array.isArray(report.rows) || !report.rows.length) { fail("empty or incomplete command proof"); continue; }
    const signature = (row) => JSON.stringify([row?.unit, row?.command]);
    const required = job.commands.map(signature).sort();
    if (JSON.stringify(required) !== JSON.stringify(report.results.map(signature).sort())) fail("command coverage differs from plan");
    for (const result of report.results) {
      if (!result || result.ok !== true || result.code !== 0 || result.timedOut !== false || !count(result.seconds) || result.isTest !== TEST_COMMAND.test(result.command ?? "") || ["executed", "failed", "ignored", "skipped", "binaries"].some((key) => !count(result[key])) || result.failed !== 0) fail(`failed or incomplete command ${result?.command ?? "missing"}`);
    }
    const units = new Set(job.commands.map((command) => command.unit));
    const seen = new Set();
    for (const row of report.rows) {
      if (!row || seen.has(row.id) || !units.has(row.id)) { fail("unexpected or duplicate unit row"); continue; }
      seen.add(row.id);
      const results = report.results.filter((result) => result?.unit === row.id);
      if (row.ok !== true || typeof row.known !== "boolean" || !Array.isArray(row.failed) || row.failed.length || !count(row.seconds) || !count(row.skipped) || row.seconds !== results.reduce((sum, result) => sum + result.seconds, 0) || row.skipped !== results.reduce((sum, result) => sum + result.skipped, 0)) fail(`failed or inconsistent unit row ${row.id}`);
    }
    if (seen.size !== units.size) fail("missing unit row");
  }
  return { ok: failures.length === 0, failures };
}
