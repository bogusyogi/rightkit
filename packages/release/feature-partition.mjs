// Feature-equivalence partition of a class B group (GATE-CONTRACT GC-A18; rightkit#1538 finding 3, reset plan section 8).
//
// One cargo invocation naming several crates (`-p a -p b`) unifies features: a dependency both crates use is compiled once with the union of
// the features each enables, so `a` is checked against more of `d` than it enables alone. A grouped pass is a pass for `a` alone only when,
// for every package in `a`'s own graph, the group resolves the same feature set `a` resolves on its own (features are additive, so the group
// can only enable more). Cargo's own resolver answers that: `cargo tree -p a` lists each package of `a`'s selection with its resolved
// features, `cargo tree -p a -p b` lists the group's. The planner compares the two per member and splits a group on the first difference:
// the members that are equivalent stay one invocation (removing a member only removes features, so their own resolution is unchanged), the
// others are partitioned again among themselves, down to single crates. A crate that ends alone carries the package and the two feature
// sets that split it, for the run notes.
//
// `cargo tree` compiles nothing and reads only manifests and the lock, so it runs as a class A read (direct cargo, scratch target), and the
// partition of a configuration group is cached on the workspace's Cargo.lock, every Cargo.toml and the toolchain file: a manifest edit
// recomputes it, anything else reuses it.
//
// Dependency-free by design (node built-ins only); deterministic-scans.mjs and unit-list-compile.mjs import it.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

export const FEATURE_PARTITION_SCHEMA = "rightkit.feature-partition/1";
const sha256 = (v) => crypto.createHash("sha256").update(v).digest("hex");
/** `{p}` and `{f}` with a tab between: the package (name, version, source) and its comma-separated resolved features. */
export const TREE_FORMAT = "{p}\t{f}";

/** Flags of a cargo argv that decide feature resolution and travel to `cargo tree` unchanged. */
const FEATURE_FLAGS = new Set(["--all-features", "--no-default-features"]);
const FEATURE_VALUE_FLAGS = new Set(["--features", "-F", "--manifest-path"]);
/** Build targets whose dependencies are dev-dependencies: when one is compiled, dev edges join the resolution (resolver 2). */
const DEV_TARGET_FLAGS = new Set(["--all-targets", "--tests", "--benches", "--examples"]);

/**
 * `rest` without its package selectors: every `-p <crate>` / `--package <crate>` pair and `--target <triple>` removed, nothing else touched.
 * Returns { rest, crates, targets }.
 */
export function stripSelectors(argv) {
  const rest = []; const crates = []; const targets = [];
  const stop = argv.indexOf("--"); const own = stop < 0 ? argv : argv.slice(0, stop);
  for (let i = 0; i < own.length; i += 1) {
    const w = own[i];
    if ((w === "-p" || w === "--package") && typeof own[i + 1] === "string") { crates.push(own[i + 1]); i += 1; continue; }
    if (w === "--target" && typeof own[i + 1] === "string") { targets.push(own[i + 1]); i += 1; continue; }
    if (/^--package=/.test(w)) { crates.push(w.slice("--package=".length)); continue; }
    if (/^--target=/.test(w)) { targets.push(w.slice("--target=".length)); continue; }
    rest.push(w);
  }
  return { rest, crates, targets };
}

/**
 * The `cargo tree` argv (after `cargo`) that resolves `packages` the way `rest` (a cargo argv after `cargo`, selectors removed) compiles
 * them: the same feature flags, dev edges when the command builds dev targets (test, bench, --all-targets, --tests, --benches, --examples),
 * `--target` as given, `--locked`, one line per package in TREE_FORMAT.
 */
export function treeArgs({ rest, packages, target = null }) {
  if (!Array.isArray(rest) || !rest.length || typeof rest[0] !== "string") throw new TypeError("treeArgs: rest must start with the cargo subcommand");
  if (!Array.isArray(packages) || !packages.length || packages.some((p) => typeof p !== "string" || !/^[A-Za-z0-9_-]+$/.test(p))) throw new TypeError("treeArgs: packages must be plain crate names");
  const [sub, ...own] = rest;
  const stop = own.indexOf("--"); const flags = stop < 0 ? own : own.slice(0, stop);
  const dev = sub === "test" || sub === "bench" || flags.some((w) => DEV_TARGET_FLAGS.has(w));
  const carried = [];
  for (let i = 0; i < flags.length; i += 1) {
    const w = flags[i];
    if (FEATURE_FLAGS.has(w)) carried.push(w);
    else if (FEATURE_VALUE_FLAGS.has(w) && typeof flags[i + 1] === "string") { carried.push(w, flags[i + 1]); i += 1; }
    else if (/^(?:--features=|--manifest-path=|-F.)/.test(w)) carried.push(w);
  }
  return ["tree", "--locked", "--prefix", "none", "-e", dev ? "normal,build,dev" : "normal,build", "-f", TREE_FORMAT, ...carried, ...(target ? ["--target", target] : []), ...packages.flatMap((p) => ["-p", p])];
}

