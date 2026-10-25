#!/usr/bin/env node
// rightkit-gate: the mandatory local deterministic gate for any repository that uses RightKit (Adrian 2026-10-22:
// "local deterministic checks ... mandatory for all builds"). Runs from the pre-push hook that `rightkit-gate install-hook`
// writes, so it never depends on an agent remembering. Every applicable scan for the pushed change runs through the one
// scan module (../deterministic-scans.mjs); Rust units come from `cargo metadata` (one run per Cargo workspace), npm units from
// the pnpm roots' workspace files, so the layout is the repository's own. CI still compiles, lints and tests everything.
//
// Layout it covers (nothing is partly covered silently):
//   - Rust: every tracked Cargo.toml that is a workspace root or a standalone package not inside another workspace is its own Cargo
//     workspace (`cargo metadata --no-deps --locked` there, through the broker). A unit of a workspace below the repository root is
//     named `<cargoDir>:<crate>`; its scans run with that directory as cwd, its reverse dependents and cache keys come from that
//     workspace's own Cargo.lock, and ci/gate.config.json `rust` may key its feature sets by `<cargoDir>:<crate>` or by the plain crate
//     name. A tracked Cargo.toml that is not a member and cannot stand alone, a member that is untracked or ignored, and an untracked
//     Cargo.toml are skipped with a note on the first lines of the run, never silently.
//   - npm: every pnpm root (discoverPnpmRoots: the repository root, and any tracked directory with a pnpm-lock.yaml or pnpm-workspace.yaml
//     beside a package.json, outside node_modules) has its own units: the pnpm-workspace.yaml globs over tracked package.json files; when that
//     file is absent, has no `packages:` or declares ".", the root's package.json is a unit too. pnpm-lockfile and pnpm-audit run once per
//     root in its directory (`pnpm-lockfile:<root>` below the repository root), and each unit gets tsc, the linter it configures (eslint
//     or biome, by config file) and its tests like any package. A unit of a root below the repository root is named from its directory.
//
// Release evidence for a PRODUCT repository (an app): the pin's "a release qualified locally on both hosts" is the app's own
// `right-release` release QA run on both hosts (docs/CI.md "Release evidence for an app"). `right-release qualify cargo` qualifies
// CRATES for a registry upload (`.rightkit-gate.json` configures it for a repository other than RightKit); it is not an app's evidence.
//
// Content checks run on an export of HEAD (exactly what is pushed), never on uncommitted work in the checkout.
//
//   rightkit-gate [--base <ref>] [--plan] [--no-cache]
//                                             run the gate (default base origin/main); --plan prints the scans and runs none;
//                                             --no-cache (or RK_GATE_CACHE=0) reruns every scan
//   rightkit-gate check [--repo <dir>] [--commit <rev> | --worktree] [--only <rule,...>] [--list] [--print-baseline] [--baseline <file>]
//                                             the deterministic stage: the app check (../app-check.mjs), then every class A scan
//                                             (lockfiles, cargo-deny/audit, secrets, licences, manifests, workflow lint, syntax) over the
//                                             whole tree of the commit, not a diff. Nothing compiles; without a broker class A uses
//                                             plain cargo for metadata/deny/audit only. The RustRight broker runs this before it admits
//                                             any compile. --list/--print-baseline/--only show the app check alone. Exit 0 pass,
//                                             1 findings, 2 could not run
//   rightkit-gate install-hook [--force]      write .githooks/pre-push and set core.hooksPath. core.hooksPath hides .git/hooks, so the
//                                             pre-push hook runs the repository's own .git/hooks/pre-push first, then `rightkit-gate
//                                             check --commit HEAD`, then the gate; every other hook present in .git/hooks gets a
//                                             .githooks/<name> trampoline that runs it; the hook finds the gate binary in the root
//                                             node_modules/.bin, then any workspace package's, then pnpm exec. Where @rightkit/release is
//                                             installed it also writes the advisory ownership pre-commit step (`right-release hooks install`
//                                             is an alias of this command)
//
// From the pre-push hook, git's ref lines arrive on stdin: the commit being pushed must be the checked-out HEAD (broker scans
// read the live checkout), and the remote's current commit is the diff base. To push an earlier commit of the local stack,
// check it out first (a detached HEAD is fine), push HEAD:<branch>, return.
// A host with neither `rustright` nor the managed cargo shim (a cloud session) refuses the Rust gate, with one exception: when git's
// stdin names only non-protected branches (not main, master, trunk, release/*, the .rightgit.json defaultBranch, or a tag) and the
// process is not in CI (no CI or GITHUB_ACTIONS), the gate defers the Rust gate to host qualification. It reads the Rust units from the
// tracked manifests (no cargo), runs every scan that needs no broker, prints "rust gate DEFERRED to host qualification" with the Rust
// units and scans it did not run, exits 0 and writes nothing into the tree. An npm test scan whose package script chain runs `rustright` or
// `cargo` (planScans marks it `brokerCall`) cannot run there either and is deferred the same way, named with its reason. The pull request then
// needs a HOST RECEIPT before merge; that receipt stays the merge gate. RIGHTKIT_GATE_NO_DEFER=1 keeps the refusal on such a host. Every pushed ref is judged, whatever its place
// in the push; git's stdin is split on single ASCII spaces (a ref name may hold Unicode whitespace) and a line that is not four fields defers
// nothing. The default branch is named by the checkout, the base ref, the remote commits being replaced and origin/HEAD, so a commit cannot
// unprotect it by editing .rightgit.json. A refusal prints the old message and, on a second line, why nothing was deferred.
// ci/gate.config.json `rust` (optional) sets per-crate feature sets, exactly as CI's gate reads it.
// Result cache: a passing class B scan is reused only when its scan id, command, tool version, environment and input digest
// (the gated commit's git object ids for the crate's workspace closure, the shared build inputs, ci/gate.config.json and every
// path the closure's sources include from outside their crate; the whole HEAD tree for Rust tests unless ci/gate.config.json
// rust.<crate>.testInputs lists the paths they read, for npm, node and docs scans, and whenever a reference cannot be resolved
// statically) are all identical. Stored in <git common dir>/rightkit-gate-cache; a hit prints
// `cached` and is listed in the result's cacheHits. Failures, deferrals and class A scans are never cached.
// RK_GATE_JOBS (default 4) sets how many compile scans run side by side; RK_GATE_TEST_JOBS (default RK_GATE_JOBS) how many test
// scans, after the compile lanes drain. A crate whose ci/gate.config.json entry says `quietTests: true` has its test scans run last,
// one at a time, under the RustRight quiet lease (`rustright quiet acquire`, RUSTRIGHT_QUIET_LEASE in their environment).
// SIGINT/SIGTERM stop scheduling, kill every running scan's process tree and exit non-zero listing the scans not run.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { BIOME_CONFIG_FILES, ESLINT_CONFIG_FILES, WORKSPACE_RUST_FILE, ancestorConfigChanged, ancestorDirsOf, canonicalJson, defaultSpawn, digestScanSet, fileScanCache, findRustfmt, metadataArgs, planScans, runScans, scanDeferrals, scanFailures } from "../deterministic-scans.mjs";
import { changedLockNames, parseLock, reach } from "../lock-graph.mjs";
import { resolveExecutor } from "../gate-executor.mjs";
import { bindingOf, endRun, readReceipt, RECEIPT_SCHEMA, sameBinding, sameSource, sealedFor, stagesTo, startRun, STAGES, writeReceipt } from "../gate-stages.mjs";
import { spawnSync as spawnInherit } from "../background-process.mjs";

const ZERO = /^0+$/;
const SAFE_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const posix = (p) => String(p).replaceAll("\\", "/");
const text = (r) => String(r?.stdout ?? "");
const failed = (r) => !r || Boolean(r.error) || r.status !== 0;
const why = (r) => String(r?.error?.message ?? r?.stderr ?? "").trim().split(/\r?\n/).slice(-3).join(" | ");

const USAGE = `usage: rightkit-gate --to check|compile|package|qualify|release [--platform mac|win] [--tier patch|update] [--receipt-in <file>] [--receipt-out <file>] | rightkit-gate [--base <ref>] [--plan] [--no-cache] [--pre-push] | rightkit-gate install-hook [--force] | rightkit-gate check [--repo <dir>] [--commit <rev> | --worktree] [--only <rule,...>] [--list] [--print-baseline] [--baseline <file>] | --version`;

// ---- pure helpers (exported for tests) ----------------------------------------------------------------------

/** Repository pre-push policy only. Full gates & publication qualification retain native checks. */
export function prePushPlan(plan) {
  const scans = plan.scans.filter((s) => s.class === "A" || (s.class === "B" && ["test:scripts-ci", "generated-docs"].includes(s.id)));
  const ids = new Set(scans.map((s) => s.id));
  return {
    ...plan, scans, deferred: plan.deferred.filter((id) => ids.has(id)),
    scanSetDigest: digestScanSet(scans),
    classDigests: { A: digestScanSet(scans.filter((s) => s.class === "A")), B: digestScanSet(scans.filter((s) => s.class === "B")) },
    notes: [...plan.notes, "pre-push mode: class A, focused scripts/ci tests & generated docs only; native checks remain required for publication qualification"],
  };
}

