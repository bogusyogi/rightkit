#!/usr/bin/env node
// rightkit-gate: the mandatory local deterministic gate for any repository that uses RightKit (Adrian 2026-10-22:
// "local deterministic checks ... mandatory for all builds"). Runs from the pre-push hook that `rightkit-gate install-hook`
// writes, so it never depends on an agent remembering. Every applicable scan for the pushed change runs through the one
// scan module (../deterministic-scans.mjs); Rust units come from `cargo metadata` (one run per Cargo workspace), npm units from
// pnpm-workspace.yaml, so the layout is the repository's own. CI still compiles, lints and tests everything.
//
// Layout it covers (nothing is partly covered silently):
//   - Rust: every tracked Cargo.toml that is a workspace root or a standalone package not inside another workspace is its own Cargo
//     workspace (`cargo metadata --no-deps --locked` there, through the broker). A unit of a workspace below the repository root is
//     named `<cargoDir>:<crate>`; its scans run with that directory as cwd, its reverse dependents and cache keys come from that
//     workspace's own Cargo.lock, and ci/gate.config.json `rust` may key its feature sets by `<cargoDir>:<crate>` or by the plain crate
//     name. A tracked Cargo.toml that is not a member and cannot stand alone, a member that is untracked or ignored, and an untracked
//     Cargo.toml are skipped with a note on the first lines of the run, never silently.
//   - npm: the pnpm-workspace.yaml globs over tracked package.json files; when that file is absent, has no `packages:` or declares ".",
//     the root package.json is a unit too (tsc, eslint and test rows like any package).
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
//   rightkit-gate install-hook [--force]      write .githooks/pre-push and set core.hooksPath. core.hooksPath hides .git/hooks, so the
//                                             pre-push hook runs the repository's own .git/hooks/pre-push first and every other hook
//                                             present in .git/hooks gets a .githooks/<name> trampoline that runs it; the hook finds
//                                             the gate binary in the root node_modules/.bin, then any workspace package's, then pnpm exec
//
// From the pre-push hook, git's ref lines arrive on stdin: the commit being pushed must be the checked-out HEAD (broker scans
// read the live checkout), and the remote's current commit is the diff base. To push an earlier commit of the local stack,
// check it out first (a detached HEAD is fine), push HEAD:<branch>, return.
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
import { WORKSPACE_RUST_FILE, ancestorConfigChanged, ancestorDirsOf, canonicalJson, defaultSpawn, digestScanSet, fileScanCache, metadataArgs, planScans, runScans, scanDeferrals, scanFailures } from "../deterministic-scans.mjs";
import { changedLockNames, parseLock, reach } from "../lock-graph.mjs";

const ZERO = /^0+$/;
const GRANT_FILE = ".rightkit-local-build.json";
const SAFE_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const posix = (p) => String(p).replaceAll("\\", "/");
const text = (r) => String(r?.stdout ?? "");
const failed = (r) => !r || Boolean(r.error) || r.status !== 0;
const why = (r) => String(r?.error?.message ?? r?.stderr ?? "").trim().split(/\r?\n/).slice(-3).join(" | ");

const USAGE = `usage: rightkit-gate [--base <ref>] [--plan] [--no-cache] [--pre-push] | rightkit-gate install-hook [--force] | --version`;

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

/**
 * npm units: pnpm-workspace.yaml globs over tracked package.json files, else the root package.json. The root package.json is a
 * unit as well when the workspace file is absent, has no `packages:` (or an empty one) or declares "." (or "./"); with a
 * `packages:` list that does not name the root it is only the workspace's own manifest and is not a unit.
 */