/**
 * The resolved features of a `cargo tree` run in TREE_FORMAT: Map of package (`name vX.Y.Z [(source)]`) to the Set of distinct feature lists
 * it was resolved with (a proc-macro or build dependency can appear for the host and the target with different features; each list is
 * sorted and comma-joined, "" for none). Lines without a tab (a client banner, a warning) are skipped; cargo's `(*)` de-duplication mark is dropped.
 */
export function parseTreeFeatures(text) {
  const out = new Map();
  for (const raw of String(text ?? "").split(/\r?\n/)) {
    const at = raw.indexOf("\t");
    if (at < 0) continue;
    const pkg = raw.slice(0, at).trim();
    const feats = raw.slice(at + 1).replace(/\s*\(\*\)\s*$/, "").trim();
    if (!pkg) continue;
    const list = feats.split(",").map((f) => f.trim()).filter(Boolean).sort().join(",");
    if (!out.has(pkg)) out.set(pkg, new Set());
    out.get(pkg).add(list);
  }
  return out;
}

const sameSet = (a, b) => a.size === b.size && [...a].every((x) => b.has(x));

/**
 * The first package the group resolves differently from the member alone, or null when the group is the member's own resolution for every
 * package of the member's graph. `own` and `group` are parseTreeFeatures maps.
 */
export function memberDifference(own, group) {
  for (const [pkg, feats] of own) {
    const g = group.get(pkg);
    if (!g || !sameSet(feats, g)) return { package: pkg, own: [...feats].sort(), group: g ? [...g].sort() : [] };
  }
  return null;
}

/**
 * Partitions `crates` into feature-equivalent groups. `tree(packages)` resolves a selection (the text of `cargo tree` in TREE_FORMAT).
 * Returns `[{ crates, split }]` in a deterministic order: `split` is null for a group every member of which resolves alone exactly as in
 * it, and for a crate that ends alone names the package and feature sets that separated it. `concurrency` bounds the parallel tree runs.
 */
export async function partitionByFeatures({ crates, tree, concurrency = 4 }) {
  if (!Array.isArray(crates) || new Set(crates).size !== crates.length) throw new TypeError("partitionByFeatures: crates must be distinct");
  if (typeof tree !== "function") throw new TypeError("partitionByFeatures: tree must be a function");
  const own = new Map();
  let active = 0; const waiting = [];
  const slot = async () => { if (active >= Math.max(1, concurrency)) await new Promise((r) => waiting.push(r)); active += 1; };
  const free = () => { active -= 1; const next = waiting.shift(); if (next) next(); };
  const resolve = async (packages) => { await slot(); try { return parseTreeFeatures(await tree([...packages].sort())); } finally { free(); } };
  const ownOf = (c) => { if (!own.has(c)) own.set(c, resolve([c])); return own.get(c); };
  const out = [];
  const split = async (set, why) => {
    if (set.length === 1) { out.push({ crates: set, split: why ?? null }); return; }
    const [group, ...owns] = await Promise.all([resolve(set), ...set.map(ownOf)]);
    const keep = []; const apart = [];
    set.forEach((c, i) => { const d = memberDifference(owns[i], group); if (d) apart.push({ crate: c, d }); else keep.push(c); });
    if (!apart.length) { out.push({ crates: set, split: null }); return; }
    // The equivalent members stay one invocation: without the others the group resolves no more than it did, and no less than each resolves alone.
    if (keep.length) out.push({ crates: keep, split: null });
    // Members the whole group changed: each stands alone when every member conflicts (a smaller group of the same members would conflict the
    // same way); otherwise they are partitioned again among themselves.
    if (!keep.length || apart.length === 1) { for (const a of apart) out.push({ crates: [a.crate], split: { crate: a.crate, ...a.d } }); return; }
    await split(apart.map((a) => a.crate), null);
  };
  await split([...crates].sort(), null);
  out.sort((a, b) => (a.crates[0] < b.crates[0] ? -1 : 1));
  return out;
}