/** The `packages:` globs of a pnpm-workspace.yaml (block or flow list; the subset pnpm repositories use). */
export function parseWorkspaceGlobs(source) {
  const out = []; let inPackages = false;
  for (const raw of String(source).split(/\r?\n/)) {
    const line = raw.replace(/\s+#.*$/, "");
    const flow = /^packages\s*:\s*\[(.*)\]\s*$/.exec(line);
    if (flow) { for (const item of flow[1].split(",")) { const v = item.trim().replace(/^["']|["']$/g, ""); if (v) out.push(v); } inPackages = false; continue; }
    if (/^packages\s*:/.test(line)) { inPackages = true; continue; }
    if (/^[^\s#-]/.test(line)) { inPackages = false; continue; }
    if (!inPackages) continue;
    const m = /^\s*-\s*(?:"([^"]*)"|'([^']*)'|(\S+))\s*$/.exec(line);
    if (m) out.push(m[1] ?? m[2] ?? m[3]);
  }
  return out;
}

/** A pnpm workspace glob as a RegExp over a repo-relative posix directory: `*` one segment, `**` any depth. */
export function globToRegExp(glob) {
  const g = posix(glob).replace(/^\.\//, "").replace(/\/+$/, "");
  let re = "";
  for (let i = 0; i < g.length;) {
    if (g.startsWith("**/", i)) { re += "(?:.+/)?"; i += 3; }
    else if (g.startsWith("**", i)) { re += ".*"; i += 2; }
    else if (g[i] === "*") { re += "[^/]*"; i += 1; }
    else { re += g[i].replace(/[.+?^${}()|[\]\\]/g, "\\$&"); i += 1; }
  }
  return new RegExp(`^${re}$`);
}

/** npm unit name from a package name: `@scope/x` -> `scope-x`; anything still unsafe falls back to the directory. */
const unitName = (pkgName, dir) => {
  const n = String(pkgName ?? "").replace(/^@/, "").replace(/\//g, "-");
  return SAFE_NAME.test(n) ? n : dir === "." ? "root" : dir.replace(/\//g, "-");
};

// The bounds of the pnpm root walk. A lock deeper than this is a buried fixture, not an app; more roots than this is refused, because covering
// some of them would hide the rest.
const PNPM_ROOT_MAX_DEPTH = 6;
const PNPM_ROOT_MAX_COUNT = 64;

/** Tracked files with exactly this name (a lister that ignores the name is filtered here). */
const trackedNamed = (tracked, name) => tracked(name).map(posix).filter((p) => path.posix.basename(p) === name);

/**
 * The pnpm roots of the repository, sorted: "." when the repository root holds a pnpm-workspace.yaml or a package.json, and every other
 * tracked directory (outside node_modules, at most 6 levels deep) that holds a pnpm-lock.yaml or a pnpm-workspace.yaml beside a
 * package.json. A repository such as CodeRight (apps/coderight-tauri and apps/coderight-web, each with its own lock, nothing at the root) has
 * two; a plain repository has "." only. A walk over the tracked files, never the file system: nothing in node_modules or untracked is a root.
 */
export function discoverPnpmRoots({ root, tracked, exists }) {
  const manifestDirs = new Set(trackedNamed(tracked, "package.json").map((p) => path.posix.dirname(p)));
  const roots = new Set();
  if (exists(path.join(root, "pnpm-workspace.yaml")) || exists(path.join(root, "package.json"))) roots.add(".");
  for (const file of [...trackedNamed(tracked, "pnpm-lock.yaml"), ...trackedNamed(tracked, "pnpm-workspace.yaml")]) {
    const dir = path.posix.dirname(file);
    const parts = dir.split("/");
    if (dir === "." || parts.includes("node_modules") || parts.length > PNPM_ROOT_MAX_DEPTH || !manifestDirs.has(dir)) continue;
    roots.add(dir);
  }
  if (roots.size > PNPM_ROOT_MAX_COUNT) throw new Error(`${roots.size} pnpm roots (the gate covers at most ${PNPM_ROOT_MAX_COUNT}, and covering some would hide the rest)`);
  return [...roots].sort();
}

/**
 * npm units, per pnpm root (discoverPnpmRoots). Within a root: pnpm-workspace.yaml globs over the tracked package.json files, else the root's
 * own package.json. The root package.json is a unit as well when the workspace file is absent, has no `packages:` (or an empty one) or declares
 * "." (or "./"); with a `packages:` list that does not name the root it is only the workspace's own manifest and is not a unit. A directory
 * belongs to the deepest root that holds it. The repository root's units are named from their package names (`@scope/x` -> `scope-x`, a
 * collision falls back to the directory) and carry no `root`, exactly as in a repository with one root. The units of any other root are named
 * from their directory (`apps/coderight-web` -> `apps-coderight-web`), so their scan ids carry the root path, and carry `root`.
 */
export function discoverNpmUnits({ root, tracked, readFile, exists }) {
  const roots = discoverPnpmRoots({ root, tracked, exists });
  const manifestDirs = [...new Set(trackedNamed(tracked, "package.json").map((p) => path.posix.dirname(p)))];
  const within = (rootDir, dir) => (rootDir === "." ? dir : dir === rootDir ? "." : dir.startsWith(`${rootDir}/`) ? dir.slice(rootDir.length + 1) : null);
  const joined = (rootDir, rel) => (rootDir === "." ? rel : rel === "." ? rootDir : `${rootDir}/${rel}`);
  const ownerOf = (dir) => roots.reduce((best, r) => (within(r, dir) !== null && (best === null || r.length > best.length) ? r : best), null);
  const found = new Map();
  for (const rootDir of roots) {
    let dirs;
    const workspaceFile = path.join(root, joined(rootDir, "pnpm-workspace.yaml"));
    if (exists(workspaceFile)) {
      const globs = parseWorkspaceGlobs(readFile(workspaceFile));
      const inc = globs.filter((g) => !g.startsWith("!")).map(globToRegExp);
      const exc = globs.filter((g) => g.startsWith("!")).map((g) => globToRegExp(g.slice(1)));
      const positive = globs.filter((g) => !g.startsWith("!"));
      const rootDeclared = positive.length === 0 || positive.some((g) => g === "." || g === "./");
      dirs = manifestDirs.map((d) => within(rootDir, d)).filter((rel) => rel !== null && (rel === "." ? rootDeclared : inc.some((r) => r.test(rel))) && !exc.some((r) => r.test(rel))).map((rel) => joined(rootDir, rel));
    } else dirs = exists(path.join(root, joined(rootDir, "package.json"))) ? [rootDir] : [];
    for (const dir of dirs) if (ownerOf(dir) === rootDir) found.set(dir, rootDir);
  }
  const rows = [...found.keys()].sort().map((dir) => {
    let manifest;
    try { manifest = JSON.parse(readFile(path.join(root, dir, "package.json")).replace(/^﻿/, "")); } catch (e) { throw new Error(`${dir}/package.json: ${e.message}`); }
    const rootDir = found.get(dir);
    return { dir, root: rootDir, name: rootDir === "." ? unitName(manifest.name, dir) : dir.replace(/\//g, "-") };
  });
  const count = new Map();
  for (const r of rows) count.set(r.name, (count.get(r.name) ?? 0) + 1);
  const named = rows.map((r) => ({ ...r, name: count.get(r.name) > 1 && r.root === "." ? unitName("", r.dir === "." ? "root" : r.dir) : r.name }));
  const taken = new Set();
  for (const r of named) {
    if (taken.has(r.name)) throw new Error(`two npm units are named ${r.name} (${r.dir}); rename one of the packages`);
    taken.add(r.name);
  }
  return named.map((r) => ({ name: r.name, dir: r.dir, ...(r.root === "." ? {} : { root: r.root }) }));
}

const dirOfManifest = (m) => path.posix.dirname(m);
const byDepthThenName = (a, b) => dirOfManifest(a).split("/").length - dirOfManifest(b).split("/").length || (a < b ? -1 : 1);

// `cargo metadata --no-deps --format-version 1 --locked` in one directory, through the resolved cargo command. Returns
// { cargoDir, members: [{ name, dir, manifest, publish }] } with every path repository-relative.
function workspaceMetadata({ root, spawn, cargoCommand, dir, realpath }) {
  const r = spawn(cargoCommand[0], [...cargoCommand.slice(1), ...metadataArgs()], { cwd: path.join(root, dir) });
  if (failed(r)) throw new Error(`cargo metadata failed: ${why(r) || `exit ${r?.status}`}${dir === "." ? "" : ` (in ${dir})`}`);
  let meta;
  try { meta = JSON.parse(text(r)); } catch (e) { throw new Error(`cargo metadata output is not JSON: ${e.message}${dir === "." ? "" : ` (in ${dir})`}`); }
  const real = (p) => { try { return realpath(p); } catch { return path.resolve(p); } };
  const rootReal = real(root);
  const rel = (abs) => posix(path.relative(rootReal, real(abs))) || ".";
  const cargoDir = rel(meta.workspace_root);
  if (cargoDir === ".." || cargoDir.startsWith("../")) throw new Error(`the Cargo workspace root ${meta.workspace_root} is outside the repository`);
  const members = (meta.packages ?? []).map((p) => {
    const pkgDir = rel(path.dirname(p.manifest_path));
    if (pkgDir === ".." || pkgDir.startsWith("../")) throw new Error(`workspace member ${p.name} lives outside the repository (${pkgDir})`);
    return { name: p.name, dir: pkgDir, manifest: pkgDir === "." ? "Cargo.toml" : `${pkgDir}/Cargo.toml`, publish: !(Array.isArray(p.publish) && p.publish.length === 0) };
  }).sort((a, b) => (a.name < b.name ? -1 : 1));
  return { cargoDir, members };
}

/**
 * Rust units of ONE Cargo workspace from `cargo metadata --no-deps --format-version 1 --locked`: several workspaces are refused
 * here. The gate itself uses discoverRustWorkspaces, which handles every workspace of the repository. Returns
 * { present, units: [{ name, dir, publish }], cargoDir, uncovered }.
 */
export function discoverRustUnits({ root, spawn, cargoCommand, tracked, readFile, realpath = fs.realpathSync }) {
  const manifests = tracked("Cargo.toml");
  if (!manifests.length) return { present: false, units: [], cargoDir: ".", uncovered: [] };
  const roots = [];
  for (const m of [...manifests].sort(byDepthThenName)) {
    const d = dirOfManifest(m);
    if (!roots.some((r) => r === "." || d === r || d.startsWith(`${r}/`))) roots.push(d);
  }
  if (roots.length > 1) throw new Error(`more than one Cargo workspace (${roots.join(", ")}); discoverRustUnits covers one, so none is partly covered (rightkit-gate uses discoverRustWorkspaces)`);
  const { cargoDir, members } = workspaceMetadata({ root, spawn, cargoCommand, dir: roots[0], realpath });
  const units = members.map(({ name, dir, publish }) => ({ name, dir, publish }));
  const covered = new Set(members.map((m) => m.manifest));
  const uncovered = manifests.filter((m) => !covered.has(m) && /^\s*\[package\]/m.test(readFile(path.join(root, m))));
  return { present: true, units, cargoDir, uncovered };
}

/** The unit name in scan ids and ci/gate.config.json: the plain crate in a root workspace, `<cargoDir>:<crate>` below it. */
export const rustUnitName = (cargoDir, crate) => (cargoDir === "." ? crate : `${cargoDir}:${crate}`);

/**
 * The top-level TOML tables of a Cargo manifest: name -> body text (the lines under `[name]` up to the next header, trimmed).
 * `preamble` holds the lines before the first header. Enough to compare manifests table by table; no value parsing.
 */
export function manifestTables(text) {
  const tables = new Map(); let current = "preamble"; let body = [];
  const flush = () => { tables.set(current, (tables.get(current) ?? "") + body.join("\n").trim()); body = []; };
  for (const line of String(text ?? "").replace(/\r\n/g, "\n").split("\n")) {
    const header = /^\s*\[\[?([^\]]+)\]\]?\s*(#.*)?$/.exec(line);
    if (header) { flush(); current = header[1].trim(); continue; }
    body.push(line);
  }
  flush();
  return tables;
}

/**
 * The value text of `key` in the top-level table `table` of a manifest read by manifestTables (everything from the `=` on), whether it is
 * written under `[table]` or as the dotted root key `table.key = ...`; null when absent. No value parsing: tomlString and tomlStrings read it.
 */
export function tomlValue(tables, table, key) {
  const esc = (x) => x.replace(/[.*+?^${}()|[\]\\-]/g, "\\$&");
  const own = tables.get(table) ?? "";
  const inTable = new RegExp(`^[ \\t]*${esc(key)}[ \\t]*=[ \\t]*`, "m").exec(own);
  if (inTable) return own.slice(inTable.index + inTable[0].length);
  const root = tables.get("preamble") ?? "";
  const dotted = new RegExp(`^[ \\t]*${esc(table)}\\.${esc(key)}[ \\t]*=[ \\t]*`, "m").exec(root);
  return dotted ? root.slice(dotted.index + dotted[0].length) : null;
}

/** True when the manifest has the top-level table `name`: as `[name]`, as a `[name.sub]` table, or as dotted root keys `name.key = ...`. */
export const hasTomlTable = (tables, name) => tables.has(name) || [...tables.keys()].some((k) => k.startsWith(`${name}.`)) || new RegExp(`^[ \\t]*${name.replace(/\W/g, "\\$&")}\\.`, "m").test(tables.get("preamble") ?? "");

/** The string a TOML value starts with ("basic" or 'literal'); null when the value is not a string. */
export function tomlString(value) {
  const m = /^(?:"((?:[^"\\\n]|\\.)*)"|'([^'\n]*)')/.exec(String(value ?? ""));
  return m ? (m[1] !== undefined ? m[1].replace(/\\(.)/g, "$1") : m[2]) : null;
}

/**
 * The strings of the TOML array a value starts with: "basic" and 'literal' strings, across lines, with `#` comments (and any `]` in
 * them or in a string) skipped. [] when the value is not an array.
 */
export function tomlStrings(value) {
  const s = String(value ?? "");
  if (s[0] !== "[") return [];
  const out = [];
  for (let i = 1; i < s.length;) {
    const c = s[i];
    if (c === "]") break;
    if (c === "#") { while (i < s.length && s[i] !== "\n") i += 1; continue; }
    if (c === '"' || c === "'") {
      let j = i + 1; let v = "";
      while (j < s.length && s[j] !== c && s[j] !== "\n") { if (c === '"' && s[j] === "\\" && j + 1 < s.length) { v += s[j + 1]; j += 2; } else { v += s[j]; j += 1; } }
      out.push(v); i = j + 1; continue;
    }
    i += 1;
  }
  return out;
}

/** The repository-relative directories a workspace manifest `exclude`s (`[workspace] exclude = [...]`); [] when none or unreadable. */
export function workspaceExcludes(readFile, root, cargoDir) {
  let text;
  try { text = readFile(path.join(root, cargoDir === "." ? "Cargo.toml" : `${cargoDir}/Cargo.toml`)); } catch { return []; }
  return tomlStrings(tomlValue(manifestTables(text), "workspace", "exclude")).map((d) => posix(d).replace(/\/+$/, "")).map((d) => (cargoDir === "." ? d : `${cargoDir}/${d}`));
}

// The manifest tables whose every effect on a build is also written into Cargo.lock: a changed source ([patch.*]) and workspace
// membership ([workspace] members/exclude: a new member is a new lock package, a removed one disappears). A [workspace.dependencies]
// change is lock-reflected only when it is a version change (the lock records the resolved version): a feature list reaches every
// `workspace = true` dependent without touching the lock. Anything else in the root manifest ([profile.*], [workspace.package],
// [workspace.lints], [workspace.metadata] read by build scripts) can change what every crate compiles to, so it keeps every crate in scope.
const LOCK_REFLECTED_TABLE = /^patch\..+$/;
const WORKSPACE_MEMBERSHIP_KEYS = /^\s*(members|exclude|default-members)\s*=\s*\[[\s\S]*?\]\s*(#.*)?$/gm;
// `serde = "1.0.2"` and `serde = { version = "1.0.2", ... }` with the version neutralised, so two dependency tables compare equal when
// only versions differ.
const versionNeutral = (t) => String(t ?? "").replace(/(\bversion\s*=\s*)"[^"]*"/g, '$1"*"').replace(/^(\s*[\w.-]+\s*=\s*)"[^"]*"/gm, '$1"*"');

/**
 * True when every difference between two root manifests lives in a lock-reflected table (above), so the lock graph already says
 * which crates the change reaches. False for any other difference, a malformed manifest, or no difference at all.
 */
export function manifestChangeIsLockReflected(before, after) {
  if (typeof before !== "string" || typeof after !== "string" || before === after) return false;
  const a = manifestTables(before); const b = manifestTables(after);
  // A comment-only line and trailing blank lines change no build; a trailing `# ...` on a value line is kept (a `#` may sit in a string).
  const text = (t) => String(t ?? "").split("\n").filter((l) => !/^\s*(#.*)?$/.test(l)).join("\n");
  const strip = (t) => text(t).replace(WORKSPACE_MEMBERSHIP_KEYS, "").trim();
  for (const name of new Set([...a.keys(), ...b.keys()])) {
    if (text(a.get(name)) === text(b.get(name))) continue;
    if (LOCK_REFLECTED_TABLE.test(name)) continue;
    if (name === "workspace" && strip(a.get(name)) === strip(b.get(name))) continue;
    if (name === "workspace.dependencies" && versionNeutral(text(a.get(name))) === versionNeutral(text(b.get(name)))) continue;
    return false;
  }
  return true;
}

/**
 * Every Cargo workspace of the repository. A tracked Cargo.toml is a workspace root, or a standalone package not inside another
 * workspace, unless an earlier (shallower) workspace already lists it as a member; each root is read with its own
 * `cargo metadata --no-deps --locked` (cwd = its directory, through the broker). A manifest below another workspace that is not
 * a member is tried the same way (an `exclude`d or self-contained crate is a real workspace); when cargo refuses it there it is a
 * fixture or vendored crate, reported in `skipped` and not scanned. A manifest outside every workspace that cargo refuses throws.
 * A member whose Cargo.toml is not tracked (untracked or ignored) is dropped into `skipped`, as are `untracked()` manifests.
 * Returns { present, workspaces: [{ cargoDir, units }], units, cargoDir, skipped: [{ manifest, why }] }; `units` is every workspace's
 * [{ name, crate, dir, publish, cargoDir }] and `cargoDir` the first workspace's (the repository root's when it has one).
 * `metadata(dir)` (optional) replaces the cargo call with a reader of the same { cargoDir, members } shape (extra fields are ignored): the
 * deferred pre-push gate passes manifestWorkspaceMetadata where no broker can run cargo.
 */
export function discoverRustWorkspaces({ root, spawn, cargoCommand, tracked, untracked = () => [], realpath = fs.realpathSync, fixtures = [], readFile = (f) => fs.readFileSync(f, "utf8"), metadata = null }) {
  const manifests = tracked("Cargo.toml");
  const trackedSet = new Set(manifests);
  const locks = new Set(tracked("Cargo.lock"));
  const skipped = [];
  for (const m of untracked()) if (!trackedSet.has(m)) skipped.push({ manifest: m, why: "untracked (not committed, so not part of the gated commit)" });
  if (!manifests.length) return { present: false, workspaces: [], units: [], cargoDir: ".", skipped };
  const fixtureDirs = fixtures.map((d) => posix(String(d)).replace(/\/+$/, "")).filter(Boolean);
  const underFixture = (dir) => fixtureDirs.find((f) => dir === f || dir.startsWith(`${f}/`));
  const workspaces = []; const covered = new Set();
  for (const m of [...manifests].sort(byDepthThenName)) {
    if (covered.has(m)) continue;
    const dir = dirOfManifest(m);
    const parent = workspaces.find((w) => w.cargoDir === "." || dir === w.cargoDir || dir.startsWith(`${w.cargoDir}/`));
    // A manifest under a declared fixture directory (ci/gate.config.json `fixtures`) is a consumer or sample crate: it is gated by the
    // consumer rows of the crate it exercises, never as a unit of its own. Nothing under it is asked of cargo.
    const fixture = underFixture(dir);
    if (fixture) { skipped.push({ manifest: m, why: `under the fixture directory ${fixture} (ci/gate.config.json fixtures); gated through the consumer rows of the crate it exercises` }); continue; }
    // A manifest the parent workspace's own manifest `exclude`s (a vendored or patched crate) is compiled only as a dependency of the
    // parent's members: the parent's lock closure gates it, so it is no unit of its own.
    if (parent) {
      const excluded = workspaceExcludes(readFile, root, parent.cargoDir).find((x) => dir === x || dir.startsWith(`${x}/`));
      if (excluded) { skipped.push({ manifest: m, why: `excluded from the Cargo workspace ${parent.cargoDir} by its manifest (${excluded}); it compiles only inside that workspace's lock closure, which gates it` }); continue; }
      // A nested manifest with no committed Cargo.lock has no reproducible build to gate: its dependency resolution is whatever the
      // registry answers today. It is a fixture or sample; the crate it exercises carries the consumer row that compiles it.
      if (!locks.has(`${dir}/Cargo.lock`)) { skipped.push({ manifest: m, why: `below the Cargo workspace ${parent.cargoDir} with no committed Cargo.lock, so its build is not reproducible; gated through the consumer rows of the crate it exercises` }); continue; }
    }
    let found;
    try { found = metadata ? metadata(dir) : workspaceMetadata({ root, spawn, cargoCommand, dir, realpath }); } catch (e) {
      if (!parent) throw e;
      skipped.push({ manifest: m, why: `below the Cargo workspace ${parent.cargoDir} but not one of its members, and cargo refuses it on its own (${String(e.message).replace(/^cargo metadata failed: /, "").replace(/ \(in [^)]*\)$/, "")})` });
      continue;
    }
    if (workspaces.some((w) => w.cargoDir === found.cargoDir)) { covered.add(m); continue; }
    const units = [];
    // The workspace's own root manifest counts as covered even when it is virtual.
    covered.add(found.cargoDir === "." ? "Cargo.toml" : `${found.cargoDir}/Cargo.toml`);
    for (const member of found.members) {
      covered.add(member.manifest);
      if (!trackedSet.has(member.manifest)) { skipped.push({ manifest: member.manifest, why: "a workspace member that is untracked or ignored, so it is not part of the gated commit" }); continue; }
      units.push({ name: rustUnitName(found.cargoDir, member.name), crate: member.name, dir: member.dir, publish: member.publish, cargoDir: found.cargoDir });
    }
    workspaces.push({ cargoDir: found.cargoDir, units });
  }
  workspaces.sort((a, b) => (a.cargoDir === "." ? -1 : b.cargoDir === "." ? 1 : a.cargoDir < b.cargoDir ? -1 : 1));
  const units = workspaces.flatMap((w) => w.units);
  const names = new Map();
  for (const u of units) { if (names.has(u.name)) throw new Error(`two Cargo workspaces both define the unit ${u.name}`); names.set(u.name, true); }
  return { present: true, workspaces, units, cargoDir: workspaces[0]?.cargoDir ?? ".", skipped };
}

// ---- pre-push input and lock scope ------------------------------------------------------------------------

/**
 * git's pre-push stdin: one "<local ref> <local sha> <remote ref> <remote sha>" per line, the four fields joined by single ASCII spaces.
 * Git forbids an ASCII space in a ref name but allows Unicode whitespace (U+00A0, U+2003, U+3000, ...), so the split is on " " alone: a
 * whitespace split would cut such a ref into extra fields and the line would vanish. Blank lines are skipped; any other line that is not
 * four non-empty fields is returned in `malformed` ({ number, line }), never dropped. Returns { lines: [{ localRef, localSha, remoteRef,
 * remoteSha }], malformed }.
 */
export function parsePushLines(stdin) {
  const lines = []; const malformed = [];
  String(stdin ?? "").split("\n").forEach((raw, index) => {
    const line = raw.replace(/\r$/, "");
    if (line === "") return;
    const f = line.split(" ");
    if (f.length !== 4 || f.some((x) => x === "")) { malformed.push({ number: index + 1, line }); return; }
    lines.push({ localRef: f[0], localSha: f[1], remoteRef: f[2], remoteSha: f[3] });
  });
  return { lines, malformed };
}

/**
 * The crates a lock-reflected workspace change reaches (the lock graph CI's selection uses): a Cargo.lock change, a vendor/ change
 * (the vendored crate is a lock package), and a workspace manifest change confined to the tables the lock also records
 * (manifestChangeIsLockReflected: [patch.*], [workspace.dependencies], membership). null when any other workspace Rust file or
 * manifest table changed or the base has no lock: every crate stays in scope. `readManifest` reads the gated commit's manifest.
 */
export function lockScope({ changed, cargoDir = ".", unitNames, showBase, readLock, readManifest = null }) {
  // Rust configuration of any ancestor directory (repository .cargo, intermediate .cargo, toolchain, clippy, rustfmt) reaches
  // every crate of the workspace, whatever the lock graph says.
  if (ancestorConfigChanged(cargoDir, changed)) return null;
  const rel = (f) => (cargoDir === "." ? f : f.startsWith(`${cargoDir}/`) ? f.slice(cargoDir.length + 1) : null);
  const wide = changed.map((f) => [f, rel(f)]).filter(([, r]) => r !== null && WORKSPACE_RUST_FILE.test(r));
  if (!wide.length || wide.some(([, r]) => r !== "Cargo.lock" && r !== "Cargo.toml" && !r.startsWith("vendor/"))) return null;
  const inWs = (f) => (cargoDir === "." ? f : `${cargoDir}/${f}`);
  if (wide.some(([, r]) => r === "Cargo.toml")) {
    if (typeof readManifest !== "function") return null;
    let after;
    try { after = readManifest(); } catch { return null; }
    if (!manifestChangeIsLockReflected(showBase(inWs("Cargo.toml")), after)) return null;
  }
  const before = showBase(inWs("Cargo.lock"));
  if (before === null) return null;
  let nextText;
  try { nextText = readLock(); } catch { return null; }
  const next = parseLock(nextText);
  const touched = changedLockNames(parseLock(before), next);
  // A member the lock no longer lists was removed: nothing left to scan for it. A package present before and absent now reaches
  // nothing; a package new now is in `touched` already.
  for (const [, r] of wide) if (r.startsWith("vendor/")) touched.add(r.split("/")[1]);
  const memo = new Map(); const scope = new Set();
  for (const [name] of next) {
    if (!unitNames.has(name)) continue;
    if (touched.has(name) || [...touched].some((t) => reach(next, name, memo).has(t))) scope.add(name);
  }
  return scope;
}

/**
 * The pre-push hook. git's ref lines arrive on stdin and the gate consumes them, so stdin is buffered once and fed to both the
 * repository's own .git/hooks/pre-push (which core.hooksPath=.githooks would otherwise hide; it runs first and a failure stops the
 * push) and the gate. Between them runs the app check, `rightkit-gate check --commit HEAD`: seconds, committed files only, so a
 * red tree stops the push before the gate's scans start; a push that only deletes refs has no content to check and skips it. The
 * gate binary is the root node_modules/.bin/rightkit-gate, else any workspace package's (up to three levels down: tauri-app-next,
 * apps/*, packages/*), else `pnpm exec rightkit-gate`, else node with a package-relative path when pnpm is absent; the line it
 * chose is printed once. No absolute path is written: everything is resolved from git at run time.
 */
export function hookText(nodePath) {
  return `#!/bin/sh
# Mandatory local check and gate before every push (rightkit-gate install-hook). Never bypass with --no-verify.
cd "$(git rev-parse --show-toplevel)" || exit 1
stdin=$(mktemp "\${TMPDIR:-/tmp}/rightkit-gate.XXXXXX") || exit 1
trap 'rm -f "$stdin"' EXIT
cat > "$stdin"
# core.hooksPath hides .git/hooks: run the repository's own pre-push hook first.
own="$(git rev-parse --git-dir)/hooks/pre-push"
if [ -x "$own" ]; then
  echo "rightkit-gate: running $own" >&2
  "$own" "$@" < "$stdin" || exit $?
fi
# The gate binary: the root install, any workspace package's install, then pnpm exec.
found=""
for bin in node_modules/.bin/rightkit-gate */node_modules/.bin/rightkit-gate */*/node_modules/.bin/rightkit-gate */*/*/node_modules/.bin/rightkit-gate; do
  if [ -x "$bin" ]; then found="$bin"; break; fi
done
if [ -n "$found" ]; then
  echo "rightkit-gate: using $found" >&2
  rightkit_gate() { "$found" "$@"; }
elif command -v pnpm >/dev/null 2>&1; then
  echo "rightkit-gate: using pnpm exec rightkit-gate" >&2
  rightkit_gate() { pnpm exec rightkit-gate "$@"; }
else
  echo "rightkit-gate: using node ${nodePath}" >&2
  rightkit_gate() { node "${nodePath}" "$@"; }
fi
# The check first, over the committed files of HEAD; a push that only deletes refs has nothing to check.
if [ -s "$stdin" ] && ! awk '$2 !~ /^0+$/ { found = 1 } END { exit !found }' "$stdin"; then
  echo "rightkit-gate: only deletions are pushed; nothing to check" >&2
else
  rightkit_gate check --commit HEAD < /dev/null >&2 || exit $?
fi
rightkit_gate --base origin/main < "$stdin"; exit $?
`;
}

/** A trampoline for a hook other than pre-push: core.hooksPath hides .git/hooks/<name>, so run that one when it exists. */
export function trampolineText(name) {
  return `#!/bin/sh
# rightkit-gate install-hook: core.hooksPath=.githooks hides .git/hooks, so run this repository's own ${name} hook from there.
hook="$(git rev-parse --git-dir)/hooks/${name}"
if [ -x "$hook" ]; then exec "$hook" "$@"; fi
exit 0
`;
}

// EVERY file present in .git/hooks is chained, whatever git version or hook family it belongs to (pre-receive, update and
// post-receive included), except git's own `.sample` templates. The name goes into a committed shell script, so only plain file
// names are chained; anything else is reported and skipped, never interpolated.
const HOOK_FILE_NAME = /^[A-Za-z0-9][A-Za-z0-9_-]*$/;
const chainableHook = (name) => !name.endsWith(".sample") && HOOK_FILE_NAME.test(name);
// ---- result cache inputs ------------------------------------------------------------------------------------

// The build inputs that change how every crate compiles, relative to the Cargo workspace root, plus the gate's own config.
const SHARED_RUST_INPUTS = ["Cargo.lock", "Cargo.toml", "rust-toolchain", "rust-toolchain.toml", ".cargo", "deny.toml", "clippy.toml", ".clippy.toml", "rustfmt.toml", ".rustfmt.toml", "vendor"];
const GATE_CONFIG = "ci/gate.config.json";
// What cargo, clippy, rustfmt and cargo-deny also read from EVERY ancestor directory of the workspace (the repository root and each
// directory between it and the workspace: they search upward), relative to that directory.
const ANCESTOR_RUST_INPUTS = [".cargo", "rust-toolchain", "rust-toolchain.toml", "clippy.toml", ".clippy.toml", "rustfmt.toml", ".rustfmt.toml", "deny.toml"];

/**
 * The unit a cargo scan id belongs to (clippy:x, check:x, check-features:x:0, check-dependent:x, doc:x, test:rust:x,
 * test:rust-serial:x:1), else null. A unit below the root is named `<cargoDir>:<crate>`, so the colon no longer ends the name:
 * pass `known` (the set of unit names) and the longest known name wins, with or without the trailing feature-set index. Without
 * `known` the name is the first segment, as for a repository with one root workspace.
 */
export function rustUnitOfScanId(id, known = null) {
  const parts = String(id).split(":");
  let rest;
  if (["clippy", "check", "check-features", "check-dependent", "doc"].includes(parts[0])) rest = parts.slice(1);
  else if (parts[0] === "test" && (parts[1] === "rust" || parts[1] === "rust-serial")) rest = parts.slice(2);
  else return null;
  if (!(known instanceof Set)) return rest[0] ?? null;
  for (let take = rest.length; take >= 1; take -= 1) { const name = rest.slice(0, take).join(":"); if (known.has(name)) return name; }
  return null;
}

// Compile scans (clippy, check, check-features, check-dependent, doc) read the crate's closure plus whatever its sources pull
// in from outside it: include!, include_str!, include_bytes!, a path literal with "../", and a build.rs that reads files.
const COMPILE_SCAN_PREFIXES = new Set(["clippy", "check", "check-features", "check-dependent", "doc"]);
const INCLUDE_MACRO = /\binclude(?:_str|_bytes)?\s*!\s*\(/g;
const STRING_BODY = String.raw`((?:[^"\\\n]|\\.)*)`;
const INCLUDE_LITERAL = new RegExp(String.raw`^\s*"` + STRING_BODY + String.raw`"\s*[,)]`);
const INCLUDE_MANIFEST = new RegExp(String.raw`^\s*concat!\s*\(\s*env!\s*\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*"` + STRING_BODY + String.raw`"\s*\)\s*[,)]`);
const INCLUDE_OUT_DIR = /^\s*concat!\s*\(\s*env!\s*\(\s*"OUT_DIR"\s*\)/;
// Any path-like token holding a "../" segment, inside a string literal or not: a stray quote cannot hide one.
const PARENT_PATH = /(?:[\w@.-]+[\\/])*\.\.(?:[\\/][\w@.-]*)+/g;
const BUILD_READS_FILES = /\b(?:fs::(?:read|read_to_string|read_dir|metadata|File)|File::open|OpenOptions|read_to_string|read_dir|walkdir|glob)\b/;
const BUILD_LEAVES_CRATE = /\.(?:parent|ancestors|pop)\s*\(|CARGO_WORKSPACE_DIR|workspace_root|current_dir/;
const RERUN_LITERAL = /cargo::?rerun-if-changed=([^"\\{}\n]+)/g;
// `#[path = "x.rs"]` on a module, and a plain `mod x;` declaration (no body): both name another source file.
const PATH_ATTRIBUTE = new RegExp(String.raw`#\s*!?\s*\[\s*path\s*=\s*"` + STRING_BODY + String.raw`"\s*\]`, "g");
const MOD_DECLARATION = /(?:^|[;{}\s\]])(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;/g;
// The most source files one crate's traversal reads before it gives up and keys the scan on the whole tree.
export const EXTERNAL_INPUT_FILE_LIMIT = 10_000;

const within = (dir, p) => dir === "." || p === dir || p.startsWith(`${dir}/`);

/**
 * Repository paths a crate's sources reach OUTSIDE their own directory, found statically: the literal targets of include!,
 * include_str! and include_bytes! (relative to the including file, or to CARGO_MANIFEST_DIR through concat!), every string
 * literal containing "../" (tried against the including file's and the crate's directory; only paths that exist count), and the
 * rerun-if-changed literals of a build.rs that reads files. `unresolved` names the first reference that cannot be resolved
 * statically (a computed include path, a build.rs that reads files and walks out of its crate, a path that leaves the
 * repository, an unreadable file, more than EXTERNAL_INPUT_FILE_LIMIT files); the caller then keys on the whole tree. `exists(path)`
 * says whether a repo path exists.
 *
 * The traversal runs to a fixed point: every out-of-crate .rs file reached is itself scanned for the same references (include!,
 * include_str!, include_bytes!, `#[path = ".."]`, `mod x;` declarations of an out-of-crate file, "../" literals), whatever the
 * depth (lib.rs -> shared/one.rs -> shared/two.rs -> shared/three.rs reaches three.rs), with a visited set so cycles end.
 */
// A source (a test, usually) that navigates out of its crate at run time without naming the path: the workspace root, the
// current directory, a bare ".." segment, or the manifest directory's parent. Its tests may read anything in the repository.
const TEST_LEAVES_CRATE = /CARGO_WORKSPACE_DIR|workspace_root|current_dir\s*\(|"\.\.\/?"|CARGO_MANIFEST_DIR[^;\n]{0,120}\.parent\s*\(|\.ancestors\s*\(/;

export function externalInputs({ dir, files, readBlob, exists, includeRuntimePaths = true }) {
  const paths = new Set(); let unresolved = null; let leavesCrate = null;
  const stop = (why) => { unresolved ??= why; };
  // Resolve `rel` against each base in turn; the path counts when it exists inside the repository and outside the crate. A
  // reference that leaves the repository from EVERY base is unresolvable (a path literal is tried against two bases, only
  // one of which is the one the code means).
  const consider = (from, bases, rel) => {
    const raw = String(rel).replaceAll("\\", "/").replace(/^\/+/, "");
    if (!raw || /^[A-Za-z]:/.test(raw)) return;
    let escapes = 0;
    for (const base of bases) {
      const resolved = path.posix.normalize(path.posix.join(base, raw)).replace(/\/+$/, "");
      if (resolved === ".." || resolved.startsWith("../")) { escapes += 1; continue; }
      if (within(dir, resolved) || paths.has(resolved)) continue;
      if (exists(resolved)) { paths.add(resolved); if (resolved.endsWith(".rs")) enqueue(resolved, { external: true }); }
    }
    if (escapes === bases.length) stop(`${from}: ${JSON.stringify(rel)} leaves the repository`);
  };
  const seen = new Set(); const queue = [];
  const enqueue = (file, options = {}) => {
    if (seen.has(file)) return;
    if (seen.size >= EXTERNAL_INPUT_FILE_LIMIT) { stop(`more than ${EXTERNAL_INPUT_FILE_LIMIT} source files are reachable`); return; }
    seen.add(file); queue.push([file, options]);
  };
  const scan = (file, { build = false, external = false } = {}) => {
    let text;
    try { text = readBlob(file); } catch (e) { stop(`${file}: unreadable (${e.message})`); return; }
    if (typeof text !== "string") { stop(`${file}: unreadable`); return; }
    const fileDir = path.posix.dirname(file);
    for (const m of text.matchAll(INCLUDE_MACRO)) {
      const rest = text.slice(m.index + m[0].length, m.index + m[0].length + 600);
      let hit;
      if ((hit = INCLUDE_LITERAL.exec(rest))) consider(file, [fileDir], hit[1]);
      else if ((hit = INCLUDE_MANIFEST.exec(rest))) consider(file, [dir], hit[1]);
      else if (INCLUDE_OUT_DIR.test(rest)) continue;
      else stop(`${file}: include macro with a computed path`);
    }
    // Publication content follows compile inputs; cache callers also retain conservative runtime-path reach.
    if (includeRuntimePaths || build) for (const m of text.matchAll(PARENT_PATH)) {
      consider(file, [fileDir, dir], m[0]);
    }
    // A module attribute names its file relative to the including file's directory (tried against the crate directory too).
    for (const m of text.matchAll(PATH_ATTRIBUTE)) consider(file, [fileDir, dir], m[1]);
    // An out-of-crate file's own `mod x;` declarations: x.rs / x/mod.rs beside a mod-rs file (mod.rs, lib.rs, main.rs), else under the file's own directory.
    if (external) {
      const base = path.posix.basename(file); const stem = base.slice(0, -3);
      const home = ["mod.rs", "lib.rs", "main.rs"].includes(base) ? fileDir : path.posix.join(fileDir, stem);
      for (const m of text.matchAll(MOD_DECLARATION)) for (const candidate of [`${m[1]}.rs`, `${m[1]}/mod.rs`]) consider(file, [home], candidate);
    }
    if (build && BUILD_READS_FILES.test(text)) {
      for (const m of text.matchAll(RERUN_LITERAL)) consider(file, [dir], m[1].trim());
      if (BUILD_LEAVES_CRATE.test(text)) stop(`${file}: a build script that reads files and navigates out of its crate`);
    }
    if (includeRuntimePaths && !build && !external && leavesCrate === null && TEST_LEAVES_CRATE.test(text)) leavesCrate = file;
  };
  for (const file of files) if (file.endsWith(".rs")) enqueue(file, { build: path.posix.basename(file) === "build.rs" && path.posix.dirname(file) === dir });
  // Fixed point: an out-of-crate .rs file that is reached is itself a source, so its own references are followed, and theirs.
  while (queue.length) { const [file, options] = queue.pop(); scan(file, options); }
  // `leavesCrate`: the first in-crate source that navigates out of the crate at run time without a literal path (tests key on
  // the whole tree then, unless ci/gate.config.json testInputs names what they read). Compile scans ignore it.
  return { paths: [...paths].sort(), unresolved, leavesCrate };
}

/**
 * The root manifest as a compile input: every table except `[patch.*]` (a patch changes a lock package's source, which the lock
 * projection records) and the membership arrays of `[workspace]` (a member is a lock package); comment-only lines dropped.
 * `[workspace.dependencies]` stays whole: its feature lists reach every `workspace = true` dependent without touching the lock.
 */
export function manifestCompileProjection(text) {
  const out = {};
  const strip = (t) => String(t ?? "").split("\n").filter((l) => !/^\s*(#.*)?$/.test(l)).join("\n");
  for (const [name, body] of manifestTables(text)) {
    if (/^patch\./.test(name)) continue;
    const kept = (name === "workspace" ? strip(body).replace(WORKSPACE_MEMBERSHIP_KEYS, "") : strip(body)).trim();
    if (kept) out[name] = kept;
  }
  return out;
}

const plainRelative = (p) => typeof p === "string" && p !== "" && !path.posix.isAbsolute(p) && !/^[A-Za-z]:/.test(p) && !p.includes("\\") && path.posix.normalize(p) === p && p !== "." && p !== ".." && !p.startsWith("../");

/**
 * The input digest function for the result cache: scan => string, or null when the scan must not be cached.
 * `objectId(path)` is the git blob/tree id of a repo-relative path in the gated commit, or null when the path is absent
 * (it throws when git cannot answer, which makes the scan uncacheable); `headTree` is that commit's tree id.
 * `listFiles(dir)` lists the gated commit's files under a directory and `readBlob(path)` reads one (both are required for any
 * per-crate key: without them a crate's reach outside its own directory is unknown and the whole tree is used).
 *
 * - A compile scan (clippy, check, check-features, check-dependent, doc) of crate X digests the object ids of X and of every
 *   workspace member in X's lock closure, the shared build inputs projected to what X consumes (the lock entries of X's closure,
 *   the root manifest without its [patch] tables and membership arrays, the `.cargo` files except audit.toml, the toolchain,
 *   clippy and rustfmt files of the workspace and every ancestor, the vendored crates in the closure, X's own ci/gate.config.json
 *   entry) and every repository path those members' sources reach outside their own directories (externalInputs). A reference
 *   that cannot be resolved statically keys the scan on the whole tree. deny.toml is no compile input (it feeds class A, never cached).
 * - A Rust test scan (test:rust*, test:rust-serial*) is keyed the same way, plus ci/gate.config.json `rust.<crate>.testInputs`
 *   when declared. Without a declaration, a closure source that navigates out of its crate at run time without a literal path
 *   (externalInputs `leavesCrate`: workspace root, current_dir, a bare "..", the manifest directory's parent) keys the scan on the
 *   whole HEAD tree, since its tests may read anything.
 * - An npm package scan (test:npm, tsc, eslint, biome) keys on the package's workspace closure (its `workspace:` dependencies,
 *   transitively, by package name) and the root npm inputs; test:scripts-ci on scripts/, ci/, .github/, packages/release and the
 *   root install graph. Every other class B scan (generated-docs, swift-build) uses the whole tree.
 * - Class A: cargo-deny, cargo-audit, pnpm-audit, pnpm-lockfile and advisory-exceptions are never cached (live answers); rustfmt,
 *   node-check, actionlint and the parse scans key on their files' blob ids; cargo-package-list on the crate, the manifest projection
 *   and its lock entry; the rest on the whole tree.
 */
// Root npm inputs every package scan reads: the install graph and the shared tool configuration.
const SHARED_NPM_INPUTS = ["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc", "tsconfig.json", "tsconfig.base.json", "eslint.config.js", "eslint.config.mjs", ".eslintrc.json", "biome.json", "vitest.config.ts", "vitest.config.mjs", "vitest.workspace.ts"];
// ESLint and Biome look their configuration up from the package directory through every ancestor. A config in an ancestor directory (or of another
// name than the shared list above) is part of a package's key only when the commit has it, so a repository without one keeps the key it had.
const LINT_CONFIG_NAMES = [...ESLINT_CONFIG_FILES, ...BIOME_CONFIG_FILES];
// The gate's own test suite reads the CI scripts, the gate config and the release package it imports from.
const SCRIPTS_CI_INPUTS = ["scripts", "ci", ".github", "packages/release", "package.json", "pnpm-lock.yaml"];

export function buildCacheDigest({ objectId, headTree, rustUnits = [], npmUnits = [], cargoDir = ".", rustLock = null, rustLocks = null, crateFeatures = {}, listFiles = null, readBlob = null }) {
  const memo = new Map(); const reachMemos = new Map(); const externalMemo = new Map();
  const idOf = (p) => { if (!memo.has(p)) memo.set(p, p === "." ? headTree : objectId(p)); return memo.get(p); };
  // npm: a package's test, tsc and eslint scans read the package, every workspace package it depends on (package.json
  // `workspace:` dependencies, transitively) and the root npm inputs. Package names come from the manifests themselves.
  const npmByUnit = new Map(npmUnits.map((u) => [u.name, u]));
  const npmManifestMemo = new Map();
  const npmManifest = (u) => {
    if (!npmManifestMemo.has(u.dir)) {
      let m = null;
      try { m = JSON.parse(String(readBlob(u.dir === "." ? "package.json" : `${u.dir}/package.json`)).replace(/^﻿/, "")); } catch { m = null; }
      npmManifestMemo.set(u.dir, m);
    }
    return npmManifestMemo.get(u.dir);
  };
  let npmByPkgName = null;
  const npmClosure = (unit) => {
    if (npmByPkgName === null) { npmByPkgName = new Map(); for (const u of npmUnits) { const m = npmManifest(u); if (m?.name) npmByPkgName.set(`${u.root ?? "."}\0${m.name}`, u); } }
    const seen = new Map([[unit.name, unit]]); const stack = [unit]; let unresolved = false;
    while (stack.length) {
      const u = stack.pop(); const m = npmManifest(u);
      if (!m) { unresolved = true; continue; }
      for (const section of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) {
        for (const [name, spec] of Object.entries(m[section] ?? {})) {
          if (!/^(workspace:|link:|file:)/.test(String(spec))) continue;
          const dep = npmByPkgName.get(`${u.root ?? "."}\0${name}`);
          if (!dep) { unresolved = true; continue; }
          if (!seen.has(dep.name)) { seen.set(dep.name, dep); stack.push(dep); }
        }
      }
    }
    return { members: [...seen.values()].sort((a, b) => (a.name < b.name ? -1 : 1)), unresolved };
  };
  // The npm inputs shared by a set of units: the repository root's (always, null when absent: the key a one-root repository has always had),
  // the same files of every other pnpm root the units belong to, and the linter configuration of their ancestor directories that the commit has.
  const sharedNpmInputs = (members) => {
    const required = new Set(SHARED_NPM_INPUTS); const optional = new Set();
    for (const m of members) {
      if ((m.root ?? ".") !== ".") for (const p of SHARED_NPM_INPUTS) required.add(inWs(m.root, p));
      for (const dir of ancestorDirsOf(m.dir)) if (dir !== m.dir) for (const f of LINT_CONFIG_NAMES) optional.add(inWs(dir, f));
    }
    const nested = [...required].filter((p) => !SHARED_NPM_INPUTS.includes(p)).sort();
    const extra = [...optional].filter((p) => !required.has(p)).sort();
    return [...SHARED_NPM_INPUTS, ...nested].map((p) => [p, idOf(p)]).concat(extra.map((p) => [p, idOf(p)]).filter(([, id]) => id !== null && id !== undefined));
  };
  // A unit belongs to the Cargo workspace it names (rustUnits[].cargoDir, else the global one) and is a package of that workspace's
  // own lock under its crate name (rustUnits[].crate, else its name); its closure never crosses into another workspace.
  const wsOf = (u) => u.cargoDir ?? cargoDir;
  const crateOf = (u) => u.crate ?? u.name;
  const lockFor = (ws) => (rustLocks?.[ws] instanceof Map ? rustLocks[ws] : ws === cargoDir && rustLock instanceof Map ? rustLock : null);
  const inWs = (ws, f) => (ws === "." ? f : `${ws}/${f}`);
  const byName = new Map(rustUnits.map((u) => [u.name, u]));
  const known = new Set(byName.keys());
  const byCrate = new Map(rustUnits.map((u) => [`${wsOf(u)}\0${crateOf(u)}`, u]));
  const whole = () => canonicalJson({ kind: "tree", tree: headTree });
  const externalOf = (member) => {
    if (!externalMemo.has(member.dir)) {
      externalMemo.set(member.dir, externalInputs({ dir: member.dir, files: listFiles(member.dir), readBlob, exists: (p) => { const id = idOf(p); return id !== null && id !== undefined; } }));
    }
    return externalMemo.get(member.dir);
  };
  // Class A: the live checks are never cached (their answer depends on the advisory databases and the registry of the day); a
  // content scan with an explicit file list is keyed on those files' blob ids; a crate's package list on the crate, its manifest
  // projection and its own lock entry; anything else (cargo metadata, licence, ownership catalog, secret scan) on the whole tree,
  // so it is reused for a rerun of the same commit and nothing else.
  const liveScan = (id) => /^(cargo-deny|cargo-audit|pnpm-audit|pnpm-lockfile|advisory-exceptions)(:|$)/.test(id);
  const fileList = (scan) => {
    if (Array.isArray(scan.files) && scan.files.length) return scan.files;
    const args = Array.isArray(scan.args) ? scan.args : [];
    if (/^rustfmt:/.test(scan.id)) return args.filter((x) => /\.rs$/.test(x));
    if (/^node-check:/.test(scan.id)) return [scan.id.slice("node-check:".length)];
    if (scan.id === "actionlint") return [...args.filter((x) => /\.ya?ml$/.test(x)), ".github/actionlint.yaml"];
    return null;
  };
  return (scan) => {
    if (scan.class === "A") {
      if (liveScan(scan.id)) return null;
      try {
        const files = fileList(scan);
        if (files) return canonicalJson({ kind: "files", id: scan.id, files: [...new Set(files)].sort().map((p) => [p, idOf(p)]) });
        if (/^cargo-package-list:/.test(scan.id)) {
          const unit = scan.id.slice("cargo-package-list:".length);
          const head = byName.get(unit);
          const ws = head ? wsOf(head) : null; const lock = ws === null ? null : lockFor(ws);
          if (!head || !lock || typeof readBlob !== "function") return whole();
          const manifestPath = inWs(ws, "Cargo.toml");
          let manifest;
          try { manifest = manifestCompileProjection(readBlob(manifestPath)); } catch { manifest = ["raw", manifestPath, idOf(manifestPath)]; }
          return canonicalJson({ kind: "package-list", unit, dir: [head.dir, idOf(head.dir)], manifest, lock: [[crateOf(head), lock.get(crateOf(head))?.signatures ?? []]] });
        }
        return whole();
      } catch { return null; }
    }
    if (scan.class !== "B") return null;
    try {
      const unit = rustUnitOfScanId(scan.id, known);
      if (!unit) {
        if (typeof readBlob !== "function") return whole();
        // npm package scans: the package's workspace closure plus the root npm inputs.
        const npmMatch = /^(test:npm|tsc|eslint|biome):(.+)$/.exec(scan.id);
        if (npmMatch && npmByUnit.has(npmMatch[2])) {
          const { members, unresolved } = npmClosure(npmByUnit.get(npmMatch[2]));
          if (unresolved) return whole();
          return canonicalJson({ kind: "npm", id: scan.id, members: members.map((m) => [m.name, m.dir, idOf(m.dir)]), shared: sharedNpmInputs(members) });
        }
        if (scan.id === "test:scripts-ci") return canonicalJson({ kind: "dirs", id: scan.id, inputs: SCRIPTS_CI_INPUTS.map((p) => [p, idOf(p)]) });
        return whole();
      }
      const head = byName.get(unit);
      const ws = wsOf(head); const lock = lockFor(ws);
      if (!lock || typeof listFiles !== "function" || typeof readBlob !== "function") return whole();
      const testScan = /^test:rust(?:-serial)?:/.test(scan.id);
      const ownConfig = crateFeatures?.[unit] ?? crateFeatures?.[crateOf(head)] ?? null;
      let testInputs = []; let declaredInputs = false;
      if (testScan) {
        const declared = ownConfig?.testInputs;
        if (declared !== undefined) {
          if (!Array.isArray(declared) || declared.length === 0 || !declared.every(plainRelative)) return whole();
          testInputs = [...new Set(declared)].sort(); declaredInputs = true;
        }
      } else if (!COMPILE_SCAN_PREFIXES.has(String(scan.id).split(":")[0])) return whole();
      if (!reachMemos.has(ws)) reachMemos.set(ws, new Map());
      const reached = [...reach(lock, crateOf(head), reachMemos.get(ws))];
      const closure = [head, ...reached.map((n) => byCrate.get(`${ws}\0${n}`)).filter(Boolean)].sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
      const extra = new Set(testInputs);
      for (const member of closure) {
        const found = externalOf(member);
        if (found.unresolved) return whole();
        if (testScan && !declaredInputs && found.leavesCrate) return whole();
        for (const p of found.paths) extra.add(p);
      }
      // The shared inputs, projected to what this crate consumes.
      const dirs = ancestorDirsOf(ws);
      const ancestors = dirs.filter((dir) => dir !== ws);
      const lockNames = [crateOf(head), ...reached].sort();
      const lockEntries = lockNames.map((n) => [n, lock.get(n)?.signatures ?? []]);
      const vendored = lockNames.map((n) => `vendor/${n}`).filter((p) => idOf(p) !== null && idOf(p) !== undefined).map((p) => [p, idOf(p)]);
      const manifestPath = inWs(ws, "Cargo.toml");
      let manifest;
      try { manifest = manifestCompileProjection(readBlob(manifestPath)); } catch { manifest = ["raw", manifestPath, idOf(manifestPath)]; }
      const cargoConfig = dirs.flatMap((dir) => {
        const cfg = inWs(dir, ".cargo");
        if (idOf(cfg) === null || idOf(cfg) === undefined) return [];
        try { return listFiles(cfg).filter((f) => !f.endsWith("/audit.toml")).sort().map((f) => [f, idOf(f)]); } catch { return [[cfg, idOf(cfg)]]; }
      });
      const toolFiles = [...SHARED_RUST_INPUTS.filter((p) => !["Cargo.lock", "Cargo.toml", ".cargo", "deny.toml", "vendor"].includes(p)).map((p) => inWs(ws, p)),
        ...ancestors.flatMap((dir) => ANCESTOR_RUST_INPUTS.filter((p) => ![".cargo", "deny.toml"].includes(p)).map((p) => inWs(dir, p)))];
      return canonicalJson({
        kind: testScan ? "rust-test" : "rust", unit, cargoDir: ws,
        members: closure.map((m) => [m.name, m.dir, idOf(m.dir)]),
        lock: lockEntries, manifest, cargoConfig, vendored, config: ownConfig,
        shared: [...new Set(toolFiles)].sort().map((p) => [p, idOf(p)]),
        external: [...extra].sort().map((p) => [p, idOf(p)]),
      });
    } catch { return null; }
  };
}

// ---- quiet lease ------------------------------------------------------------------------------------------------

// The broker's quiet lease TTL ceiling is 30 minutes (RustRight, monotonic clock); an expired lease is not refused, the lane just
// loses its protection, so a quiet lane longer than this is reported by the gate when it happens, never hidden.
export const QUIET_LEASE_TTL_MS = 30 * 60 * 1000;
export const QUIET_LEASE_WAIT_DRAIN_MS = 2 * 60 * 1000;

/**
 * The runScans `quietLane` for a host with RustRight (the reason carries no spaces: a .cmd shim on Windows re-splits its arguments): `rustright quiet acquire` before the quiet test scans (waits up to two
 * minutes for running broker jobs to drain), `RUSTRIGHT_QUIET_LEASE=<id>` in their environment, `rustright quiet release` after.
 * Without `rustright` on PATH, or when the broker refuses, the lane runs without a lease and the gate says so: the tests still run,
 * their verdict is simply taken on a possibly busy host. `o.spawn` is the gate's synchronous spawner.
 */
export function quietLeaseLane(o, { tag = "rightkit-gate:", reason = "rightkit-gate-quiet-tests", ttlMs = QUIET_LEASE_TTL_MS, waitDrainMs = QUIET_LEASE_WAIT_DRAIN_MS, pid = process.pid } = {}) {
  const which = (cmd) => {
    const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]);
    return failed(r) ? null : (text(r).split(/\r?\n/).map((l) => l.trim()).find(Boolean) ?? null);
  };
  return {
    acquire: async () => {
      if (!which("rustright")) { o.out(`${tag} note: no rustright on PATH; quiet test scans run without a broker lease`); return null; }
      const started = Date.now();
      const r = o.spawn("rustright", ["quiet", "acquire", "--ttl-ms", String(ttlMs), "--reason", reason, "--wait-drain-ms", String(waitDrainMs), "--holder-pid", String(pid), "--json"]);
      if (failed(r)) { o.out(`${tag} note: broker quiet lease refused (${(why(r) || `exit ${r?.status}`).slice(0, 200)}); quiet test scans run without a lease`); return null; }
      let id = null;
      // The CLI prints the broker's QUIET_LEASE event: { type, granted, state, id } when held; a refusal exits non-zero above.
      try { const j = JSON.parse(text(r)); id = j?.granted === false ? null : (j?.id ?? j?.lease?.id ?? null); } catch { id = null; }
      if (typeof id !== "string" || !id) { o.out(`${tag} note: broker quiet lease answer has no lease id (${text(r).trim().slice(0, 200)}); quiet test scans run without a lease`); return null; }
      o.out(`${tag} broker quiet lease ${id} held (ttl ${Math.round(ttlMs / 60000)} min; waited ${Math.round((Date.now() - started) / 1000)} s for the broker to drain)`);
      return { id, env: { RUSTRIGHT_QUIET_LEASE: id }, at: Date.now() };
    },
    release: async (id) => {
      const r = o.spawn("rustright", ["quiet", "release", String(id)]);
      if (failed(r)) o.out(`${tag} note: broker quiet lease ${id} release failed (${(why(r) || `exit ${r?.status}`).slice(0, 200)}); it expires on its own`);
      else o.out(`${tag} broker quiet lease ${id} released`);
    },
  };
}



// ---- install-hook -------------------------------------------------------------------------------------------

/** The first tracked package.json (or the root one when git cannot list) that declares @rightkit/release, or is that package itself. */
function releaseProvider(o, root, git) {
  let files = ["package.json"];
  const listed = git("ls-files", "-z", "--", ":(glob)**/package.json");
  if (!failed(listed)) files = text(listed).split("\0").filter(Boolean).map(posix);
  for (const file of files) {
    const abs = path.join(root, file);
    if (!o.exists(abs)) continue;
    let manifest;
    try { manifest = JSON.parse(o.readFile(abs).replace(/^\uFEFF/, "")); } catch { continue; }
    if (manifest?.name === "@rightkit/release") return file;
    for (const section of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) if (manifest?.[section] && Object.hasOwn(manifest[section], "@rightkit/release")) return file;
  }
  return null;
}

// The advisory ownership pre-commit step (`right-release hooks install`, ownership-hooks.mjs) runs the pinned @rightkit/release, so
// it is written only where that is installed. Its own hook and the repository's .git/hooks/pre-commit both keep running.
const PINNED_CLI = "node_modules/@rightkit/release/cli/right-release.mjs";
const FOREIGN_PRE_COMMIT = "pre-commit.rightkit-foreign";

async function installHook(o, root, force) {
  const git = (...a) => o.spawn("git", a, { cwd: root });
  const hooksDir = path.join(root, ".githooks");
  const hookPath = path.join(hooksDir, "pre-push");
  const installed = "node_modules/@rightkit/release/bin/rightkit-gate.mjs";
  const nodePath = o.exists(path.join(root, installed)) ? installed : posix(path.relative(root, fileURLToPath(import.meta.url)));
  const wanted = hookText(nodePath);
  const hasHook = o.exists(hookPath);
  const current = hasHook ? o.readFile(hookPath).replace(/\r\n/g, "\n") : null;
  if (hasHook && current !== wanted && !force) {
    o.err(`rightkit-gate: ${path.join(".githooks", "pre-push")} already exists and differs; nothing changed. Review it, or run \`rightkit-gate install-hook --force\` to replace it.`);
    return 1;
  }
  // core.hooksPath=.githooks makes git ignore .git/hooks: chain the hooks that are there now (pre-push at run time inside the gate hook).
  const gitDirResult = git("rev-parse", "--git-dir");
  let present = [];
  if (!failed(gitDirResult) && text(gitDirResult).trim()) { try { present = o.readdir(path.join(path.resolve(root, text(gitDirResult).trim()), "hooks")); } catch { present = []; } }
  const gitHooksDir = !failed(gitDirResult) && text(gitDirResult).trim() ? path.join(path.resolve(root, text(gitDirResult).trim()), "hooks") : null;
  const own = present.filter(chainableHook).sort();
  // The ownership step takes .githooks/pre-commit. The repository's own pre-commit hook keeps running first, through a trampoline
  // beside it, unless it is the ownership hook a plain `right-release hooks install` wrote (this one replaces that), whose own
  // chained hook, pre-commit.rightkit-foreign, then runs instead. A name that is no plain hook name is reported, never chained.
  const ownership = o.exists(path.join(root, PINNED_CLI)) ? await import("../ownership-hooks.mjs") : null;
  const readHook = (name) => { try { return o.readFile(path.join(gitHooksDir, name)); } catch { return ""; } };
  const chainName = !ownership ? null : own.includes("pre-commit") && !readHook("pre-commit").includes(ownership.HOOK_MARKER) ? "pre-commit" : present.includes(FOREIGN_PRE_COMMIT) ? FOREIGN_PRE_COMMIT : null;
  const skipped = present.filter((name) => !name.endsWith(".sample") && !chainableHook(name) && !(ownership && name === FOREIGN_PRE_COMMIT)).sort();
  if (skipped.length) o.err(`rightkit-gate: WARNING: not chaining .git/hooks/${skipped.join(", .git/hooks/")}: not a plain hook file name (letters, digits, - and _ only); run it from a hook of its own.`);
  const trampolines = own.filter((name) => name !== "pre-push" && !(ownership && name === "pre-commit")).map((name) => ({ name, file: path.join(hooksDir, name), wanted: trampolineText(name), note: `runs .git/hooks/${name}` }));
  if (ownership) {
    trampolines.push({ name: "pre-commit", file: path.join(hooksDir, "pre-commit"), wanted: ownership.renderPreCommitHook(), ownership: true, replaces: [trampolineText("pre-commit")], marker: ownership.HOOK_MARKER, note: "the advisory ownership gate: right-release ownership --staged" });
    if (chainName) trampolines.push({ name: FOREIGN_PRE_COMMIT, file: path.join(hooksDir, FOREIGN_PRE_COMMIT), wanted: trampolineText(chainName), note: `runs .git/hooks/${chainName}, before the ownership gate` });
  }
  for (const t of trampolines) {
    t.exists = o.exists(t.file);
    const found = t.exists ? o.readFile(t.file).replace(/\r\n/g, "\n") : null;
    t.current = found === t.wanted;
    // Our own earlier artifacts are updated in place: a plain trampoline where the ownership hook now goes, and an older ownership hook.
    const ours = found !== null && (t.current || t.replaces?.includes(found) || (t.marker && found.includes(t.marker)));
    if (t.exists && !ours && !force) {
      o.err(`rightkit-gate: .githooks/${t.name} already exists and differs; nothing changed. Review it, or run \`rightkit-gate install-hook --force\` to replace it with ${t.ownership ? "the ownership pre-commit hook" : `a trampoline to .git/hooks/${chainName ?? t.name}`}.`);
      return 1;
    }
  }
  const currentPath = git("config", "--get", "core.hooksPath");
  const existingPath = failed(currentPath) ? "" : text(currentPath).trim();
  if (existingPath && existingPath !== ".githooks" && !force) {
    o.err(`rightkit-gate: core.hooksPath is already ${JSON.stringify(existingPath)}; nothing changed. Run \`rightkit-gate install-hook --force\` to point it at .githooks.`);
    return 1;
  }
  const done = [];
  const put = (file, body, label, note) => {
    o.mkdir(path.dirname(file), { recursive: true }); o.writeFile(file, body);
    try { o.chmod(file, 0o755); } catch { /* not supported here */ }
    done.push(`${label}${note}`);
  };
  if (current === wanted) done.push(".githooks/pre-push already current");
  else put(hookPath, wanted, `${hasHook ? "replaced" : "wrote"} .githooks/pre-push`, ` (${nodePath} without pnpm)`);
  for (const t of trampolines) {
    if (t.current) done.push(`.githooks/${t.name} already current`);
    else put(t.file, t.wanted, `${t.exists ? "replaced" : "wrote"} .githooks/${t.name}`, ` (${t.note})`);
  }
  if (existingPath === ".githooks") done.push("core.hooksPath already .githooks");
  else {
    const set = git("config", "core.hooksPath", ".githooks");
    if (failed(set)) { o.err(`rightkit-gate: git config core.hooksPath failed: ${why(set)}`); return 2; }
    done.push(`set core.hooksPath to .githooks${existingPath ? ` (was ${existingPath})` : ""}`);
  }
  // The agent side of the one door: a Claude Code PreToolUse hook in the repository's .claude/settings.json runs door-policy.mjs on every
  // shell command, on every host (the broker-less cloud included), and denies any build that does not go through rightkit-gate.
  const agentHookFile = "node_modules/@rightkit/release/bin/rightkit-agent-hook.mjs";
  const agentHook = o.exists(path.join(root, agentHookFile)) ? agentHookFile : posix(path.relative(root, fileURLToPath(new URL("./rightkit-agent-hook.mjs", import.meta.url))));
  const settingsPath = path.join(root, ".claude", "settings.json");
  let settings = {};
  if (o.exists(settingsPath)) { try { settings = JSON.parse(o.readFile(settingsPath)); } catch { o.err("rightkit-gate: .claude/settings.json is not JSON; the agent hook was not added"); settings = null; } }
  if (settings) {
    const command = `node ${agentHook}`;
    const pre = Array.isArray(settings.hooks?.PreToolUse) ? settings.hooks.PreToolUse : [];
    const has = pre.some((entry) => (entry.hooks ?? []).some((h) => typeof h.command === "string" && h.command.includes("rightkit-agent-hook")));
    if (has) done.push(".claude/settings.json agent hook already present");
    else {
      settings.hooks = { ...(settings.hooks ?? {}), PreToolUse: [...pre, { matcher: "Bash", hooks: [{ type: "command", command }] }] };
      try { o.mkdir(path.dirname(settingsPath), { recursive: true }); } catch { /* exists */ }
      o.writeFile(settingsPath, `${JSON.stringify(settings, null, 2)}\n`);
      done.push("added the one-door agent hook to .claude/settings.json (commit it)");
    }
  }
  for (const line of done) o.out(`rightkit-gate: ${line}`);
  const chained = own.length ? own.map((name) => (name === "pre-push" ? "pre-push (run first, then the gate)" : name)) : [];
  o.out(`rightkit-gate: chained hooks from .git/hooks: ${chained.length ? chained.join(", ") : "none"}${own.includes("pre-push") ? "" : "; the pre-push hook still runs .git/hooks/pre-push first if one appears later"}`);
  if (!ownership) o.out(`rightkit-gate: no ownership pre-commit step: ${PINNED_CLI} is not installed here; install @rightkit/release, then run install-hook again`);
  const files = [".githooks/pre-push", ...trampolines.map((t) => `.githooks/${t.name}`)];
  o.out(`rightkit-gate: commit the hooks with executable mode (git add --chmod=+x ${files.join(" ")}); the pre-push hook finds rightkit-gate in node_modules/.bin of the repository root, else of any workspace package, else through \`pnpm exec\`.`);
  const provider = releaseProvider(o, root, git);
  if (!provider) o.err("rightkit-gate: WARNING: no package.json in this repository depends on @rightkit/release, so the pre-push hook cannot find the rightkit-gate binary; add @rightkit/release as a devDependency of the root or of a workspace package and install.");
  else o.out(`rightkit-gate: @rightkit/release is declared in ${provider}`);
  return 0;
}

// ---- the stages: rightkit-gate --to <stage> --------------------------------------------------------------------------------

/**
 * Runs every stage up to `--to`, in order, for HEAD: check (class A), compile (the whole gate over every file), then, through the existing
 * owners, package (source QA, then `right-release build`, which seals the exact artifact set), qualify (installed QA on that sealed set)
 * and release (`right-release upload` of that same sealed set). Stops at the first stage that fails. check and compile are reused from
 * the receipt only when every bound input is unchanged; the right-release stages run under a live door run, so they refuse any other
 * caller (gate-stages.mjs requireDoorRun).
 */
async function runStages(o, args, root, git, io) {
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  const tag = "rightkit-gate:";
  const to = option("--to");
  const stages = stagesTo(to);
  if (!stages) { o.err(`${tag} --to must be one of ${STAGES.join(", ")}`); return 2; }
  const platform = option("--platform");
  const tier = option("--tier");
  if (stages.includes("package") && !["mac", "win"].includes(platform)) { o.err(`${tag} --to ${to} needs --platform mac|win`); return 2; }
  if (stages.includes("release") && !["patch", "update"].includes(tier)) { o.err(`${tag} --to release needs --tier patch|update (chosen explicitly, never defaulted)`); return 2; }
  const commit = text(git("rev-parse", "HEAD")).trim();
  const tree = text(git("rev-parse", "HEAD^{tree}")).trim();
  const common = text(git("rev-parse", "--git-common-dir")).trim();
  if (!/^[0-9a-f]{40}$/.test(commit) || !tree || !common) { o.err(`${tag} cannot read HEAD and the git directory`); return 2; }
  const commonDir = path.resolve(root, common);
  const dirty = git("status", "--porcelain", "--untracked-files=no");
  if (failed(dirty) || text(dirty).trim()) { o.err(`${tag} BLOCKED: the checkout differs from HEAD; every stage works on the commit, so commit or stash first`); return 1; }

  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile });
  if (executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
  const version = (cmd, a) => { const r = o.spawn(cmd, a, { cwd: root }); return failed(r) ? null : text(r).trim().split(/\r?\n/)[0]; };
  const listed = git("ls-tree", "-r", "--name-only", "-z", "HEAD");
  const lockFiles = {};
  for (const f of failed(listed) ? [] : text(listed).split("\0").filter((x) => /(^|\/)(Cargo\.lock|pnpm-lock\.yaml)$/.test(x))) {
    const shown = git("show", `HEAD:${f}`); if (!failed(shown)) lockFiles[f] = text(shown);
  }
  const binding = bindingOf({
    tree, lockFiles, gateFile: fileURLToPath(import.meta.url), executor: executor.kind,
    toolchain: { node: process.version, rustc: version(executor.kind === "broker" && executor.cargo[0] === "rustright" ? "rustright" : "rustc", executor.kind === "broker" && executor.cargo[0] === "rustright" ? ["rustc", "-V"] : ["-V"]), pnpm: version("pnpm", ["--version"]) },
  });
  // --receipt-in <file>: the receipt an earlier job of the same pipeline wrote (another runner: CI's package job, then its qualify and
  // publish jobs). It is taken only for this commit and the same source binding (tree, lockfiles, gate); it then stands in for the
  // stages it records, and the sealed bytes on disk must still hash to its manifest digest before anything uses them.
  const receiptIn = option("--receipt-in");
  if (receiptIn !== undefined) {
    let imported = null;
    try { imported = JSON.parse(o.readFile(path.resolve(o.cwd, receiptIn))); } catch { /* refused below */ }
    if (!imported || imported.schema !== RECEIPT_SCHEMA || imported.commit !== commit || !sameSource(imported.binding, binding)) {
      o.err(`${tag} BLOCKED: --receipt-in ${receiptIn} is not a ${RECEIPT_SCHEMA} receipt for ${commit.slice(0, 12)} with this tree, these lockfiles and this gate`);
      return 1;
    }
    (o.writeReceipt ?? writeReceipt)({ commonDir, receipt: imported });
  }
  const previous = (o.readReceipt ?? readReceipt)({ commonDir, commit });
  // Same host and inputs: every recorded stage counts. Same source on another host: only the stages bound to sealed bytes (package and
  // later) carry over; check and compile run again here.
  const receipt = previous && sameBinding(previous.binding, binding) ? previous
    : previous && sameSource(previous.binding, binding) ? { ...previous, binding, stages: Object.fromEntries(Object.entries(previous.stages ?? {}).filter(([k]) => k !== "check" && k !== "compile")) }
      : { schema: RECEIPT_SCHEMA, commit, binding, stages: {} };
  const receiptOut = option("--receipt-out");
  const save = () => {
    (o.writeReceipt ?? writeReceipt)({ commonDir, receipt });
    if (receiptOut !== undefined) o.writeFile(path.resolve(o.cwd, receiptOut), `${JSON.stringify(receipt, null, 2)}\n`);
  };
  // Release writes to R2. The write credential lives only in the trusted publisher: a GitHub Actions job in the protected
  // publication environment, which sets RIGHTKIT_PUBLISHER=github-environment. Hosts and agents never hold it, so the door refuses the
  // stage anywhere else rather than fail half way through an upload.
  if (stages.includes("release") && !(o.env.GITHUB_ACTIONS === "true" && o.env.RIGHTKIT_PUBLISHER === "github-environment")) {
    o.err(`${tag} BLOCKED: --to release runs only in the trusted publisher (the release workflow's protected publication job). Run --to qualify here, then dispatch the release workflow for ${commit.slice(0, 12)}`);
    return 1;
  }

  const cli = fileURLToPath(new URL("../cli/right-release.mjs", import.meta.url));
  const rightRelease = (stage, cliArgs) => {
    const run = startRun({ commonDir, commit, tree, stage });
    try {
      o.out(`${tag} [${stage}] right-release ${cliArgs.join(" ")}`);
      const r = (o.spawnInherit ?? ((cmd, a, opts) => spawnInherit(cmd, a, { ...opts, stdio: "inherit" })))(process.execPath, [cli, ...cliArgs], { cwd: root, env: { ...o.env, ...run.env } });
      return r.status === 0;
    } finally { endRun(run); }
  };

  o.out(`${tag} --to ${to} for ${commit.slice(0, 12)}: ${stages.join(" -> ")}; executor ${executor.kind}`);
  // A later stage on the sealed bytes: check, compile and package already passed for them (in this receipt) and the bytes on disk are
  // the ones they sealed, so they are not run again.
  const sealedNow = receipt.stages.package?.ok ? (o.sealedFor ?? sealedFor)({ root, commit, platform }) : null;
  const sealedIntact = Boolean(sealedNow && sealedNow.id === receipt.stages.package.sealed && sealedNow.digest === receipt.stages.package.manifestSha256);
  for (const stage of stages) {
    if ((stage === "check" || stage === "compile") && receipt.stages[stage]?.ok) { o.out(`${tag} [${stage}] already passed for this exact input (commit, tree, lockfiles, gate, toolchain)`); continue; }
    if ((stage === "check" || stage === "compile" || stage === "package") && sealedIntact) { o.out(`${tag} [${stage}] already passed for the sealed set ${sealedNow.id} (manifest ${sealedNow.digest.slice(0, 12)})`); continue; }
    let ok = false; const at = new Date().toISOString(); let extra = {};
    if (stage === "check") ok = (await (o.runCheck ?? (() => main(["check", "--commit", "HEAD"], { ...io, cwd: root })))()) === 0;
    else if (stage === "compile") ok = (await (o.runCompile ?? (() => main(["--all"], { ...io, cwd: root, readStdin: () => "" })))()) === 0;
    else if (stage === "package") {
      ok = rightRelease(stage, ["qa", "--stage", "source", "--platform", platform]) && rightRelease(stage, ["build", "--platform", platform]);
      const sealed = ok ? (o.sealedFor ?? sealedFor)({ root, commit, platform }) : null;
      if (ok && !sealed) { o.err(`${tag} [package] the build reported success but sealed nothing for ${commit.slice(0, 8)} on ${platform}`); ok = false; }
      if (sealed) extra = { platform, sealed: sealed.id, manifestSha256: sealed.digest };
    } else {
      const sealed = receipt.stages.package?.ok ? receipt.stages.package : null;
      const current = sealed ? (o.sealedFor ?? sealedFor)({ root, commit, platform }) : null;
      if (!sealed || !current || current.id !== sealed.sealed || current.digest !== sealed.manifestSha256) {
        o.err(`${tag} BLOCKED: [${stage}] needs the sealed set the package stage produced for this commit, unchanged (sealed manifest digest)`);
        ok = false;
      } else if (stage === "qualify") {
        ok = rightRelease(stage, ["qa", "--stage", "installed", "--platform", platform, "--release", sealed.sealed]);
        extra = { sealed: sealed.sealed, manifestSha256: sealed.manifestSha256 };
      } else {
        if (!receipt.stages.qualify?.ok || receipt.stages.qualify.manifestSha256 !== sealed.manifestSha256) { o.err(`${tag} BLOCKED: [release] the sealed set was not qualified (installed QA) in this door run's receipt`); ok = false; }
        else { ok = rightRelease(stage, ["upload", "--release", sealed.sealed, "--platform", platform, "--tier", tier]); extra = { sealed: sealed.sealed, manifestSha256: sealed.manifestSha256, tier }; }
      }
    }
    receipt.stages[stage] = { ok, at, ...extra };
    save();
    if (!ok) { o.err(`${tag} --to ${to} stopped at ${stage}: nothing after it ran`); return 1; }
  }
  o.out(`${tag} --to ${to} passed: ${stages.join(", ")}`);
  return 0;
}

// ---- main ---------------------------------------------------------------------------------------------------

/**
 * Run the CLI. Returns the exit code (0 pass, 1 blocked or failed, 2 cannot run, 128+n cancelled). `io` overrides the
 * process, fs and output bindings so tests execute nothing real: spawn, exists, readFile, writeFile, mkdir, chmod,
 * readStdin, out, err, cwd, env, platform, abort (an AbortController), runScans and findRustfmt (the scan runner and the
 * lookup of a usable rustfmt, which the deferred Rust gate needs).
 */
export async function main(argv, io = {}) {
  const o = {
    spawn: defaultSpawn, exists: fs.existsSync, readFile: (f) => fs.readFileSync(f, "utf8"), writeFile: fs.writeFileSync, mkdir: fs.mkdirSync, chmod: fs.chmodSync, readdir: fs.readdirSync,
    readStdin: () => { try { return process.stdin.isTTY ? "" : fs.readFileSync(0, "utf8"); } catch { return ""; } },
    out: (m) => console.log(m), err: (m) => console.error(m), cwd: process.cwd(), env: process.env, platform: process.platform, ...io,
  };
  const args = [...argv];
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  if (args.includes("--version") || args.includes("-v")) {
    o.out(JSON.parse(fs.readFileSync(new URL("../package.json", import.meta.url), "utf8")).version);
    return 0;
  }
  if (args.includes("--help") || args.includes("-h")) { o.out(USAGE); return 0; }
  const sub = args[0] && !args[0].startsWith("-") ? args[0] : null;
  if (sub && sub !== "install-hook" && sub !== "check") { o.err(`rightkit-gate: unknown command ${JSON.stringify(sub)}\n${USAGE}`); return 2; }
  // `check` is the deterministic stage (Adrian, 2026-10-25: deterministic always first, one door): the app-check rules, then every
  // class A scan (lockfiles, cargo-deny, cargo-audit, secrets, licences, manifests, workflow lint, syntax...) over the whole tree of the
  // commit, never a diff. Nothing compiles. The RustRight broker runs exactly this before it admits any compile. `--list`,
  // `--print-baseline` and `--only` stay app-check-only views.
  if (sub === "check") {
    const appCheck = await (o.appCheck ?? (await import("../app-check.mjs")).main)(args.slice(1), { out: o.out, err: o.err, label: "check", cwd: o.cwd });
    if (args.includes("--list") || args.includes("--print-baseline") || args.includes("--only") || o.classA === false) return appCheck;
    const repoArg = option("--repo");
    const classA = await main(["--class-a", ...(args.includes("--worktree") ? ["--worktree"] : ["--commit", option("--commit") ?? "HEAD"])], { ...io, cwd: repoArg ? path.resolve(o.cwd, repoArg) : o.cwd });
    return Math.max(appCheck, classA);
  }
  const classAOnly = args.includes("--class-a");
  const classACommit = classAOnly && !args.includes("--worktree") ? (option("--commit") ?? "HEAD") : null;
  if (args.includes("--base") && (!option("--base") || option("--base").startsWith("--"))) { o.err("rightkit-gate: --base needs a ref"); return 2; }

  const top = o.spawn("git", ["rev-parse", "--show-toplevel"], { cwd: o.cwd });
  if (failed(top) || !text(top).trim()) { o.err("rightkit-gate: not inside a git repository"); return 2; }
  const root = path.resolve(text(top).trim());
  const git = (...a) => o.spawn("git", a, { cwd: root });
  if (sub === "install-hook") return await installHook(o, root, args.includes("--force"));
  if (option("--to") !== undefined) return await runStages(o, args, root, git, io);

  const planOnly = args.includes("--plan");
  let base = option("--base") ?? "origin/main";
  const tag = "rightkit-gate:";

  // git pre-push: "<local ref> <local sha> <remote ref> <remote sha>" per pushed ref. A plan reads no stdin.
  // `pushedRefs` is every remote ref named, deletions included: it decides whether the Rust gate may defer (rustDeferral). The fields are
  // split on single ASCII spaces only (parsePushLines): a ref name may hold Unicode whitespace. A line that does not read as four fields
  // (git hands the local field over verbatim, so a rev expression such as `HEAD@{1 minute ago}` or `HEAD^{/one thing}` holds spaces)
  // refuses the push on every host and in every repository, never a line that disappears: its pushed commit could not be compared with
  // HEAD and its remote ref could not be judged. `pushedRemoteShas` are the remote commits being replaced.
  if (!planOnly && !classAOnly) {
    const head = text(git("rev-parse", "HEAD")).trim();
    const parsed = parsePushLines(o.readStdin());
    if (parsed.malformed.length) {
      o.err(`${tag} BLOCKED: pre-push stdin line ${parsed.malformed[0].number} is not "<local ref> <local sha> <remote ref> <remote sha>" (four fields separated by single spaces), so the pushed refs cannot all be read and the pushed commit cannot be compared with HEAD; push a plain ref (git push <remote> HEAD:<branch>)`);
      return 1;
    }
    for (const { localSha, remoteRef, remoteSha } of parsed.lines) {
      if (ZERO.test(localSha)) continue; // a delete pushes no content
      if (localSha !== head) {
        o.err(`${tag} BLOCKED: pushing ${localSha.slice(0, 10)} to ${remoteRef}, but HEAD is ${head.slice(0, 10)}; broker scans read the checkout, so check the pushed commit out first (git checkout --detach ${localSha.slice(0, 10)}), push HEAD:<branch>, then return`);
        return 1;
      }
      if (!ZERO.test(remoteSha) && !failed(git("cat-file", "-e", `${remoteSha}^{commit}`))) base = remoteSha;
    }
  }

  // ---- one door: the executor, then the exact snapshot every scan plans from and reads -----------------------------------
  // The executor is chosen here, never by the caller (Adrian, 2026-10-25): the RustRight broker where this machine has it, a refusal
  // on a broker-managed machine whose broker cannot be reached, direct cargo on any other host (a cloud container, a CI runner).
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile });
  if (executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
  const cargoCommand = executor.cargo;
  // The snapshot: the commit or tree being checked (check --commit), HEAD for the gate, or the working tree (check --worktree, --plan).
  const allFilesMode = classAOnly || args.includes("--all");
  const snapshot = planOnly ? null : classAOnly ? classACommit : "HEAD";

  const ac = o.abort ?? new AbortController();
  let exportRoot = null;
  const cleanup = () => { if (exportRoot) fs.rmSync(exportRoot, { recursive: true, force: true }); };
  const signals = [];
  try {
    let repo = root;
    if (snapshot) {
      exportRoot = fs.mkdtempSync(path.join(o.env.RK_GATE_TMPDIR || os.tmpdir(), "rk-gate-"));
      repo = path.join(exportRoot, "repo");
      const made = o.spawn("git", ["archive", "--format=tar", "--prefix=repo/", "-o", path.join(exportRoot, "head.tar"), snapshot], { cwd: root });
      const x = failed(made) ? made : o.spawn("tar", ["-xf", "head.tar"], { cwd: exportRoot });
      if (failed(made) || failed(x)) { o.err(`${tag} cannot export ${snapshot}: ${why(made) || why(x)}`); return 2; }
    }
    // Every file of the snapshot, once: units, manifests, lockfiles and the changed set are all read from what is scanned, never from
    // the live checkout (a live edit cannot change what gets planned).
    const listed = snapshot ? git("ls-tree", "-r", "--name-only", "-z", snapshot) : git("ls-files", "-z", "--cached", ...(classAOnly ? ["--others", "--exclude-standard"] : []));
    if (failed(listed)) { o.err(`${tag} cannot list the files of ${snapshot ?? "the working tree"}: ${why(listed)}`); return 2; }
    const files = text(listed).split("\0").filter(Boolean).map(posix);
    const tracked = (name) => files.filter((f) => f === name || f.endsWith(`/${name}`));
    const readSnap = (f) => o.readFile(f);

    let rust; let npm; let npmRoots = []; let manifests = [];
    try {
      manifests = tracked("Cargo.toml");
      // ci/gate.config.json `fixtures` (optional): directories whose manifests are consumer or sample crates, gated through the
      // consumer rows of the crates they exercise (CI treats them the same), never as units.
      let fixtures = [];
      try { const f = JSON.parse(o.readFile(path.join(repo, GATE_CONFIG))).fixtures; if (Array.isArray(f)) fixtures = f.filter((d) => typeof d === "string"); } catch { /* no config: no fixtures */ }
      // A snapshot has nothing untracked; the working tree (--plan, check --worktree) names its untracked manifests as skipped.
      const untracked = () => {
        if (snapshot) return [];
        const r = git("ls-files", "-z", "--others", "--exclude-standard", "--", ":(glob)**/Cargo.toml");
        return failed(r) ? [] : text(r).split("\0").filter(Boolean).map(posix);
      };
      rust = discoverRustWorkspaces({ root: repo, spawn: (c, a, opts) => o.spawn(c, a, opts), cargoCommand, tracked, untracked, fixtures, readFile: readSnap });
      npm = discoverNpmUnits({ root: repo, tracked, readFile: readSnap, exists: o.exists });
      npmRoots = discoverPnpmRoots({ root: repo, tracked, exists: o.exists });
    } catch (e) { o.err(`${tag} BLOCKED: cannot discover the repository's units: ${e.message}`); return 1; }
    for (const k of rust.skipped) o.out(`${tag} note: ${k.manifest} is skipped, not scanned: ${k.why}`);

    // Class B scans (compile, lint, tests) run in the live checkout, so it must equal HEAD where they read it: an uncommitted fix could
    // otherwise hide a committed failure. Class A reads only the snapshot.
    if (!planOnly && !classAOnly) {
      const npmRootFiles = ["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc", ...ESLINT_CONFIG_FILES, ...BIOME_CONFIG_FILES];
      const inputs = [...new Set([...rust.units.map((u) => u.dir), ...npm.map((u) => u.dir), ...rust.workspaces.map((w) => w.cargoDir), "ci", "scripts", "vendor", "Cargo.toml", "Cargo.lock", ".cargo", "rust-toolchain.toml", "rust-toolchain",
        "deny.toml", "clippy.toml", "rustfmt.toml", ...npmRootFiles, ...npmRoots.filter((r) => r !== ".").flatMap((r) => npmRootFiles.map((f) => `${r}/${f}`))])];
      const dirty = git("status", "--porcelain", "--untracked-files=all", "--", ...inputs);
      const dirtyFiles = failed(dirty) ? [`git status failed: ${why(dirty)}`] : text(dirty).split(/\r?\n/).filter(Boolean).filter((l) => !l.includes("node_modules"));
      if (dirtyFiles.length) {
        o.err(`${tag} BLOCKED: the checkout differs from HEAD where compile scans read it; commit or stash first:`);
        for (const l of dirtyFiles.slice(0, 20)) o.err(`  ${l}`);
        return 1;
      }
    }

    const diff = allFilesMode ? null : git("diff", "--name-only", "--diff-filter=ACMR", `${base}...HEAD`);
    if (diff && failed(diff)) { o.err(`${tag} cannot diff against ${base}: ${why(diff)}; pass --base <ref> or --all`); return 2; }
    const changed = allFilesMode ? files : [...new Set(text(diff).split(/\r?\n/).map((f) => f.trim()).filter(Boolean))].filter((f) => o.exists(path.join(repo, f)));
    if (allFilesMode) base = snapshot ?? "the working tree";

    let crateFeatures = {}; let packageFeatures = {};
    try { const gateConfig = JSON.parse(o.readFile(path.join(repo, "ci/gate.config.json"))); crateFeatures = gateConfig.rust ?? {}; packageFeatures = gateConfig.npm ?? {}; } catch { /* no config: default feature sets */ }
    // One lock and one lock scope per Cargo workspace. A workspace whose lock-only change reaches a subset puts only that subset's
    // units (by unit name) in scope; every other workspace keeps all of its units in scope (planScans reads the scope only for the
    // workspaces whose own root files changed).
    const rustLocks = {}; const scopeNames = new Set(); let restricted = false;
    for (const ws of rust.workspaces) {
      const lockPath = ws.cargoDir === "." ? "Cargo.lock" : `${ws.cargoDir}/Cargo.lock`;
      try { rustLocks[ws.cargoDir] = parseLock(o.readFile(path.join(repo, lockPath))); } catch { if (ws.units.length) o.out(`${tag} note: no ${lockPath}; reverse dependents are not checked`); }
      const nameOf = new Map(ws.units.map((u) => [u.crate, u.name]));
      const scope = ws.units.length ? lockScope({
        changed, cargoDir: ws.cargoDir, unitNames: new Set(nameOf.keys()),
        showBase: (f) => { const r = git("show", `${base}:${f}`); return failed(r) ? null : text(r); },
        readLock: () => o.readFile(path.join(repo, lockPath)),
        readManifest: () => o.readFile(path.join(repo, ws.cargoDir === "." ? "Cargo.toml" : `${ws.cargoDir}/Cargo.toml`)),
      }) : null;
      if (scope === null) for (const u of ws.units) scopeNames.add(u.name);
      else { restricted = true; for (const crate of scope) if (nameOf.has(crate)) scopeNames.add(nameOf.get(crate)); }
    }
    const rustScope = restricted ? scopeNames : null;

    const fullPlan = planScans({
      changed, contentRoot: repo, liveRoot: root, includeTests: true, rustScope, cargoCommand,
      // The quiet tests (port, pipe, desktop and wall-clock bound) are proven by the CI shards, one crate per runner, unless
      // RK_GATE_QUIET_TESTS=local asks for the serial quiet lane here (no CI, or a CI outage).
      quietTests: o.env.RK_GATE_QUIET_TESTS === "local" ? "local" : "ci", packageFeatures,
      rustUnits: rust.units, npmUnits: npm, npmRoots, cargoDir: ".", rustLocks, crateFeatures,
      // The broker (or, on a direct host, the gate itself) admits compiles; no separate grant file decides it here.
      localCompile: true,
      exists: o.exists, readFile: o.readFile, platform: o.platform,
    });
    let plan = args.includes("--pre-push") ? prePushPlan(fullPlan) : fullPlan;
    const jobs = Math.max(1, Number(o.env.RK_GATE_JOBS ?? 4) || 4);
    const testJobs = Math.max(1, Number(o.env.RK_GATE_TEST_JOBS ?? jobs) || jobs);

    if (planOnly) {
      o.out(`${tag} plan: ${plan.scans.length} scan(s) for ${changed.length} changed file(s) vs ${base.slice(0, 12)} (reads the working tree; nothing runs)`);
      const wsList = rust.workspaces.map((w) => w.cargoDir);
      const wsNote = wsList.length === 1 ? (wsList[0] === "." ? "" : ` (workspace ${wsList[0]})`) : wsList.length > 1 ? ` (workspaces ${wsList.join(", ")})` : "";
      o.out(`${tag} rust units: ${rust.units.map((u) => `${u.name}=${u.dir}`).join(", ") || "none"}${wsNote}; npm units: ${npm.map((u) => `${u.name}=${u.dir}`).join(", ") || "none"}`);
      for (const s of plan.scans) o.out(`  [${s.class}] ${s.id} :: ${s.command}${plan.deferred.includes(s.id) ? " [deferred: no local compile grant]" : ""}`);
      for (const n of plan.notes) o.out(`${tag} note: ${n}`);
      return 0;
    }

    for (const n of plan.notes) o.out(`${tag} note: ${n}`);
    o.out(`${tag} executor: ${executor.kind}${executor.kind === "broker" ? ` (${cargoCommand.join(" ")})` : " (no broker on this host: cargo runs directly, through the same gates)"}`);
    const quietCount = plan.scans.filter((s) => s.quiet === true).length;
    o.out(`${tag} ${plan.scans.length} scan(s) for ${changed.length} changed file(s) vs ${base.slice(0, 12)}; compile scans ${jobs} at a time, tests ${testJobs} at a time${quietCount ? `, ${quietCount} quiet test scan(s) last under the broker's quiet lease` : ""}`);
    let signalCode = 0;
    for (const sig of ["SIGINT", "SIGTERM"]) {
      const handler = () => {
        if (ac.signal.aborted) process.exit(128 + (sig === "SIGINT" ? 2 : 15)); // a second signal: leave now
        signalCode = 128 + (sig === "SIGINT" ? 2 : 15);
        o.err(`${tag} ${sig}: stopping; running scans are being killed`);
        ac.abort();
      };
      process.on(sig, handler); signals.push([sig, handler]);
    }
    // The result cache keys on the gated commit's object ids; the pre-push check above made HEAD the pushed commit.
    let cache = null;
    if (!classAOnly && !args.includes("--no-cache") && o.env.RK_GATE_CACHE !== "0") {
      try {
        const headSha = text(git("rev-parse", "HEAD")).trim();
        const treeId = text(git("rev-parse", `${headSha}^{tree}`)).trim();
        const common = git("rev-parse", "--git-common-dir");
        if (headSha && treeId && !failed(common) && text(common).trim()) {
          const objectId = (p) => {
            const r = git("ls-tree", headSha, "--", p);
            if (failed(r)) throw new Error(`git ls-tree failed: ${why(r)}`);
            const line = text(r).split(/\r?\n/).find(Boolean);
            return line ? (line.split("\t")[0].split(/\s+/)[2] ?? null) : null;
          };
          const store = fileScanCache(path.join(path.resolve(root, text(common).trim()), "rightkit-gate-cache"));
          const listFiles = (dir) => {
            const r = git("ls-tree", "-r", "--name-only", "-z", headSha, "--", dir);
            if (failed(r)) throw new Error(`git ls-tree failed: ${why(r)}`);
            return text(r).split("\0").filter(Boolean).map(posix);
          };
          const readBlob = (p) => {
            const r = git("show", `${headSha}:${p}`);
            if (failed(r)) throw new Error(`git show failed: ${why(r)}`);
            return text(r);
          };
          cache = { ...store, digest: buildCacheDigest({ objectId, headTree: treeId, rustUnits: rust.units, npmUnits: npm, cargoDir: ".", rustLocks, crateFeatures, listFiles, readBlob }) };
        }
      } catch { cache = null; }
    }
    const scanResult = await (o.runScans ?? runScans)(plan, {
      classes: classAOnly ? ["A"] : ["A", "B"], jobs, testJobs, signal: ac.signal, ...(cache ? { cache } : {}),
      quietLane: quietLeaseLane(o, { tag, reason: `rightkit-gate-quiet-tests@${text(git("rev-parse", "--short", "HEAD")).trim()}` }),
      onResult: (row, p) => o.out(`${tag} [${p.done}/${p.total}] ${(row.cached ? "cached" : row.status).padEnd(7)} ${row.id} (${p.seconds}s)${row.status === "fail" ? ` :: ${String(row.detail ?? "").slice(0, 300)}` : ""}`),
    });
    if (ac.signal.aborted) {
      const notRun = scanResult.scans.filter((r) => r.status !== "pass" && r.status !== "fail").map((r) => r.id);
      o.err(`${tag} CANCELLED (${signalCode || "aborted"}); ${notRun.length} scan(s) not run:`);
      for (const id of notRun) o.err(`  - ${id}`);
      for (const line of scanFailures(scanResult).filter((l) => !/\(not run\)/.test(l))) o.err(`  - ${line}`);
      return signalCode || 130;
    }
    const failures = scanFailures(scanResult);
    const deferrals = scanDeferrals(scanResult);
    for (const line of deferrals) failures.push(`DEFERRED ${line}`);
    for (const note of scanResult.notes) o.out(`${tag} ${note}`);
    if (failures.length) {
      o.err(`${tag} BLOCKED (${failures.length})`);
      for (const f of failures) o.err(`  - ${f}`);
      o.err(classAOnly ? "Class A is red: nothing compiles until it is green." : "Fix and push again. Never bypass with --no-verify.");
      return 1;
    }
    if (classAOnly) {
      o.out(`${tag} class A pass: ${scanResult.scans.length} deterministic scan(s) over ${changed.length} file(s) of ${base}`);
      return 0;
    }
    o.out(`${tag} pass (${changed.length} changed file(s) vs ${base}); ${deferrals.length} scan(s) deferred, not passed${scanResult.cacheHits.length ? `; ${scanResult.cacheHits.length} reused from the result cache` : ""}`);
    return 0;
  } finally {
    for (const [sig, handler] of signals) process.off(sig, handler);
    cleanup();
  }
}

const invokedDirectly = (() => {
  try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; }
})();
if (invokedDirectly) {
  main(process.argv.slice(2)).then((code) => process.exit(code), (e) => { console.error(`rightkit-gate: ${e?.stack ?? e}`); process.exit(2); });
}