export function discoverNpmUnits({ root, tracked, readFile, exists }) {
  let dirs;
  const workspaceFile = path.join(root, "pnpm-workspace.yaml");
  if (exists(workspaceFile)) {
    const globs = parseWorkspaceGlobs(readFile(workspaceFile));
    const inc = globs.filter((g) => !g.startsWith("!")).map(globToRegExp);
    const exc = globs.filter((g) => g.startsWith("!")).map((g) => globToRegExp(g.slice(1)));
    const positive = globs.filter((g) => !g.startsWith("!"));
    const rootDeclared = positive.length === 0 || positive.some((g) => g === "." || g === "./");
    dirs = tracked("package.json").map((p) => path.posix.dirname(p)).filter((d) => (d === "." ? rootDeclared : inc.some((r) => r.test(d))) && !exc.some((r) => r.test(d)));
  } else dirs = exists(path.join(root, "package.json")) ? ["."] : [];
  dirs = [...new Set(dirs)].sort();
  const rows = dirs.map((dir) => {
    let manifest;
    try { manifest = JSON.parse(readFile(path.join(root, dir, "package.json")).replace(/^﻿/, "")); } catch (e) { throw new Error(`${dir}/package.json: ${e.message}`); }
    return { dir, name: unitName(manifest.name, dir) };
  });
  const count = new Map();
  for (const r of rows) count.set(r.name, (count.get(r.name) ?? 0) + 1);
  return rows.map((r) => ({ name: count.get(r.name) > 1 ? unitName("", r.dir === "." ? "root" : r.dir) : r.name, dir: r.dir }));
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

/** The repository-relative directories a workspace manifest `exclude`s (`[workspace] exclude = [...]`); [] when none or unreadable. */
export function workspaceExcludes(readFile, root, cargoDir) {
  let text;
  try { text = readFile(path.join(root, cargoDir === "." ? "Cargo.toml" : `${cargoDir}/Cargo.toml`)); } catch { return []; }
  const body = manifestTables(text).get("workspace") ?? "";
  const list = /^\s*exclude\s*=\s*\[([\s\S]*?)\]/m.exec(body)?.[1] ?? "";
  return [...list.matchAll(/"([^"]+)"/g)].map((m) => posix(m[1]).replace(/\/+$/, "")).map((d) => (cargoDir === "." ? d : `${cargoDir}/${d}`));
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
 */
export function discoverRustWorkspaces({ root, spawn, cargoCommand, tracked, untracked = () => [], realpath = fs.realpathSync, fixtures = [], readFile = (f) => fs.readFileSync(f, "utf8") }) {
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
    try { found = workspaceMetadata({ root, spawn, cargoCommand, dir, realpath }); } catch (e) {
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
 * push) and the gate. The gate binary is the root node_modules/.bin/rightkit-gate, else any workspace package's (up to three
 * levels down: tauri-app-next, apps/*, packages/*), else `pnpm exec rightkit-gate`, else node with a package-relative path when
 * pnpm is absent; the line it chose is printed. No absolute path is written: everything is resolved from git at run time.
 */
export function hookText(nodePath) {
  return `#!/bin/sh
# Mandatory local gate before every push (rightkit-gate install-hook). Never bypass with --no-verify.
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
for bin in node_modules/.bin/rightkit-gate */node_modules/.bin/rightkit-gate */*/node_modules/.bin/rightkit-gate */*/*/node_modules/.bin/rightkit-gate; do
  if [ -x "$bin" ]; then
    echo "rightkit-gate: using $bin" >&2
    ("$bin" --base origin/main) < "$stdin"; exit $?
  fi
done
if command -v pnpm >/dev/null 2>&1; then
  echo "rightkit-gate: using pnpm exec rightkit-gate" >&2
  (exec pnpm exec rightkit-gate --base origin/main) < "$stdin"; exit $?
fi
echo "rightkit-gate: using node ${nodePath}" >&2
(exec node "${nodePath}" --base origin/main) < "$stdin"; exit $?
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

export function externalInputs({ dir, files, readBlob, exists }) {
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
    for (const m of text.matchAll(PARENT_PATH)) {
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
    if (!build && !external && leavesCrate === null && TEST_LEAVES_CRATE.test(text)) leavesCrate = file;
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
 * - An npm package scan (test:npm, tsc, eslint) keys on the package's workspace closure (its `workspace:` dependencies,
 *   transitively, by package name) and the root npm inputs; test:scripts-ci on scripts/, ci/, .github/, packages/release and the
 *   root install graph. Every other class B scan (generated-docs, swift-build) uses the whole tree.
 * - Class A: cargo-deny, cargo-audit, pnpm-audit, pnpm-lockfile and advisory-exceptions are never cached (live answers); rustfmt,
 *   node-check, actionlint and the parse scans key on their files' blob ids; cargo-package-list on the crate, the manifest projection
 *   and its lock entry; the rest on the whole tree.
 */
// Root npm inputs every package scan reads: the install graph and the shared tool configuration.
const SHARED_NPM_INPUTS = ["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc", "tsconfig.json", "tsconfig.base.json", "eslint.config.js", "eslint.config.mjs", ".eslintrc.json", "biome.json", "vitest.config.ts", "vitest.config.mjs", "vitest.workspace.ts"];
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
    if (npmByPkgName === null) { npmByPkgName = new Map(); for (const u of npmUnits) { const m = npmManifest(u); if (m?.name) npmByPkgName.set(m.name, u); } }
    const seen = new Map([[unit.name, unit]]); const stack = [unit]; let unresolved = false;
    while (stack.length) {
      const u = stack.pop(); const m = npmManifest(u);
      if (!m) { unresolved = true; continue; }
      for (const section of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) {
        for (const [name, spec] of Object.entries(m[section] ?? {})) {
          if (!/^(workspace:|link:|file:)/.test(String(spec))) continue;
          const dep = npmByPkgName.get(name);
          if (!dep) { unresolved = true; continue; }
          if (!seen.has(dep.name)) { seen.set(dep.name, dep); stack.push(dep); }
        }
      }
    }
    return { members: [...seen.values()].sort((a, b) => (a.name < b.name ? -1 : 1)), unresolved };
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
        const npmMatch = /^(test:npm|tsc|eslint):(.+)$/.exec(scan.id);
        if (npmMatch && npmByUnit.has(npmMatch[2])) {
          const { members, unresolved } = npmClosure(npmByUnit.get(npmMatch[2]));
          if (unresolved) return whole();
          return canonicalJson({ kind: "npm", id: scan.id, members: members.map((m) => [m.name, m.dir, idOf(m.dir)]), shared: SHARED_NPM_INPUTS.map((p) => [p, idOf(p)]) });
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

// ---- command resolution -------------------------------------------------------------------------------------

// Cargo only through the broker: `rightkit cargo`, or a `cargo` on PATH that is the managed shim (it runs cargo-guard.mjs).
function resolveCargoCommand(o) {
  const which = (cmd) => {
    const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]);
    return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  };
  if (which("rightkit").length) return ["rightkit", "cargo"];
  const first = which("cargo")[0];
  if (first) { try { if (o.readFile(first).includes("cargo-guard.mjs")) return ["cargo"]; } catch { /* unreadable: not the shim */ } }
  return null;
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

function installHook(o, root, force) {
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
  const own = present.filter(chainableHook).sort();
  const skipped = present.filter((name) => !name.endsWith(".sample") && !chainableHook(name)).sort();
  if (skipped.length) o.err(`rightkit-gate: WARNING: not chaining .git/hooks/${skipped.join(", .git/hooks/")}: not a plain hook file name (letters, digits, - and _ only); run it from a hook of its own.`);
  const trampolines = own.filter((name) => name !== "pre-push").map((name) => ({ name, file: path.join(hooksDir, name), wanted: trampolineText(name) }));
  for (const t of trampolines) {
    t.exists = o.exists(t.file);
    t.current = t.exists ? o.readFile(t.file).replace(/\r\n/g, "\n") === t.wanted : false;
    if (t.exists && !t.current && !force) {
      o.err(`rightkit-gate: .githooks/${t.name} already exists and differs; nothing changed. Review it, or run \`rightkit-gate install-hook --force\` to replace it with a trampoline to .git/hooks/${t.name}.`);
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
    else put(t.file, t.wanted, `${t.exists ? "replaced" : "wrote"} .githooks/${t.name}`, ` (runs .git/hooks/${t.name})`);
  }
  if (existingPath === ".githooks") done.push("core.hooksPath already .githooks");
  else {
    const set = git("config", "core.hooksPath", ".githooks");
    if (failed(set)) { o.err(`rightkit-gate: git config core.hooksPath failed: ${why(set)}`); return 2; }
    done.push(`set core.hooksPath to .githooks${existingPath ? ` (was ${existingPath})` : ""}`);
  }
  for (const line of done) o.out(`rightkit-gate: ${line}`);
  const chained = own.length ? own.map((name) => (name === "pre-push" ? "pre-push (run first, then the gate)" : name)) : [];
  o.out(`rightkit-gate: chained hooks from .git/hooks: ${chained.length ? chained.join(", ") : "none"}${own.includes("pre-push") ? "" : "; the pre-push hook still runs .git/hooks/pre-push first if one appears later"}`);
  const files = [".githooks/pre-push", ...trampolines.map((t) => `.githooks/${t.name}`)];
  o.out(`rightkit-gate: commit the hooks with executable mode (git add --chmod=+x ${files.join(" ")}); the pre-push hook finds rightkit-gate in node_modules/.bin of the repository root, else of any workspace package, else through \`pnpm exec\`.`);
  const provider = releaseProvider(o, root, git);
  if (!provider) o.err("rightkit-gate: WARNING: no package.json in this repository depends on @rightkit/release, so the pre-push hook cannot find the rightkit-gate binary; add @rightkit/release as a devDependency of the root or of a workspace package and install.");
  else o.out(`rightkit-gate: @rightkit/release is declared in ${provider}`);
  return 0;
}

// ---- main ---------------------------------------------------------------------------------------------------

/**
 * Run the CLI. Returns the exit code (0 pass, 1 blocked or failed, 2 cannot run, 128+n cancelled). `io` overrides the
 * process, fs and output bindings so tests execute nothing real: spawn, exists, readFile, writeFile, mkdir, chmod,
 * readStdin, out, err, cwd, env, platform, abort (an AbortController).
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
  if (sub && sub !== "install-hook") { o.err(`rightkit-gate: unknown command ${JSON.stringify(sub)}\n${USAGE}`); return 2; }
  if (args.includes("--base") && (!option("--base") || option("--base").startsWith("--"))) { o.err("rightkit-gate: --base needs a ref"); return 2; }

  const top = o.spawn("git", ["rev-parse", "--show-toplevel"], { cwd: o.cwd });
  if (failed(top) || !text(top).trim()) { o.err("rightkit-gate: not inside a git repository"); return 2; }
  const root = path.resolve(text(top).trim());
  const git = (...a) => o.spawn("git", a, { cwd: root });
  if (sub === "install-hook") return installHook(o, root, args.includes("--force"));

  const planOnly = args.includes("--plan");
  let base = option("--base") ?? "origin/main";
  const tag = "rightkit-gate:";

  // git pre-push: "<local ref> <local sha> <remote ref> <remote sha>" per pushed ref. A plan reads no stdin.
  if (!planOnly) {
    const head = text(git("rev-parse", "HEAD")).trim();
    const lines = o.readStdin().split(/\r?\n/).map((l) => l.trim().split(/\s+/)).filter((f) => f.length === 4);
    for (const [, localSha, remoteRef, remoteSha] of lines) {
      if (ZERO.test(localSha)) continue; // a delete pushes no content
      if (localSha !== head) {
        o.err(`${tag} BLOCKED: pushing ${localSha.slice(0, 10)} to ${remoteRef}, but HEAD is ${head.slice(0, 10)}; broker scans read the checkout, so check the pushed commit out first (git checkout --detach ${localSha.slice(0, 10)}), push HEAD:<branch>, then return`);
        return 1;
      }
      if (!ZERO.test(remoteSha) && !failed(git("cat-file", "-e", `${remoteSha}^{commit}`))) base = remoteSha;
    }
  }

  const tracked = (name) => {
    const r = git("ls-files", "-z", "--", `:(glob)**/${name}`);
    if (failed(r)) throw new Error(`git ls-files failed: ${why(r)}`);
    return text(r).split("\0").filter(Boolean).map(posix);
  };
  const readLive = (f) => o.readFile(f);

  // Units come from the live checkout (cargo reads it); a dirty checkout then blocks below.
  let rust; let npm; let cargoCommand = ["rightkit", "cargo"];
  try {
    const manifests = tracked("Cargo.toml");
    if (manifests.length) {
      cargoCommand = resolveCargoCommand(o);
      if (!cargoCommand) { o.err(`${tag} BLOCKED: neither \`rightkit\` nor the managed cargo shim (cargo-guard.mjs) is first on PATH; plain cargo is never used`); return 1; }
      if (!o.exists(path.join(root, GRANT_FILE))) { o.err(`${tag} BLOCKED: no local compile grant (${GRANT_FILE}); the broker refuses cargo, so the Rust units cannot be discovered or scanned`); return 1; }
    }
    // Untracked (not ignored) manifests only: git does not descend into ignored directories, so this stays cheap. A member of a
    // workspace that is untracked or ignored is dropped by discoverRustWorkspaces itself.
    const untracked = () => {
      const r = git("ls-files", "-z", "--others", "--exclude-standard", "--", ":(glob)**/Cargo.toml");
      return failed(r) ? [] : text(r).split("\0").filter(Boolean).map(posix);
    };
    // ci/gate.config.json `fixtures` (optional): directories whose manifests are consumer or sample crates, gated through the
    // consumer rows of the crates they exercise (CI treats them the same: scripts/ci/gate.mjs consumerRowInvocations), never as units.
    let fixtures = [];
    try { const f = JSON.parse(o.readFile(path.join(root, GATE_CONFIG))).fixtures; if (Array.isArray(f)) fixtures = f.filter((d) => typeof d === "string"); } catch { /* no config: no fixtures */ }
    rust = discoverRustWorkspaces({ root, spawn: (c, a, opts) => o.spawn(c, a, opts), cargoCommand, tracked, untracked, fixtures, readFile: readLive });
    npm = discoverNpmUnits({ root, tracked, readFile: readLive, exists: o.exists });
  } catch (e) { o.err(`${tag} BLOCKED: cannot discover the repository's units: ${e.message}`); return 1; }
  for (const k of rust.skipped) o.out(`${tag} note: ${k.manifest} is skipped, not scanned: ${k.why}`);

  // Broker scans (cargo, pnpm) read the live checkout, so it must equal HEAD where they read it: an uncommitted fix could
  // otherwise hide a committed failure. Any change or untracked file under those paths blocks the gate.
  if (!planOnly) {
    const inputs = [...new Set([...rust.units.map((u) => u.dir), ...npm.map((u) => u.dir), ...rust.workspaces.map((w) => w.cargoDir), "ci", "scripts", "vendor", "Cargo.toml", "Cargo.lock", ".cargo", "rust-toolchain.toml", "rust-toolchain",
      "deny.toml", "clippy.toml", "rustfmt.toml", "package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc", "eslint.config.js", "eslint.config.mjs", ".eslintrc.json", "biome.json"])];
    const dirty = git("status", "--porcelain", "--untracked-files=all", "--", ...inputs);
    const dirtyFiles = failed(dirty) ? [`git status failed: ${why(dirty)}`]
      : text(dirty).split(/\r?\n/).filter(Boolean).filter((l) => !l.includes("node_modules") && !l.includes(GRANT_FILE));
    if (dirtyFiles.length) {
      o.err(`${tag} BLOCKED: the checkout differs from HEAD where broker scans read it; commit or stash first:`);
      for (const l of dirtyFiles.slice(0, 20)) o.err(`  ${l}`);
      return 1;
    }
  }

  const ac = o.abort ?? new AbortController();
  let exportRoot = null;
  const cleanup = () => { if (exportRoot) fs.rmSync(exportRoot, { recursive: true, force: true }); };
  const signals = [];
  try {
    let repo = root;
    if (!planOnly) {
      exportRoot = fs.mkdtempSync(path.join(o.env.RK_GATE_TMPDIR || os.tmpdir(), "rk-gate-"));
      repo = path.join(exportRoot, "repo");
      const made = o.spawn("git", ["archive", "--format=tar", "--prefix=repo/", "-o", path.join(exportRoot, "head.tar"), "HEAD"], { cwd: root });
      const x = failed(made) ? made : o.spawn("tar", ["-xf", "head.tar"], { cwd: exportRoot });
      if (failed(made) || failed(x)) { o.err(`${tag} cannot export HEAD: ${why(made) || why(x)}`); return 2; }
    }
    const diff = git("diff", "--name-only", "--diff-filter=ACMR", `${base}...HEAD`);
    if (failed(diff)) { o.err(`${tag} cannot diff against ${base}: ${why(diff)}; pass --base <ref>`); return 2; }
    const changed = [...new Set(text(diff).split(/\r?\n/).map((f) => f.trim()).filter(Boolean))].filter((f) => o.exists(path.join(repo, f)));

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
      rustUnits: rust.units, npmUnits: npm, cargoDir: ".", rustLocks, crateFeatures,
      localCompile: o.exists(path.join(root, GRANT_FILE)),
      exists: o.exists, readFile: o.readFile, platform: o.platform,
    });
    const plan = args.includes("--pre-push") ? prePushPlan(fullPlan) : fullPlan;
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
    if (!args.includes("--no-cache") && o.env.RK_GATE_CACHE !== "0") {
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
    const scanResult = await runScans(plan, {
      classes: ["A", "B"], jobs, testJobs, signal: ac.signal, ...(cache ? { cache } : {}),
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
      o.err("Fix and push again. Never bypass with --no-verify.");
      return 1;
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