/**
 * The common refinement of several partitions of the same crates (one per `--target` of a group naming several targets, NN42): two crates share a
 * group only when every partition puts them in the same part, so no boundary any target drew is crossed. A crate alone in any partition stays
 * alone and keeps that partition's split reason; otherwise the split is null. Returns `[{ crates, split }]` sorted by first crate.
 */
export function meetPartitions(perTarget) {
  if (!Array.isArray(perTarget) || !perTarget.length) throw new TypeError("meetPartitions: needs at least one partition");
  if (perTarget.length === 1) return perTarget[0];
  const crates = [...new Set(perTarget.flatMap((parts) => parts.flatMap((p) => p.crates)))].sort();
  const signature = new Map(); const reason = new Map();
  for (const c of crates) {
    const sig = perTarget.map((parts) => { const at = parts.findIndex((p) => p.crates.includes(c)); if (at >= 0 && parts[at].split && !reason.has(c)) reason.set(c, parts[at].split); return at; });
    if (sig.some((at) => at < 0)) throw new Error(`meetPartitions: ${c} is missing from a partition`);
    signature.set(c, sig.join("/"));
  }
  const groups = new Map();
  for (const c of crates) { const k = signature.get(c); if (!groups.has(k)) groups.set(k, []); groups.get(k).push(c); }
  return [...groups.values()].map((set) => ({ crates: set, split: set.length === 1 ? (reason.get(set[0]) ?? null) : null })).sort((a, b) => (a.crates[0] < b.crates[0] ? -1 : 1));
}

/** One line for the run notes: why a crate runs alone. */
export function splitNote(split) {
  return `${split.crate} compiles alone: ${split.package} resolves to {${split.own.join(" | ") || "no features"}} for it and to {${split.group.join(" | ") || "no features"}} in its group`;
}

/**
 * sha256 over what `cargo tree` reads under `dir`: Cargo.lock, every Cargo.toml and rust-toolchain(.toml) (target, node_modules and dot
 * directories skipped), by relative path. The partition of a configuration group is cached on it.
 */
export function workspaceManifestDigest(dir, { fsApi = fs } = {}) {
  const h = crypto.createHash("sha256");
  const walk = (d) => {
    let entries; try { entries = fsApi.readdirSync(d, { withFileTypes: true }); } catch { return; }
    for (const e of entries.sort((a, b) => (a.name < b.name ? -1 : 1))) {
      // `.cargo/config.toml` (patches, target cfg, rustflags) is a resolution input; every other dot directory is not.
      if ((e.name.startsWith(".") && e.name !== ".cargo") || e.name === "node_modules" || e.name === "target") continue;
      const p = path.join(d, e.name);
      const inCargoDir = path.basename(d) === ".cargo";
      if (e.isDirectory()) walk(p);
      else if (e.isFile() && (e.name === "Cargo.toml" || e.name === "Cargo.lock" || e.name === "rust-toolchain" || e.name === "rust-toolchain.toml" || (inCargoDir && (e.name === "config.toml" || e.name === "config")))) { h.update(path.relative(dir, p).split(path.sep).join("/")); h.update("\0"); h.update(fsApi.readFileSync(p)); h.update("\0"); }
    }
  };
  walk(dir);
  return h.digest("hex");
}

/** The code of this module, part of every partition key: a change to how the partition is computed invalidates every stored partition. */
let moduleDigestMemo = null;
function moduleDigest() {
  if (moduleDigestMemo === null) { try { moduleDigestMemo = sha256(fs.readFileSync(new URL(import.meta.url))); } catch { moduleDigestMemo = "unreadable"; } }
  return moduleDigestMemo;
}

/** The cache key of one configuration group's partition: the command shape, the crates, the target, the workspace, its manifest digest, the cargo that resolved and this module's code. */
export function partitionCacheKey({ rest, crates, target = null, cwd = ".", inputs, toolVersion = null }) {
  return sha256(`${FEATURE_PARTITION_SCHEMA}\0${JSON.stringify({ rest, crates: [...crates].sort(), target, cwd, inputs, toolVersion, code: moduleDigest() })}`);
}
