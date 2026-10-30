#!/usr/bin/env node
// rightkit-gate: the local deterministic gate for any repository that uses RightKit. The gate is for building: commit and push
// run no gate and no local hook check (docs/GATE-CONTRACT.md), and `rightkit-gate install-hook` writes no git hook. Every
// applicable scan for the gated change runs through the one scan module (../deterministic-scans.mjs); Rust units come from `cargo metadata` (one run per Cargo workspace), npm units from
// the pnpm roots' workspace files, so the layout is the repository's own. CI still compiles, lints and tests everything.
//
// Layout it covers (nothing is partly covered silently):
//   - Rust: every tracked Cargo.toml that is a workspace root or a standalone package not inside another workspace is its own Cargo
//     workspace (`cargo metadata --no-deps --locked` there, through the broker). A unit of a workspace below the repository root is
//     named `<cargoDir>:<crate>`; its scans run with that directory as cwd, its reverse dependents and cache keys come from that
//     workspace's own Cargo.lock, and ci/gate.config.json `rust` may key its feature sets by `<cargoDir>:<crate>` or by the plain crate
//     name. One workspace and lock per crate (RightKit itself, rightkit#1285): a crate whose workspace is its own crates/<crate> directory keeps
//     its plain name and unsuffixed scan ids; a selected crate also selects every crate that path-depends on it, across workspaces, read from
//     the manifests (packages/release/crate-workspaces.mjs); a vendored source a crate patches in selects that crate. A tracked Cargo.toml that is not a member and cannot stand alone, a member that is untracked or ignored, and an untracked
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
//   rightkit-gate --to check|compile|package|qualify|dev|test --route [--platform p] [--commit <rev>]
//                                             prints one JSON line (schema rightkit-gate.route/1) naming the route this host takes for the
//                                             stage and the refusal it would meet, and runs nothing: no ownership, snapshot, check, run record
//                                             or receipt (rightkit#1297; the release canary probes it for every app). Exit 0 answered, 2 not
//   rightkit-gate check [--repo <dir>] [--commit <rev> | --worktree] [--rule-tier compile|release] [--only <rule,...>] [--list] [--print-baseline] [--baseline <file>]
//                                             the deterministic stage: the app check (../app-check.mjs), then every class A scan
//                                             (lockfiles, cargo-deny/audit, secrets, licences, manifests, workflow lint, syntax) over the
//                                             whole tree of the commit, not a diff. Nothing compiles; without a broker class A uses
//                                             plain cargo for metadata/deny/audit only. The RustRight broker runs this before it admits
//                                             any compile. --list/--print-baseline/--only show the app check alone. Exit 0 pass,
//                                             1 findings, 2 could not run
//   rightkit-gate install-hook                retire the git hooks an earlier release wrote and add the agent hook: it removes
//                                             .githooks/pre-push, the .githooks/<name> forwarders and the advisory ownership pre-commit
//                                             hook, each only when its own header or marker says RightKit wrote it (a foreign hook is never
//                                             touched; pre-commit.rightkit-foreign goes back to pre-commit), unsets a local core.hooksPath
//                                             of `.githooks` once nothing foreign remains there, and registers the one-door agent hook in
//                                             .claude/settings.json. It writes no pre-push, pre-commit or other git hook
//                                             (`right-release hooks install` is an alias of this command; --force is ignored)
//
// Git ref lines on stdin (the form git hands a pre-push hook; nothing here installs one) are still read when given: the commit
// pushed must be the checked-out HEAD (broker scans read the live checkout), and the remote's current commit is the diff base.
// The executor is chosen by the gate, never the caller (gate-executor.mjs resolveExecutor): the RustRight broker where this machine
// has it (`rustright` on PATH, or its managed cargo shim), a refusal on a broker-managed machine whose broker is unreachable, and
// direct cargo on any other host (a cloud container, a CI runner). Every host runs the same gates in the same order (app rules, class A,
// the compile scans); an unmanaged host is never told to find a broker.
// ci/gate.config.json `rust` (optional) sets per-crate feature sets, exactly as CI's gate reads it; `npm.<unit>` sets `quietTests` and
// `requiresBroker: { "reason": "..." }` (a test the package scripts do not show reaching the broker: node -e, node scripts, sh -c, pnpm --dir).
// Result cache: a passing class B scan is reused only when its scan id, command, tool version, environment and input digest
// (the gated commit's git object ids for the crate's workspace closure, the shared build inputs, ci/gate.config.json and every
// path the closure's sources include from outside their crate; the whole HEAD tree for Rust tests unless ci/gate.config.json
// rust.<crate>.testInputs lists the paths they read, for npm, node and docs scans, and whenever a reference cannot be resolved
// statically) are all identical. Stored in <git common dir>/rightkit-gate-cache; a hit prints
// `cached` and is listed in the result's cacheHits. Failures, deferrals and class A scans are never cached.
// RK_GATE_JOBS (default 4) sets class A scan concurrency. RK_GATE_CLASS_B_JOBS (default 3, the Mac broker's build capacity; the
// broker queues beyond its own) sets how many components' class B scans run at once; one component's scans stay in order (rightkit#1285).
// A crate whose ci/gate.config.json entry says `quietTests: true` has its test scans run last,
// one at a time, under the RustRight quiet lease (`rustright quiet acquire`, RUSTRIGHT_QUIET_LEASE in their environment).
// SIGINT/SIGTERM stop scheduling, kill every running scan's process tree and exit non-zero listing the scans not run.
import fs from "node:fs";
import { createHash, randomUUID } from "node:crypto";
import { resolveManagedRoots } from "../managed-roots.mjs";
import os from "node:os";
import path from "node:path";
import { FOREIGN_PRE_COMMIT, retireHooksIn } from "../hook-retirement.mjs";
import { fileURLToPath } from "node:url";
import { BIOME_CONFIG_FILES, ESLINT_CONFIG_FILES, WORKSPACE_RUST_FILE, ancestorConfigChanged, ancestorDirsOf, canonicalJson, defaultSpawn, defaultSpawnAsync, digestScanSet, fileScanCache, findRustfmt, metadataArgs, planScans, runScans, scanDeferrals, scanFailures } from "../deterministic-scans.mjs";
import { changedLockNames, parseLock, reach } from "../lock-graph.mjs";
import { resolveExecutor } from "../gate-executor.mjs";
import { directPlatform } from "../managed-cargo.mjs";
import { bindingOf, endRun, readReceipt, RECEIPT_SCHEMA, receiptKey, sameBinding, sameSource, sealedFor, stagesTo, startRun, STAGES, STAGE_SOURCES, writeReceipt } from "../gate-stages.mjs";
import { spawnSync as spawnInherit } from "../background-process.mjs";
import { readDiskPolicy, scratchRoots } from "../disk-policy.mjs";
import { buildDirectReceipt, directVerdict, resolveTargetDirectory, verifyDirectReceipt, writeDirectDiagnostics } from "../direct-receipt.mjs";
import { createFreshnessCensus } from "../freshness-census.mjs";

const ZERO = /^0+$/;
const SAFE_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const posix = (p) => String(p).replaceAll("\\", "/");
const text = (r) => String(r?.stdout ?? "");
const failed = (r) => !r || Boolean(r.error) || r.status !== 0;
const why = (r) => String(r?.error?.message ?? r?.stderr ?? "").trim().split(/\r?\n/).slice(-3).join(" | ");

// A green `check --only` view: app rules passed but no class A scan ran. Distinct from 0 so no caller can admit it as a full check.
export const PARTIAL_EXIT = 3;
/** The platforms a door run can package: desktop (signed, sealed, installed QA, publisher upload) and mobile (signed, sealed, store test track). */
export const PLATFORMS = Object.freeze(["mac", "win", "ios", "android"]);
export const MOBILE_PLATFORMS = Object.freeze(new Set(["ios", "android"]));
const USAGE = `usage: rightkit-gate --to check|compile|package|qualify|release [--platform mac|win|ios|android] [--tier patch|update] [--receipt-in <file>] [--receipt-out <file>] | rightkit-gate --to check|compile|package|qualify|release --unit npm:@rightkit/<name>|cargo:rightkit-<name> [--receipt-out <file>] | rightkit-gate --to dev|test [--commit <rev>] [-- <right-release args>] | rightkit-gate --to check|compile|package|qualify|dev|test --route [--platform mac|win|ios|android] [--commit <rev>] | rightkit-gate stage --platform mac|win [--tier patch|update] | rightkit-gate [--base <ref>] [--plan] [--no-cache] | rightkit-gate install-hook | rightkit-gate env-digest | rightkit-gate snapshot [--commit <rev>] [--verify [--env <sha256>]] [--json] | rightkit-gate live-scans --repo <root> | rightkit-gate clippy [<cargo clippy args>] | rightkit-gate env-digest [--snapshot <dir> --commit <sha>] | rightkit-gate check [--repo <dir>] [--commit <rev> | --worktree] [--rule-tier compile|release] [--only <rule,...>] [--list] [--print-baseline] [--baseline <file>] | rightkit-gate check --commit <sha> --full-a --snapshot <dir> | --version`;

// ---- pure helpers (exported for tests) ----------------------------------------------------------------------

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
/**
 * Where the snapshot export lives. The RustRight broker refuses a job whose workspace is on the system volume
 * (RK_INTERNAL_DRIVE_REFUSED: agents never write to the internal drive), and os.tmpdir() is on it on every Mac and Dell, so on a
 * host with a disk-policy scratch root (/Volumes/D/scratch, D:\scratch) the export goes there. RK_GATE_TMPDIR overrides; os.tmpdir()
 * is the fallback where no scratch root exists (CI runners, cloud containers), where plain cargo runs and nothing refuses the system
 * volume. (rightkit#1215)
 */
export function exportTmpRoot({ env = process.env, platform = process.platform, exists = fs.existsSync, policy = null } = {}) {
  if (env.RK_GATE_TMPDIR) return env.RK_GATE_TMPDIR;
  try {
    const roots = scratchRoots(policy ?? readDiskPolicy(), { platform, env });
    const found = roots.find((root) => exists(root));
    if (found) return found;
  } catch { /* no readable policy: the system temp dir */ }
  return os.tmpdir();
}

/** The files cargo metadata, deny and audit read, in the repository root and in every workspace that has a manifest. */
export const CARGO_INPUT_NAMES = Object.freeze(["Cargo.toml", "Cargo.lock", "deny.toml", "rust-toolchain.toml", "rust-toolchain", ".cargo"]);

/**
 * The Cargo inputs of the live checkout `root` that are not blob-identical to `snapshot`'s (a commit or tree): a changed or deleted
 * tracked input, or any untracked file (ignored ones included, cargo reads them) under those paths. Empty means the broker may scan
 * the primary checkout for exactly the snapshot's result. `manifests` are the snapshot's tracked Cargo.toml paths (posix).
 */
export function cargoInputsDiffer({ root, snapshot, manifests, spawn = defaultSpawn }) {
  const git = (...a) => spawn("git", a, { cwd: root });
  // Every manifest directory and every ancestor up to the root: cargo reads a .cargo/config.toml or a rust-toolchain file from any of them.
  const dirs = new Set([""]);
  for (const m of manifests) { const parts = path.posix.dirname(m).split("/").filter((x) => x && x !== "."); for (let i = 1; i <= parts.length; i++) dirs.add(parts.slice(0, i).join("/")); }
  const dirList = [...dirs];
  const paths = dirList.flatMap((d) => CARGO_INPUT_NAMES.map((n) => (d ? `${d}/${n}` : n)));
  // Pathspecs go to git in bounded batches: a repository with many crates (RightKit: ~1000 input paths) overflows Windows' 32767-
  // character command line in one argv (ENAMETOOLONG on the Dell, 2026-10-27; G4 was the same for rustfmt). Each batch's results
  // are merged; a failed batch fails the whole listing closed.
  const batches = pathspecBatches(paths);
  const listedAll = batches.map((b) => git("ls-tree", "-r", "-z", snapshot, "--", ...b));
  const badList = listedAll.find((r) => failed(r));
  if (badList) return [`(cannot list ${snapshot}: ${why(badList)})`];
  const snap = new Map();
  for (const listed of listedAll) for (const entry of text(listed).split("\0").filter(Boolean)) { const tab = entry.indexOf("\t"); const [, , id] = entry.slice(0, tab).split(" "); snap.set(entry.slice(tab + 1), id); }
  const differ = new Set();
  for (const [p, id] of snap) {
    const live = path.join(root, ...p.split("/"));
    let liveId = null;
    // Cargo reads the live bytes, so the live bytes are hashed raw (`--no-filters`: no clean filter, eol or text attribute can make
    // two different byte strings look the same). The one accepted difference is an exact CRLF checkout of an LF blob (D13, Dell
    // 2026-10-27: core.autocrlf), which every TOML/toolchain reader treats as the same newline; anything else differs.
    try { if (fs.statSync(live).isFile()) { const h = git("hash-object", "--no-filters", "--", live); liveId = failed(h) ? null : text(h).trim(); } } catch { liveId = null; }
    if (liveId !== id && !isCrlfCheckoutOf({ git, id, live })) differ.add(p);
  }
  // Untracked under the input paths, ignored files included: cargo reads a .cargo/config.toml or a deny.toml whether or not git tracks it.
  for (const batch of batches) {
    const others = git("ls-files", "-z", "--others", "--", ...batch);
    if (failed(others)) differ.add(`(cannot list untracked Cargo inputs: ${why(others)})`); // fail closed: an unlisted input is an unknown input
    else for (const p of text(others).split("\0").filter(Boolean)) if (!snap.has(p)) differ.add(p);
    // A tracked input the snapshot lacks (added in HEAD after the snapshot, say) is live-only too.
    const tracked = git("ls-files", "-z", "--", ...batch);
    if (failed(tracked)) differ.add(`(cannot list tracked Cargo inputs: ${why(tracked)})`);
    else for (const p of text(tracked).split("\0").filter(Boolean)) if (!snap.has(p)) differ.add(p);
  }
  return [...differ].sort();
}

/** True only when the live file's bytes are exactly blob `id` with every LF written as CRLF (an autocrlf checkout); fails closed. */
export function isCrlfCheckoutOf({ git, id, live, readFile = fs.readFileSync }) {
  try {
    const blob = git("cat-file", "blob", id);
    if (failed(blob)) return false;
    const committed = text(blob);
    if (committed.includes("\r")) return false;
    return readFile(live, "utf8") === committed.replace(/\n/g, "\r\n");
  } catch { return false; }
}

/**
 * Tool caches inside node_modules that nothing installs: test-result caches (vitest), the vite optimizer and bundled-config temp
 * files, transpile and other tool caches (.cache). Some hold code a later step executes, so the environment a verdict carries binds
 * their bytes as they stand when class A ends, and the broker's START recomputes it: a cache edited, added or removed after the
 * verdict (by a dev server, another tool, or by hand) voids the ticket and class A runs again. Nothing is deleted. Inside one run
 * the start/end comparison leaves them out, because the run itself writes them. pnpm's task-run state holds only completion
 * markers pnpm reads, never code, and is not bound.
 */
/** Longest the class A environment waits for `pnpm --version`. */
export const PNPM_PROBE_TIMEOUT_MS = 60_000;
export const TOOL_CACHE_DIRS = new Set([".vite", ".vite-temp", ".vitest-cache", ".cache"]);
export const INSTALL_STATE_NOT_COVERED = new Set([".pnpm-task-run-state-v1"]);
const nodeModulesDirs = (root, files) => [...new Set(files.filter((f) => /(^|\/)package\.json$/.test(f)).map((f) => path.join(path.dirname(path.join(root, f)), "node_modules")))].sort();
function isLink(p) { try { return fs.lstatSync(p).isSymbolicLink(); } catch { return false; } }

/**
 * Members of the root pnpm workspace holding their own pnpm virtual store (node_modules/.pnpm) while the root has one, without
 * being an install root (no pnpm-lock.yaml of their own): leftovers of a standalone install from before the package joined the workspace. Their dependencies resolve to
 * the root store, but the leftover can still be read, and vitest hashes the nearest lockfile it finds (node_modules/.pnpm/
 * lock.yaml) for a module cache it shares across the workspace, so one such package makes concurrent vitest runs clear the
 * shared cache under each other (ENOTEMPTY, fullA-16).
 */
export function orphanedPnpmStores({ root, files }) {
  // Only proven orphans: the repository root is a pnpm workspace with its own virtual store, and the package is a member of it by
  // the root pnpm-workspace.yaml globs (negations honoured) and not an install root of its own. A standalone nested project is
  // not judged.
  const ws = path.join(root, "pnpm-workspace.yaml");
  if (!fs.existsSync(ws) || !fs.existsSync(path.join(root, "node_modules", ".pnpm"))) return [];
  const globs = parseWorkspaceGlobs(fs.readFileSync(ws, "utf8"));
  const include = globs.filter((g) => !g.startsWith("!")).map(globToRegExp);
  const exclude = globs.filter((g) => g.startsWith("!")).map((g) => globToRegExp(g.slice(1)));
  return nodeModulesDirs(root, files).filter((nm) => {
    const pkg = path.dirname(nm); const rel = path.relative(root, pkg).split(path.sep).join("/");
    if (!rel || !include.some((re) => re.test(rel)) || exclude.some((re) => re.test(rel))) return false;
    return fs.existsSync(path.join(nm, ".pnpm")) && !fs.existsSync(path.join(pkg, "pnpm-lock.yaml"));
  }).map((nm) => path.relative(root, path.join(nm, ".pnpm")).split(path.sep).join("/"));
}

/**
 * Cache files that hold only a tool's own bookkeeping, never code a check executes, at exact paths: vitest's result ordering,
 * node_modules/.vite/vitest/results.json and node_modules/.vite/vitest/<40-hex project hash>/results.json, which vitest reads only
 * with JSON.parse to order test files (vitest 5.0.5 dist/chunks/cli-api.*.js ResultsCache.readFromCache).
 */
export const CACHE_NON_CODE = [/(^|\/)node_modules\/\.vite\/vitest\/([0-9a-f]{40}\/)?results\.json$/];
/** Every file in the workspace's tool caches (TOOL_CACHE_DIRS, not following links) with its content hash, keyed by path. */
export function toolCacheManifest({ root, files }) {
  const out = new Map();
  const walk = (dir) => {
    let entries; try { entries = fs.readdirSync(dir, { withFileTypes: true }); } catch (error) { if (error?.code === "ENOENT") return; throw new Error(`cannot list the tool cache ${path.relative(root, dir)} (${error?.code ?? error?.message})`); }
    for (const e of entries) {
      const p = path.join(dir, e.name); const rel = path.relative(root, p).split(path.sep).join("/");
      if (e.isDirectory()) walk(p);
      else if (e.isSymbolicLink()) out.set(rel, `link:${fs.readlinkSync(p)}`);
      else if (e.isFile()) { try { out.set(rel, createHash("sha256").update(fs.readFileSync(p)).digest("hex")); } catch (error) { if (error?.code !== "ENOENT") throw new Error(`cannot read the tool cache file ${rel} (${error?.code ?? error?.message})`); } }
    }
  };
  for (const d of nodeModulesDirs(root, files)) for (const name of TOOL_CACHE_DIRS) walk(path.join(d, name));
  return out;
}
/**
 * What a run did to the tool caches: a pre-existing cache file that changed or vanished means the checks may have read bytes that
 * are no longer there (a concurrent writer, an interrupted or partial write), so the verdict cannot name them. Files added while
 * it ran are listed and bound in env, though another process may have written them. Limits: a file changed and restored between
 * the two manifests is not observed, and a file added and read by a check is bound only as it stands at the end. CACHE_NON_CODE
 * files may change.
 */
export function toolCacheDrift(before, after) {
  const changed = []; const added = [];
  for (const [p, h] of before) if (!CACHE_NON_CODE.some((re) => re.test(p)) && after.get(p) !== h) changed.push(p);
  for (const p of after.keys()) if (!before.has(p)) added.push(p);
  return { changed: changed.sort(), added: added.sort() };
}

/**
 * The installed dependency bytes a class A verdict executes: every node_modules directory of the workspace (the root one holds
 * pnpm's virtual store) walked without following links, plus everything a link there resolves to outside those directories (a
 * workspace package's built and ignored files, a linked package elsewhere on disk), each resolved target hashed once. A real file
 * is its path, exec bit and content hash; a link is its target (a dangling one is bound as dangling). A cycle is cut by the
 * resolved-path set; a target that is the root, an ancestor of it or the filesystem root, any other entry type, an unreadable
 * entry, or more than `maxFiles` files throws. Tool caches (TOOL_CACHE_DIRS) leave no trace. Bytes that change while it hashes are caught by comparing the digest at the start
 * and end of the run and again at START.
 */
export function installedDependencyDigest({ root, files, maxFiles = 400000, includeCaches = true }) {
  const h = createHash("sha256");
  const realRoot = fs.realpathSync(root);
  const walkedRoots = nodeModulesDirs(root, files);
  const realWalked = walkedRoots.map((d) => { try { return fs.realpathSync(d); } catch (error) { if (error?.code === "ENOENT") return null; throw error; } }).filter(Boolean);
  const inside = (p, dir) => p === dir || p.startsWith(dir + path.sep);
  const rel = (p) => (inside(p, realRoot) ? path.relative(realRoot, p).split(path.sep).join("/") : p);
  const seen = new Set(); const pending = []; let count = 0;
  const fail = (what, error) => { throw new Error(`cannot read ${what} for the class A environment (${error?.code ?? error?.message ?? error})`); };
  const entry = (p, name, parentIsNodeModules, fromLink) => {
    const st = (() => { try { return fs.lstatSync(p); } catch (error) { return fail(rel(p), error); } })();
    if (st.isSymbolicLink()) {
      let target; let real;
      try { target = fs.readlinkSync(p); } catch (error) { return fail(`the link ${rel(p)}`, error); }
      // A dangling link (pnpm hoists optional packages for other platforms that were never installed) resolves nothing, so it
      // supplies no bytes; it is bound as dangling, and a target that later appears changes the digest.
      try { real = fs.realpathSync(p); } catch (error) { if (error?.code === "ENOENT") { h.update(`${rel(p)}\tdangling\t${target}\n`); return; } return fail(`the link ${rel(p)}`, error); }
      h.update(`${rel(p)}\tlink\t${target}\n`);
      if (!realWalked.some((d) => inside(real, d))) pending.push(real);
      return;
    }
    // pnpm's task-run state holds only completion markers, never code, and leaves no trace. Tool caches are bound byte for byte
    // unless `includeCaches` is false (only for the start/end comparison inside one run, which writes them itself).
    if (parentIsNodeModules && !fromLink && INSTALL_STATE_NOT_COVERED.has(name)) return;
    if (parentIsNodeModules && !fromLink && TOOL_CACHE_DIRS.has(name) && !includeCaches) return;
    if (st.isDirectory()) return walk(p, fromLink);
    if (st.isFile()) {
      if (++count > maxFiles) throw new Error(`more than ${maxFiles} installed dependency files; refusing an unbounded class A environment`);
      let bytes; try { bytes = fs.readFileSync(p); } catch (error) { return fail(rel(p), error); }
      h.update(`${rel(p)}\tfile\t${st.mode & 0o111 ? "x" : "-"}\t${createHash("sha256").update(bytes).digest("hex")}\n`);
      return;
    }
    throw new Error(`unsupported entry ${rel(p)} in the installed dependencies`);
  };
  const walk = (dir, fromLink) => {
    let names; try { names = fs.readdirSync(dir).sort(); } catch (error) { return fail(rel(dir), error); }
    const nm = path.basename(dir) === "node_modules";
    for (const name of names) {
      const p = path.join(dir, name);
      if (fromLink && (name === ".git" || (name === "node_modules" && walkedRoots.includes(p)))) continue;
      entry(p, name, nm, fromLink);
    }
  };
  for (const d of walkedRoots) {
    h.update(`root\t${rel(d)}\n`);
    try { fs.lstatSync(d); } catch (error) { if (error?.code === "ENOENT" || error?.code === "ENOTDIR") { h.update("absent\n"); continue; } fail(rel(d), error); }
    walk(d, false);
  }
  while (pending.length) {
    const real = pending.shift();
    if (seen.has(real)) continue;
    seen.add(real);
    if (real === path.parse(real).root || inside(realRoot, real)) throw new Error(`a dependency link resolves to ${real}, the repository or one of its ancestors; refusing to bind it`);
    h.update(`target\t${rel(real)}\n`);
    const st = fs.lstatSync(real);
    // A linked package outside the walked node_modules trees (a workspace package, or one elsewhere on disk) is hashed whole:
    // any of its files can be executed by a consumer (an ignored dist/, an implicit index.js, a file its entry imports).
    if (st.isDirectory()) walk(real, true); else entry(real, path.basename(real), false, true);
  }
  return h.digest("hex");
}

/**
 * What a class A verdict depends on outside the checked tree: every file of this gate package (code at any depth, the advisory
 * database, contracts, assets), node and pnpm versions, platform/arch, and each package's installed state
 * (node_modules/.modules.yaml) and the installed dependency bytes (installedDependencyDigest). The broker binds an admission ticket to it and recomputes it when the compile starts
 * (`rightkit-gate env-digest`), so a dependency or tool change while a compile waits invalidates the ticket.
 */
export function classAEnvironmentDigest({ root, files, spawn = defaultSpawn, includeCaches = true, gatePackage = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..") }) {
  // Only a file that does not exist is "absent"; any other read failure throws: an input whose bytes cannot be read is never
  // identified by a placeholder that would match the same failure later.
  // `optional`: only an install-state file may legitimately not exist; a gate file found by the walk must be readable.
  const fileDigest = (f, { optional = false } = {}) => { try { return createHash("sha256").update(fs.readFileSync(f)).digest("hex"); } catch (error) { if (optional && (error?.code === "ENOENT" || error?.code === "ENOTDIR")) return "absent"; throw new Error(`cannot read ${f} for the class A environment (${error?.code ?? error?.message})`); } };
  // `gatePackage` defaults to this very package; only a test that must not be disturbed by other writers of the live package injects another.
  const gatePkg = gatePackage;
  const walkGate = (dir) => fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.name === "node_modules" || e.name.startsWith(".") ? [] : e.isDirectory() ? walkGate(path.join(dir, e.name)) : [path.join(dir, e.name)]));
  const gateFiles = walkGate(gatePkg).sort().map((f) => `${path.relative(gatePkg, f)}:${fileDigest(f)}`);
  // Bounded: a pnpm that switches itself to the repository's packageManager version on first use, or waits on the network,
  // must fail the probe (and its process tree) within PNPM_PROBE_TIMEOUT_MS, never hang an admission.
  const pnpmVersion = (() => {
    const r = spawn("pnpm", ["--version"], { cwd: root, boundedMs: PNPM_PROBE_TIMEOUT_MS });
    if (r?.timedOut) throw new Error(`cannot identify pnpm for the class A environment (pnpm --version timed out after ${PNPM_PROBE_TIMEOUT_MS} ms; run it once in ${root} so pnpm can switch to the packageManager version)`);
    if (failed(r) || !text(r).trim()) throw new Error(`cannot identify pnpm for the class A environment (${r?.error ? `could not run pnpm: ${r.error.code ?? r.error.message}` : why(r)})`);
    return text(r).trim();
  })();
  const installState = files.filter((f) => /(^|\/)package\.json$/.test(f)).map((f) => path.join(path.dirname(path.join(root, f)), "node_modules", ".modules.yaml")).map((m) => `${path.relative(root, m)}:${fileDigest(m, { optional: true })}`);
  return createHash("sha256").update([`node ${process.version}`, `pnpm ${pnpmVersion}`, `platform ${process.platform}-${process.arch}`, ...gateFiles, ...installState, `installed ${installedDependencyDigest({ root, files, includeCaches })}`].join("\n")).digest("hex");
}

/** Split pathspecs into batches whose joined length stays under `limit` characters (well below Windows' 32767 command line). */
export function pathspecBatches(paths, limit = 8000) {
  const batches = []; let current = []; let size = 0;
  for (const p of paths) {
    if (current.length && size + p.length + 1 > limit) { batches.push(current); current = []; size = 0; }
    current.push(p); size += p.length + 1;
  }
  if (current.length) batches.push(current);
  return batches;
}

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

/**
 * The unit name in scan ids and ci/gate.config.json: the plain crate in a root workspace and in a workspace that is exactly that crate's
 * own directory (`crates/rightkit-fs` holding rightkit-fs: one workspace and lock per crate, rightkit#1285), `<cargoDir>:<crate>` below
 * any other workspace.
 */
export const rustUnitName = (cargoDir, crate) => (cargoDir === "." || cargoDir === `crates/${crate}` ? crate : `${cargoDir}:${crate}`);

/**
 * The vendored sources a crate compiles: `{ "vendor/<name>": [unit names] }`, from every quoted `../../vendor/<name>` path in the crate
 * manifests (a [patch.crates-io] or dependency `path`, rightkit-typeset's vendor/citationberg). A changed file under the directory
 * selects those units. `readManifest(unit)` returns the manifest text, or null.
 */
export function vendorOwners(units, readManifest) {
  const owners = {};
  for (const u of units) {
    let manifest = null;
    try { manifest = readManifest(u); } catch { manifest = null; }
    for (const m of String(manifest ?? "").matchAll(/["'](?:\.\.\/)+vendor\/([A-Za-z0-9_.@+-]+)\/?["']/g)) {
      const list = (owners[`vendor/${m[1]}`] ??= []);
      if (!list.includes(u.name)) list.push(u.name);
    }
  }
  return owners;
}

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
 * `metadata(dir)` (optional) replaces the cargo call with a reader of the same { cargoDir, members } shape (extra fields are ignored).
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
 * - An npm package scan (test:npm, tsc, eslint, biome) keys on the package's workspace closure (every dependency that is a workspace
 *   package, whatever its spec, transitively, by package name) and the root npm inputs; test:scripts-ci on scripts/, ci/, .github/, packages/release and the
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
          // A workspace package is a member whatever the spec: pnpm links it for a version range too (linkWorkspacePackages), and the
          // tsc row checks it from its committed source (deterministic-scans.mjs TSC_PROJECT). Only an explicit local spec must resolve.
          const dep = npmByPkgName.get(`${u.root ?? "."}\0${name}`);
          if (!dep) { if (/^(workspace:|link:|file:)/.test(String(spec))) unresolved = true; continue; }
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
  const siblingByCrate = new Map(rustUnits.filter((u) => wsOf(u) === `crates/${crateOf(u)}`).map((u) => [crateOf(u), u]));
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
      // A batched Clippy scan (clippy:batch:*, planScans `clippyBatching`) is keyed on the inputs of every member it names: each member's
      // own compile key, in member order. Any member that falls back to the whole tree makes the batch fall back too.
      if (/^clippy:batch:/.test(String(scan.id)) && Array.isArray(scan.batch?.units) && scan.batch.units.length && scan.batch.units.every((n) => known.has(n))) {
        const keys = scan.batch.units.map((n) => rustUnitKey(n, scan));
        return keys.includes(undefined) ? whole() : canonicalJson({ kind: "rust-batch", id: scan.id, units: scan.batch.units.map((n, i) => [n, keys[i]]) });
      }
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
      const keyed = rustUnitKey(unit, scan);
      return keyed === undefined ? whole() : keyed;
    } catch { return null; }
  };
  // The cache key of one Rust unit's compile or test scan, or undefined when the scan must fall back to the whole tree.
  function rustUnitKey(unit, scan) {
      const head = byName.get(unit);
      const ws = wsOf(head); const lock = lockFor(ws);
      if (!lock || typeof listFiles !== "function" || typeof readBlob !== "function") return undefined;
      const testScan = /^test:rust(?:-serial)?:/.test(scan.id);
      const ownConfig = crateFeatures?.[unit] ?? crateFeatures?.[crateOf(head)] ?? null;
      let testInputs = []; let declaredInputs = false;
      if (testScan) {
        const declared = ownConfig?.testInputs;
        if (declared !== undefined) {
          if (!Array.isArray(declared) || declared.length === 0 || !declared.every(plainRelative)) return undefined;
          testInputs = [...new Set(declared)].sort(); declaredInputs = true;
        }
      } else if (!COMPILE_SCAN_PREFIXES.has(String(scan.id).split(":")[0])) return undefined;
      if (!reachMemos.has(ws)) reachMemos.set(ws, new Map());
      const reached = [...reach(lock, crateOf(head), reachMemos.get(ws))];
      // A crate that is its own workspace (one workspace and lock per crate) holds its sibling crates in its lock as path packages: the
      // sibling's source is part of the closure whichever workspace it lives in, or a change to it would leave a stale key.
      const closure = [...new Set([head, ...reached.map((n) => byCrate.get(`${ws}\0${n}`) ?? siblingByCrate.get(n)).filter(Boolean)])].sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
      const extra = new Set(testInputs);
      for (const member of closure) {
        const found = externalOf(member);
        if (found.unresolved) return undefined;
        if (testScan && !declaredInputs && found.leavesCrate) return undefined;
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
  }
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
export function quietLeaseLane(o, { tag = "rightkit-gate:", reason = "rightkit-gate-quiet-tests", ttlMs: baseTtlMs = QUIET_LEASE_TTL_MS, waitDrainMs = QUIET_LEASE_WAIT_DRAIN_MS, pid = process.pid } = {}) {
  const which = (cmd) => {
    const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]);
    return failed(r) ? null : (text(r).split(/\r?\n/).map((l) => l.trim()).find(Boolean) ?? null);
  };
  return {
    // `want.ttlMs` (from the scan runner): the lane's own bound, every quiet scan's timeout plus a margin, so the lease cannot
    // expire while a quiet suite still runs (each scan is killed at its timeout first).
    acquire: async (want = {}) => {
      const ttlMs = Number.isFinite(want.ttlMs) && want.ttlMs > baseTtlMs ? want.ttlMs : baseTtlMs;
      // A direct (Linux) host never takes a broker lease, whatever is on PATH (managed-cargo directPlatform).
      if (directPlatform(o.platform ?? process.platform)) { o.out(`${tag} note: direct host (no broker on ${o.platform ?? process.platform}); quiet test scans run without a broker lease`); return null; }
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

/**
 * `rightkit-gate install-hook` installs no git hook: commit and push run no gate and no local hook check (docs/GATE-CONTRACT.md).
 * It retires what an earlier release wrote, each file only by its own header or marker (../hook-retirement.mjs), never by name or place:
 *   - .githooks/pre-push, the gate hook, and the .githooks/<name> forwarders that chained .git/hooks beside it;
 *   - the advisory ownership pre-commit hook (in .githooks, in .git/hooks or in the directory core.hooksPath names), with the foreign
 *     pre-commit it displaced (pre-commit.rightkit-foreign) put back as pre-commit;
 *   - the repository-local core.hooksPath, only when it is exactly `.githooks` and nothing foreign remains in that directory.
 * A foreign hook (any other file in .githooks, .git/hooks or the configured directory) is never read for change, moved or deleted, and
 * a foreign .githooks directory keeps its core.hooksPath. The global and system git configuration is never touched. It still registers
 * the one-door agent hook in .claude/settings.json (the agent side, not a git hook).
 */
async function installHook(o, root) {
  const git = (...a) => o.spawn("git", a, { cwd: root });
  const ops = { readdir: o.readdir, readFile: o.readFile, lstat: o.lstat ?? fs.lstatSync, unlink: o.unlink ?? fs.unlinkSync, rename: o.rename ?? fs.renameSync, rmdir: o.rmdir ?? fs.rmdirSync };
  const done = [];
  const isRealDirectory = (dir) => { try { const stat = ops.lstat(dir); return stat.isDirectory() && !stat.isSymbolicLink(); } catch { return false; } };
  const label = (dir) => posix(path.relative(root, dir)) || ".";
  const retire = (dir, options) => {
    if (!isRealDirectory(dir)) return null;
    const result = retireHooksIn(dir, ops, options);
    for (const name of result.removed) done.push(`retired ${label(dir)}/${name}`);
    for (const name of result.restored) done.push(`restored ${label(dir)}/${name} from ${FOREIGN_PRE_COMMIT}`);
    if (result.removedDir) done.push(`removed the empty ${label(dir)}`);
    return result;
  };

  // The gate hook, its forwarders and the ownership step in .githooks, then the ownership step wherever else git ran hooks from.
  const hooksDir = path.join(root, ".githooks");
  retire(hooksDir, { scripts: true });
  const gitHooksResult = git("rev-parse", "--git-path", "hooks");
  const gitHooksDir = !failed(gitHooksResult) && text(gitHooksResult).trim() ? path.resolve(root, text(gitHooksResult).trim()) : null;
  const configured = git("config", "--local", "--get", "core.hooksPath");
  const hooksPath = failed(configured) ? "" : text(configured).trim();
  const seen = new Set([path.resolve(hooksDir)]);
  for (const dir of [gitHooksDir, hooksPath && hooksPath !== ".githooks" ? path.resolve(root, hooksPath) : null]) {
    if (!dir || seen.has(path.resolve(dir))) continue;
    seen.add(path.resolve(dir));
    retire(dir, { scripts: false });
  }

  // core.hooksPath: unset only when it is `.githooks` and that directory holds nothing foreign (it is gone, or empty).
  let remaining = [];
  if (hooksPath === ".githooks") {
    if (isRealDirectory(hooksDir)) { try { remaining = [...o.readdir(hooksDir)].map(String).sort(); } catch { remaining = ["(unreadable)"]; } }
    else if (o.exists(hooksDir)) remaining = ["(not a plain directory)"];
    if (!remaining.length) {
      const unset = git("config", "--local", "--unset", "core.hooksPath");
      if (failed(unset)) { o.err(`rightkit-gate: git config --unset core.hooksPath failed: ${why(unset)}`); return 2; }
      done.push("unset core.hooksPath (was .githooks)");
    } else {
      done.push(`core.hooksPath stays .githooks: foreign file(s) remain there (${remaining.join(", ")}); the hooks in .git/hooks do not run while it is set`);
    }
  } else if (hooksPath) done.push(`core.hooksPath is ${JSON.stringify(hooksPath)}: left as it is`);
  if (!done.length) done.push("no git hook of RightKit's to retire");

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
  o.out("rightkit-gate: no git hook is written: commit and push run no gate and no local hook check (the gate is for building)");
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
/**
 * The app a door run is for: the directory the gate was run from when it holds a release config (a nested app such as
 * apps/coderight-tauri), else the repository root. Every right-release stage runs there, the sealed set is read from there, and the
 * receipt is keyed to it (receiptKey). Returns { appRoot, appDir } with appDir relative to the repository root, "." at the root.
 */
export function appRootOf({ root, cwd, exists = fs.existsSync }) {
  const inside = cwd && (cwd === root || cwd.startsWith(root + path.sep));
  const hasConfig = (d) => ["right-release.config.json", "right-release.config.mjs"].some((f) => exists(path.join(d, f)));
  // The nearest directory at or above cwd (below the root) that carries a release config is the app; a deeper cwd (apps/foo/src) is apps/foo.
  let appRoot = root;
  for (let d = path.resolve(cwd); d.startsWith(root) && d !== root; d = path.dirname(d)) { if (hasConfig(d)) { appRoot = d; break; } }
  return { appRoot, appDir: path.relative(root, appRoot).split(path.sep).join("/") || "." };
}

async function runStages(o, args, root, git, io) {
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  const tag = "rightkit-gate:";
  const { appRoot, appDir } = appRootOf({ root, cwd: o.cwd, exists: o.exists });
  const to = option("--to");
  // dev and test are door stages like the rest: the same commit-only route (select a commit, sync its snapshot under ownership, class A
  // of the snapshot), then right-release dev|test with the snapshot's app root as its cwd. They have no stage list of their own.
  const devTest = to === "dev" || to === "test";
  let stages = devTest ? [] : stagesTo(to);
  if (!stages) { o.err(`${tag} --to must be one of ${[...STAGES, "dev", "test"].join(", ")}`); return 2; }
  // Every stage the door runs has a row naming where it reads its source from (gate-stages.mjs STAGE_SOURCES, drift-tested in
  // door-stages.test.mjs); a stage added without one is refused rather than run with an unstated source.
  if (!STAGE_SOURCES[to]) { o.err(`${tag} BLOCKED: stage ${to} has no STAGE_SOURCES row naming its source; nothing ran`); return 2; }
  const platform = option("--platform");
  const tier = option("--tier");
  if (stages.includes("package") && !PLATFORMS.includes(platform)) { o.err(`${tag} --to ${to} needs --platform ${PLATFORMS.join("|")}`); return 2; }
  // iOS and Android (Adrian, 2026-10-26: every app build goes through the door, class A first): check, compile and package run here as
  // for desktop; there is no installed-QA lane for them, so the qualify stage does not exist for a mobile platform and is never
  // recorded as passed. Qualification is the store's test track, after the release stage uploads the sealed build from this host
  // (TestFlight, Play internal track; right-release upload --apply, no patch/update tier).
  const mobile = MOBILE_PLATFORMS.has(platform);
  if (mobile && to === "qualify") { o.err(`${tag} --to qualify has no stage for ${platform}: there is no installed-QA lane for a mobile build; qualification is the store's test track after --to release. Run --to package or --to release.`); return 2; }
  if (mobile) stages = stages.filter((s) => s !== "qualify");
  if (stages.includes("release") && !mobile && !["patch", "update"].includes(tier)) { o.err(`${tag} --to release needs --tier patch|update (chosen explicitly, never defaulted)`); return 2; }
  // `--route`: which route this stage takes here and whether it refuses, answered without running anything (rightkit#1297).
  if (o.routeProbe) return await runRouteProbe(o, args, root, git, { to, platform, stages: devTest ? [] : stages, devTest, appDir });
  // Commit-only build (GATE-CONTRACT §1): on every host (not hosted CI, whose runner checkout is the commit) the door selects a commit
  // (--commit <rev>, default HEAD), takes the host's build ownership, syncs the commit's gate-owned snapshot once, and every stage
  // below reads that snapshot; the checkout may hold anything and is never a build source. Every non-hosted host takes this route: Mac
  // and Dell compile it through the RustRight snapshot carrier (`deps.snapshotBroker`, wired by `productionEntryDeps`; item 7 rows 7.20
  // to 7.24, rightkit#1301), right-release's own children included. Once it is selected any failure refuses; there is no retry on the
  // checkout, and no flag or environment variable selects another route.
  const hosted = o.env.GITHUB_ACTIONS === "true";
  let selected = null;
  if (!o.doorSnapshot && !hosted) {
    const snapModule = await import("../snapshot.mjs");
    try { selected = snapModule.resolveCommit(root, option("--commit") ?? "HEAD"); }
    catch (e) { o.err(`${tag} cannot resolve ${option("--commit") ?? "HEAD"} to a commit: ${e.message}`); return 2; }
  }
  // The route and every refusal the door can meet before it takes ownership are `routePlan`'s, the one function the `--route` probe
  // also answers from (rightkit#1297 L1): the probe and the real run cannot drift, and the door-route parity test compares the two.
  if (!o.doorSnapshot) {
    const whichRoute = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
    const routeExecutor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which: whichRoute, readFile: o.readFile });
    let decided;
    try { decided = routePlan(o, { to, stages: devTest ? [] : stages, devTest, executor: routeExecutor, capabilities: brokerCapabilityState(o, { to, executor: routeExecutor }) }); }
    catch (e) { o.err(`${tag} ${e.message}`); return 2; }
    if (decided.refusal) { o.err(`${tag} BLOCKED: ${decided.refusal.code}: ${decided.refusal.message}; nothing ran${decided.route === "snapshot" ? " and the checkout is never built" : ""}`); return 1; }
  }
  if (!o.doorSnapshot && !hosted) {
    const snap = await import("../snapshot.mjs");
    const { acquireBuildOwnership, ownershipFile } = await import("../build-ownership.mjs");
    const snapshotsAt = o.snapshotRoot ?? snap.snapshotRoot({ platform: o.platform, env: o.env });
    let own;
    try { own = (o.acquireOwnership ?? acquireBuildOwnership)({ file: ownershipFile({ platform: o.platform, beside: o.snapshotRoot }), stage: `door:${to}`, snapshot: path.join(snapshotsAt, snap.repositoryOf(root).key), commit: selected.commit, platform: o.platform, env: o.env }); }
    catch (e) { if (e.code !== "busy" && e.code !== "refused") throw e; o.err(`${tag} BLOCKED: ${e.message}; nothing ran${e.code === "refused" ? ", nothing created" : ""}`); return 1; }
    o.out(`${tag} build ownership ${own.joined ? "joined" : "held"} ${own.handle}${own.reclaimed ? ` (reclaimed from ${own.reclaimed.from}: ${own.reclaimed.proofs.join("; ")})` : ""}`);
    try {
      // One sync per door run: a second one would prune what class A installed into the snapshot.
      let synced;
      try { synced = (o.syncSnapshot ?? snap.syncSnapshot)({ checkout: root, rev: selected.commit, root: snapshotsAt }); }
      catch (e) { o.err(`${tag} ${e.code === "error" ? "RK_CHECK_ERROR" : "BLOCKED"}: snapshot ${e.message}; nothing ran`); return e.code === "error" ? 2 : 1; }
      o.out(`${tag} snapshot ${synced.path} is commit ${synced.commit} tree ${synced.tree} (written=${synced.written} removed=${synced.removed} unchanged=${synced.unchanged})`);
      return await runStages({ ...o, buildOwnership: own, doorSnapshot: { own, path: synced.path, commit: synced.commit, tree: synced.tree, root: snapshotsAt, ledger: { entries: [] } } }, args, root, git, { ...io, buildOwnership: own });
    } finally {
      const r = own.release();
      if (r?.released === false && !own.joined) o.err(`${tag} build ownership ${own.handle} kept: ${r.why}`);
    }
  }
  if (devTest) return await runDevTest(o, args, root, git, io, to, appRoot, appDir);
  const ds = o.doorSnapshot ?? null;
  const commit = ds ? ds.commit : text(git("rev-parse", "HEAD")).trim();
  const tree = ds ? ds.tree : text(git("rev-parse", "HEAD^{tree}")).trim();
  const common = text(git("rev-parse", "--git-common-dir")).trim();
  if (!/^[0-9a-f]{40}$/.test(commit) || !tree || !common) { o.err(`${tag} cannot read HEAD and the git directory`); return 2; }
  const commonDir = path.resolve(root, common);
  // The snapshot route reads only the commit; the checkout may hold anything (GATE-CONTRACT §1: no dirty-tree condition).
  const dirty = ds ? null : git("status", "--porcelain", "--untracked-files=no");
  if (!ds && (failed(dirty) || text(dirty).trim())) { o.err(`${tag} BLOCKED: the checkout differs from HEAD; every stage works on the commit, so commit or stash first`); return 1; }

  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
  if (executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
  const version = (cmd, a) => { const r = o.spawn(cmd, a, { cwd: root }); return failed(r) ? null : text(r).trim().split(/\r?\n/)[0]; };
  const listed = git("ls-tree", "-r", "--name-only", "-z", commit);
  const lockFiles = {};
  for (const f of failed(listed) ? [] : text(listed).split("\0").filter((x) => /(^|\/)(Cargo\.lock|pnpm-lock\.yaml)$/.test(x))) {
    const shown = git("show", `${commit}:${f}`); if (!failed(shown)) lockFiles[f] = text(shown);
  }
  const binding = bindingOf({
    tree, lockFiles, gateFile: fileURLToPath(import.meta.url), executor: executor.kind,
    toolchain: { classA: { "cargo-deny": version("cargo-deny", ["--version"]), "cargo-audit": version("cargo-audit", ["--version"]), actionlint: version("actionlint", ["-version"]), shellcheck: version("shellcheck", ["--version"]) }, node: process.version, rustc: version(executor.kind === "broker" && executor.cargo[0] === "rustright" ? "rustright" : "rustc", executor.kind === "broker" && executor.cargo[0] === "rustright" ? ["rustc", "-V"] : ["-V"]), pnpm: version("pnpm", ["--version"]) },
  });
  // Release writes to R2. The write credential lives only in the trusted publisher: a GitHub Actions job in the protected
  // publication environment, which sets RIGHTKIT_PUBLISHER=github-environment. Hosts and agents never hold it, so the door refuses the
  // stage anywhere else rather than fail half way through an upload.
  if (stages.includes("release") && !mobile && !(o.env.GITHUB_ACTIONS === "true" && o.env.RIGHTKIT_PUBLISHER === "github-environment")) {
    o.err(`${tag} BLOCKED: --to release runs only in the trusted publisher (the release workflow's protected publication job). Run --to qualify here, then dispatch the release workflow for ${commit.slice(0, 12)}`);
    return 1;
  }
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
  const previous = (o.readReceipt ?? readReceipt)({ commonDir, commit, app: appDir });
  // Same host and inputs: every recorded stage counts. Same source on another host: only the stages bound to sealed bytes (package and
  // later) carry over; check and compile run again here.
  const receipt = previous && sameBinding(previous.binding, binding) ? previous
    : previous && sameSource(previous.binding, binding) ? { ...previous, binding, stages: Object.fromEntries(Object.entries(previous.stages ?? {}).filter(([k]) => k !== "check" && k !== "compile")) }
      : { schema: RECEIPT_SCHEMA, commit, app: appDir, binding, stages: {} };
  const receiptOut = option("--receipt-out");
  const save = () => {
    (o.writeReceipt ?? writeReceipt)({ commonDir, receipt });
    if (receiptOut !== undefined) o.writeFile(path.resolve(o.cwd, receiptOut), `${JSON.stringify(receipt, null, 2)}\n`);
  };

  const cli = fileURLToPath(new URL("../cli/right-release.mjs", import.meta.url));
  // On the snapshot route right-release runs INSIDE the snapshot (its sources, config and every command it spawns resolve there);
  // the door run records the split, so the child can prove which snapshot it may read and which checkout holds its outputs
  // (source-split.mjs). Anywhere else it runs in the app root of the checkout, which hosted CI's runner holds at the commit.
  const sourceOf = ds ? sourceSplitOf({ root, ds, appDir }) : null;
  const rightRelease = async (stage, cliArgs, extraEnv = {}) => {
    // A broker host's child reaches the broker only through this stage's carrier (carried-cargo.mjs): its Cargo calls, and its pnpm, node and QA
    // steps as adapter jobs with the third-party children's compiles nested under them. Every call is in the gate-owned ledger and is verified
    // again after the child ends.
    const carrier = await carrierForChildren({ o, ds, root, git, executor, stage, tag });
    if (carrier.refusal) { o.err(`${tag} BLOCKED: ${carrier.refusal}; nothing ran`); return false; }
    const run = startRun({ commonDir, commit, tree, stage, ...(sourceOf ? { source: sourceOf } : {}) });
    let childOk = false;
    try {
      o.out(`${tag} [${stage}] right-release ${cliArgs.join(" ")}${sourceOf ? ` (from the snapshot ${ds.path})` : ""}`);
      const r = (o.spawnInherit ?? ((cmd, a, opts) => spawnInherit(cmd, a, { ...opts, stdio: "inherit" })))(process.execPath, [cli, ...cliArgs], { cwd: sourceOf ? snapshotAppRootOf(ds.path, appDir) : appRoot, env: carriedEnv(carrier, { ...o.env, ...run.env }, extraEnv) });
      childOk = r.status === 0;
    } finally { endRun(run); }
    const carriedOk = await carrier.finish({ stage });
    return childOk && carriedOk;
  };

  o.out(`${tag} --to ${to} for ${commit.slice(0, 12)}${appDir === "." ? "" : ` (app ${appDir})`}: ${stages.join(" -> ")}; executor ${executor.kind}`);
  // A later stage on the sealed bytes: check, compile and package already passed for them (in this receipt) and the bytes on disk are
  // the ones they sealed, so they are not run again.
  // The release configs of the commit (under this app) that declare `sidecars`: the commit's config decides on the snapshot route, never the checkout's.
  const sidecarConfigsOf = () => {
    const listedConfigs = git("ls-tree", "-r", "--name-only", "-z", commit);
    return (failed(listedConfigs) ? [] : text(listedConfigs).split("\0").filter((f) => /(^|\/)right-release\.config\.(json|mjs)$/.test(f)).filter((f) => appDir === "." || f.startsWith(`${appDir}/`)))
      .filter((f) => { try { return /\bsidecars\b/.test(o.readFile(path.join(ds ? ds.path : root, f))); } catch { return false; } });
  };
  const sealedNow = receipt.stages.package?.ok ? (o.sealedFor ?? sealedFor)({ root: appRoot, commit, platform }) : null;
  const sealedIntact = Boolean(sealedNow && sealedNow.id === receipt.stages.package.sealed && sealedNow.digest === receipt.stages.package.manifestSha256);
  // Every deterministic scan (full class A) runs before any compile, for every door stage. The app rules run at the tier the stage
  // owes: dev, test and package take the compile-tier rules; qualify and release add the release-only rules (approvals, ratified
  // rulings, notice digests), which are conditions for publishing, not for compiling (Adrian, 2026-10-30, rightkit#1296; the
  // 2026-10-28 change to release tier everywhere was never his decision and blocked HeardRight's dev and test builds).
  const earlyTier = ["qualify", "release"].includes(to) ? "release" : "compile";
  const tierAtLeast = (have, need) => have === "release" || need === "compile";
  for (const stage of stages) {
    if (stage === "check" && receipt.stages.check?.ok && tierAtLeast(receipt.stages.check.ruleTier ?? "compile", earlyTier)) { o.out(`${tag} [check] already passed (${receipt.stages.check.ruleTier ?? "compile"} tier) for this exact input (commit, tree, lockfiles, gate, toolchain)`); continue; }
    if (stage === "compile" && receipt.stages.compile?.ok) { o.out(`${tag} [${stage}] already passed for this exact input (commit, tree, lockfiles, gate, toolchain)`); continue; }
    if ((stage === "check" || stage === "compile" || stage === "package") && sealedIntact) {
      o.out(`${tag} [${stage}] already passed for the sealed set ${sealedNow.id} (manifest ${sealedNow.digest.slice(0, 12)})`); continue;
    }
    // The publisher (another runner, often another OS) cannot repeat installed QA; it takes the qualify result recorded for exactly
    // these sealed bytes. --to qualify itself always runs it.
    if (stage === "qualify" && to !== "qualify" && sealedIntact && receipt.stages.qualify?.ok && receipt.stages.qualify.manifestSha256 === sealedNow.digest) {
      o.out(`${tag} [qualify] already passed for the sealed set ${sealedNow.id} (manifest ${sealedNow.digest.slice(0, 12)})`); continue;
    }
    // The publisher only uploads. It never checks out a build environment, signs or runs installed QA, so every earlier stage must
    // already stand in its receipt for exactly the sealed bytes on disk; anything else is refused, never rebuilt or requalified here.
    if (to === "release" && !mobile && stage !== "release") {
      o.err(`${tag} BLOCKED: [${stage}] --to release only uploads: the receipt must already record ${stage} for the sealed bytes on disk (${sealedNow ? `manifest ${sealedNow.digest.slice(0, 12)}` : "none found"}); run --to qualify on the host and stage again`);
      return 1;
    }
    // The start of every stage: the snapshot is exactly tree + dependencies + the ledger of the stages before it (7.08, decision 2 Q1).
    if (ds && !(await openSnapshotStage({ o, ds, root, stage, tag }))) { receipt.stages[stage] = { ok: false, at: new Date().toISOString(), snapshot: { path: ds.path, tree: ds.tree, intactAfter: false }, ownership: ds.own.handle }; save(); o.err(`${tag} --to ${to} stopped before ${stage}: the snapshot is not the commit's bytes plus the ledger; nothing ran`); return 2; }
    let ok = false; const at = new Date().toISOString(); let extra = {}; let directReceipt = null;
    if (stage === "check") {
      ok = (await (o.runCheck ?? (() => main(["check", "--commit", ds ? commit : "HEAD", "--full-a", ...(ds ? ["--snapshot", ds.path] : []), "--rule-tier", earlyTier], { ...io, cwd: root, ...(ds ? { buildOwnership: ds.own } : {}) })))(earlyTier)) === 0;
      extra = { ruleTier: earlyTier };
    }
    else if (stage === "compile") {
      // A Tauri crate with externalBin needs its sidecars in src-tauri/binaries before it compiles (its build script refuses
      // otherwise), so every release config that declares `sidecars` is staged first, through right-release under this door run.
      const sidecarConfigs = sidecarConfigsOf();
      ok = true;
      // On the snapshot route the sidecars are staged from the snapshot's own config and sources through right-release's source split
      // (the staged files and receipt land beside the sources the compile reads; the caches and build trees stay the checkout's), before
      // `main --all` compiles. A broker host's stage-sidecar child reaches Cargo through the stage's carrier (carried-cargo.mjs).
      // right-release runs in appRoot (nested apps), so the repository-relative config is handed over absolute (Astra, second pass).
      // On the snapshot route sidecars are staged ONCE per door run by the ledger bookkeeping (attribution diff, immutable staged entries).
      if (ok && ds) ok = await stageSidecarsOnce({ o, ds, root, git, configs: sidecarConfigs, appDir, cli, commonDir, commit, tree, stage, executor });
      else for (const config of ok ? sidecarConfigs : []) if (!await rightRelease(stage, ["stage-sidecar", "--config", path.join(root, config)])) { ok = false; break; }
      if (ok) ok = (await (o.runCompile ?? (() => main(ds ? ["--all", "--commit", commit, "--snapshot", ds.path] : ["--all"], { ...io, cwd: root, readStdin: () => "", ...(ds ? { buildOwnership: ds.own, snapshotLedger: ds.ledger?.entries ?? [], onCompileLedger: (entries) => { ds.compileAdmitted = entries; }, onDirectReceipt: (r) => { directReceipt = r; } } : {}) })))()) === 0;
      if (directReceipt) extra = { ...extra, direct: directReceipt };
      // The cross-check with `.rightkit/compile-inputs.json`: a declared input that covers ledger entries must carry the digest of those
      // very bytes in the compile receipt (the broker digests declared inputs in place in the snapshot, BC:39).
      if (ok && ds && directReceipt?.declaredInputDigests) {
        const { declaredInputProblems } = await import("../stage-ledger.mjs");
        const { judgeCompileInputs } = await import("../compile-inputs.mjs");
        const bad = declaredInputProblems({ snapshot: ds.path, entries: ds.ledger?.entries ?? [], receiptDigests: directReceipt.declaredInputDigests, judge: (dir) => judgeCompileInputs(dir) });
        if (bad.length) { for (const p of bad.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [compile] ${p}`); ok = false; }
      }
    }
    else if (stage === "package") {
      // On the snapshot route the sidecars are staged ONCE per door run, before the first stage that builds the Tauri crate: here when
      // compile was already passed (a separately invoked `--to package` re-stages after the sync, so it never meets a missing externalBin).
      // right-release then takes its identity from the selected commit and the verified snapshot (release-source-identity.mjs, row 7.04),
      // never the checkout's HEAD, status or branch; its outputs, state and the sealed directory stay in the checkout's `.right-release`.
      ok = ds ? await stageSidecarsOnce({ o, ds, root, git, configs: sidecarConfigsOf(), appDir, cli, commonDir, commit, tree, stage, executor }) : true;
      // Source QA (right-release qa --stage source) exists for mac and win only; a mobile package is the signed build alone.
      // Row 7.04c (R-T1, 7.09): the snapshot holds no Cargo target. On a direct host the checkout's output directory is resolved here, in the primary
      // checkout, and handed to right-release absolute: it binds every manifest's target for this stage, and the sealed manifest must record it back
      // (the door never trusts the child's value alone). One workspace is pinned as a whole; several are refused, naming them. A broker host's target
      // is the carrier's (rows 7.20 to 7.24): the carried calls' signed receipts name it (carried-cargo.mjs), and no CARGO_TARGET_DIR is passed.
      let packagePin = { dir: null };
      if (ok && ds && executor.kind !== "broker") {
        packagePin = cargoOutputPin({ o, git, root, commit, executor });
        if (packagePin.refusal) { o.err(`${tag} BLOCKED: RK_SNAPSHOT_OUTPUT_UNPINNED: [package] ${packagePin.refusal}; nothing was built`); ok = false; }
        else if (packagePin.dir) o.out(`${tag} [package] output directory for the Cargo workspace: ${packagePin.dir} (the checkout's)`);
      }
      const packageEnv = packagePin.dir ? { CARGO_TARGET_DIR: packagePin.dir } : {};
      ok = ok && (mobile || await rightRelease(stage, ["qa", "--stage", "source", "--platform", platform], packageEnv)) && await rightRelease(stage, ["build", "--platform", platform], packageEnv);
      const sealed = ok ? (o.sealedFor ?? sealedFor)({ root: appRoot, commit, platform }) : null;
      if (ok && !sealed) { o.err(`${tag} [package] the build reported success but sealed nothing for ${commit.slice(0, 8)} on ${platform}`); ok = false; }
      if (sealed) extra = { platform, sealed: sealed.id, manifestSha256: sealed.digest };
      // The cross-check: the Cargo targets the sealed manifest records are the directory this door resolved itself. A different one voids the stage.
      if (ok && sealed && ds && packagePin.dir) {
        let recorded = null;
        try { recorded = JSON.parse(String(o.readFile(sealed.manifest))).boundTargets ?? null; } catch { recorded = null; }
        const { crossCheckBoundTargets, SPLIT_TARGET_DRIFT_CODE } = await import("../release-artifact-path.mjs");
        const drift = crossCheckBoundTargets({ recorded, expected: [packagePin.dir] });
        if (drift.length) {
          for (const problem of drift.slice(0, 10)) o.err(`${tag} ${SPLIT_TARGET_DRIFT_CODE}: [package] ${problem}`);
          const closedDrift = await closeSnapshotStage({ o, ds, root, stage, step: stage, tag });
          receipt.stages[stage] = { ok: false, at, voided: SPLIT_TARGET_DRIFT_CODE, ...extra, snapshot: { path: ds.path, tree: ds.tree, intactAfter: closedDrift.ok }, ownership: ds.own.handle };
          save();
          o.err(`${tag} --to ${to} stopped at ${stage}: ${SPLIT_TARGET_DRIFT_CODE}; the sealed set is void and nothing after it ran`);
          return 2;
        }
      }
      // A broker host's Cargo target is the carrier's: every target the sealed manifest records is a directory a verified, signed receipt of this
      // stage named (`outputDir`), never one the child alone reported.
      if (ok && sealed && ds && executor.kind === "broker") {
        let recorded = null;
        try { recorded = JSON.parse(String(o.readFile(sealed.manifest))).boundTargets ?? null; } catch { recorded = null; }
        const { crossCheckBoundTargets, SPLIT_TARGET_DRIFT_CODE } = await import("../release-artifact-path.mjs");
        // The commit's workspaces decide whether a bound target is owed; the receipts compared are THIS package stage's own (rightkit#1301 L1).
        const owed = Object.keys(await snapshotCargoWorkspaces({ git, ds, root })).length > 0;
        const want = [...(ds.carriedByStage?.[stage] ?? [])];
        let drift = [];
        if (owed) {
          if (!Array.isArray(recorded) || !recorded.length) drift = ["the sealed manifest records no bound Cargo target; a broker host's package stage binds the output directory its carried build's receipt named"];
          else if (!want.length) drift = ["no verified receipt of this package stage names an output directory, so the recorded target cannot be checked"];
          else drift = crossCheckBoundTargets({ recorded, expected: want });
        }
        if (drift.length) {
          for (const problem of drift.slice(0, 10)) o.err(`${tag} ${SPLIT_TARGET_DRIFT_CODE}: [package] ${problem}`);
          const closedDrift = await closeSnapshotStage({ o, ds, root, stage, step: stage, tag });
          receipt.stages[stage] = { ok: false, at, voided: SPLIT_TARGET_DRIFT_CODE, ...extra, snapshot: { path: ds.path, tree: ds.tree, intactAfter: closedDrift.ok }, ownership: ds.own.handle };
          save();
          o.err(`${tag} --to ${to} stopped at ${stage}: ${SPLIT_TARGET_DRIFT_CODE}; the sealed set is void and nothing after it ran`);
          return 2;
        }
      }
    } else {
      const sealed = receipt.stages.package?.ok ? receipt.stages.package : null;
      const current = sealed ? (o.sealedFor ?? sealedFor)({ root: appRoot, commit, platform }) : null;
      if (!sealed || !current || current.id !== sealed.sealed || current.digest !== sealed.manifestSha256) {
        o.err(`${tag} BLOCKED: [${stage}] needs the sealed set the package stage produced for this commit, unchanged (sealed manifest digest)`);
        ok = false;
      } else if (stage === "release" && mobile) {
        // The host uploads the sealed mobile build to the store's test track itself (the credentials live in the host's credential
        // store); the release tier is owed first, as for desktop qualification.
        const releaseTier = receipt.stages.check?.ok && receipt.stages.check.ruleTier === "release"
          ? (o.out(`${tag} [release] the release tier already passed at check for this exact input; not run again`), 0)
          : await (o.runReleaseCheck ?? (() => main(["check", "--commit", ds ? commit : "HEAD", "--full-a", ...(ds ? ["--snapshot", ds.path] : []), "--rule-tier", "release"], { ...io, cwd: root, ...(ds ? { buildOwnership: ds.own } : {}) })))();
        if (releaseTier !== 0) { o.err(`${tag} [release] the release tier (every rule and scan) is red for ${commit.slice(0, 12)}; nothing was uploaded`); ok = false; }
        else ok = await rightRelease(stage, ["upload", "--platform", platform, "--release", sealed.sealed, "--apply"]);
        extra = { sealed: sealed.sealed, manifestSha256: sealed.manifestSha256, track: platform === "ios" ? "testflight" : "play-internal", ruleTier: "release" };
      } else if (stage === "qualify") {
        // Qualification owes the release tier: every rule and scan, the ones held out of compile admission included (app-check.mjs TIERS).
        const releaseTier = receipt.stages.check?.ok && receipt.stages.check.ruleTier === "release"
          ? (o.out(`${tag} [qualify] the release tier already passed at check for this exact input; not run again`), 0)
          : await (o.runReleaseCheck ?? (() => main(["check", "--commit", ds ? commit : "HEAD", "--full-a", ...(ds ? ["--snapshot", ds.path] : []), "--rule-tier", "release"], { ...io, cwd: root, ...(ds ? { buildOwnership: ds.own } : {}) })))();
        if (releaseTier !== 0) { o.err(`${tag} [qualify] the release tier (every rule and scan) is red for ${commit.slice(0, 12)}; nothing was qualified`); ok = false; }
        else ok = await rightRelease(stage, ["qa", "--stage", "installed", "--platform", platform, "--release", sealed.sealed]);
        extra = { sealed: sealed.sealed, manifestSha256: sealed.manifestSha256, ruleTier: "release" };
      } else {
        if (!receipt.stages.qualify?.ok || receipt.stages.qualify.manifestSha256 !== sealed.manifestSha256) { o.err(`${tag} BLOCKED: [release] the sealed set was not qualified (installed QA) in this door run's receipt`); ok = false; }
        else { ok = await rightRelease(stage, ["upload", "--release", sealed.sealed, "--platform", platform, "--tier", tier]); extra = { sealed: sealed.sealed, manifestSha256: sealed.manifestSha256, tier }; }
      }
    }
    if (ds) {
      // The stage covers the commit only if the snapshot is still its tree plus the ledger and the host is still this door's (E6, 7.08):
      // what the stage's children wrote may remain only as ledger entries, recorded by this gate's own walk and kept in the receipt (never
      // in the snapshot); a staged entry changed since staging voids the stage.
      const closed = await closeSnapshotStage({ o, ds, root, stage, step: stage, tag });
      const { receiptLedger } = await import("../stage-ledger.mjs");
      extra = { ...extra, snapshot: { path: ds.path, tree: ds.tree, intactAfter: closed.ok, ledger: receiptLedger(closed.entries) }, ownership: ds.own.handle };
      if (!closed.ok) { receipt.stages[stage] = { ok: false, at, ...extra }; save(); o.err(`${tag} --to ${to} stopped at ${stage}: the result covers no known bytes`); return 2; }
    }
    receipt.stages[stage] = { ok, at, ...extra };
    save();
    if (!ok) { o.err(`${tag} --to ${to} stopped at ${stage}: nothing after it ran`); return 1; }
  }
  o.out(`${tag} --to ${to} passed: ${stages.join(", ")}`);
  return 0;
}

/**
 * rightkit-gate stage --platform mac|win [--tier patch|update]: hands a qualified sealed set to the trusted publisher. The host signed,
 * sealed and qualified it (--to qualify); only the publisher holds the R2 write credential. Requires this host's receipt for HEAD with
 * package and qualify passed on the sealed bytes as they are now. Packs the sealed directory and the receipt into the draft GitHub
 * release `rightkit-staging-<commit>` (replacing that platform's asset), then, with --tier, dispatches RightKit's publish-app workflow
 * (the one trusted publisher for every app) for HEAD.
 */
const PUBLISHER_REPO = "bogusyogi/rightkit";
function stageForPublisher(o, args, root, git) {
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  const tag = "rightkit-gate:";
  const platform = option("--platform");
  const tier = option("--tier");
  if (!["mac", "win"].includes(platform)) { o.err(`${tag} stage needs --platform mac|win`); return 2; }
  if (tier !== undefined && !["patch", "update"].includes(tier)) { o.err(`${tag} stage --tier must be patch or update`); return 2; }
  const commit = text(git("rev-parse", "HEAD")).trim();
  const common = text(git("rev-parse", "--git-common-dir")).trim();
  if (!/^[0-9a-f]{40}$/.test(commit) || !common) { o.err(`${tag} cannot read HEAD and the git directory`); return 2; }
  const commonDir = path.resolve(root, common);
  const { appRoot, appDir } = appRootOf({ root, cwd: o.cwd, exists: o.exists });
  const receipt = (o.readReceipt ?? readReceipt)({ commonDir, commit, app: appDir });
  const pack = receipt?.stages?.package; const qual = receipt?.stages?.qualify;
  const sealed = pack?.ok ? (o.sealedFor ?? sealedFor)({ root: appRoot, commit, platform }) : null;
  if (!pack?.ok || !qual?.ok || pack.platform !== platform || qual.manifestSha256 !== pack.manifestSha256 || !sealed || sealed.id !== pack.sealed || sealed.digest !== pack.manifestSha256) {
    o.err(`${tag} BLOCKED: stage needs this host's receipt for ${commit.slice(0, 12)} with package and qualify passed on ${platform} and the sealed set unchanged; run rightkit-gate --to qualify --platform ${platform} first`);
    return 1;
  }
  // One draft per commit and app (receiptKey): two apps of one repository staged at one commit never clobber each other's assets.
  const releaseTag = `rightkit-staging-${receiptKey(commit, appDir)}`;
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "rightkit-stage-"));
  try {
    const receiptFile = path.join(work, `receipt-${platform}.json`);
    fs.writeFileSync(receiptFile, `${JSON.stringify(receipt, null, 2)}\n`);
    const archive = path.join(work, `sealed-${platform}.tar.gz`);
    const tar = o.spawn("tar", ["-czf", archive, "-C", appRoot, path.join(".right-release", "sealed", sealed.id, platform)]);
    if (failed(tar)) { o.err(`${tag} could not pack ${sealed.id}/${platform}`); return 1; }
    const gh = (...a) => o.spawn("gh", a, { cwd: root });
    if (failed(gh("release", "view", releaseTag))) {
      const made = gh("release", "create", releaseTag, "--draft", "--target", commit, "--title", `staging ${commit.slice(0, 12)}`, "--notes", "Sealed and qualified sets for the trusted publisher; not a release.");
      if (failed(made)) { o.err(`${tag} could not create the draft ${releaseTag}: ${text(made).trim()}`); return 1; }
    }
    const up = gh("release", "upload", releaseTag, archive, receiptFile, "--clobber");
    if (failed(up)) { o.err(`${tag} could not upload to ${releaseTag}: ${text(up).trim()}`); return 1; }
    o.out(`${tag} staged ${sealed.id}/${platform} (manifest ${sealed.digest.slice(0, 12)}) in draft ${releaseTag}`);
    if (tier) {
      // The trusted publisher is RightKit's publish-app workflow: app repositories are private and run no Actions.
      const slug = text(gh("repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner")).trim();
      const run = gh("workflow", "run", "publish-app.yml", "-R", PUBLISHER_REPO, "-f", `app=${slug}`, "-f", `app_dir=${appDir}`, "-f", `source_revision=${commit}`, "-f", `tier=${tier}`, "-f", `platforms=${platform}`);
      if (!slug || failed(run)) { o.err(`${tag} staged, but could not dispatch ${PUBLISHER_REPO} publish-app.yml: ${text(run).trim()}`); return 1; }
      o.out(`${tag} dispatched ${PUBLISHER_REPO} publish-app.yml for ${slug}@${commit.slice(0, 12)} (${platform}, ${tier}); the publisher re-runs class A, verifies the receipt and the bytes, then uploads`);
    }
    return 0;
  } finally { fs.rmSync(work, { recursive: true, force: true }); }
}

// ---- the stage-output ledger (stage-ledger.mjs; decision 2 Q1, rows 7.05/7.05b/7.07/7.08) ----------------------------------------

/** What the snapshot holds beyond the commit right now: { entries, problems } by this gate's own walk. */
async function walkDoorSnapshot(o, ds, root) {
  const snap = await import("../snapshot.mjs");
  const tracked = new Set((o.treeEntries ?? snap.treeEntries)(root, ds.commit).map((e) => e.path));
  const dependencyRoots = (o.dependencyDirs ?? snap.dependencyDirs)((o.treeEntries ?? snap.treeEntries)(root, ds.commit), o.platform);
  const { walkSnapshot } = await import("../stage-ledger.mjs");
  return (o.walkSnapshot ?? walkSnapshot)({ dir: ds.path, tracked, dependencyRoots, env: o.env, platform: o.platform });
}

/** `verifySnapshot` with the ledger (item 5 row 5.09d: `ledger` admits exactly these entries, `admitNew` only at a stage's end). */
async function verifyDoorSnapshot(o, ds, root, { ledger, admitNew = false }) {
  const verify = o.verifySnapshot ?? (await import("../snapshot.mjs")).verifySnapshot;
  return verify({ checkout: root, rev: ds.commit, root: ds.root, dir: ds.path, ledger, admitNew });
}

/**
 * The start of a stage: the snapshot is exactly tree + dependencies + the ledger of the stages before it, no entry new, changed or
 * missing, and the host's ownership is still held (7.08, decision 2 Q1). An ignored file planted before the stage is a new entry here.
 */
export async function openSnapshotStage({ o, ds, root, stage, tag = "rightkit-gate:" }) {
  if (!ds.own.held()) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] the build ownership ${ds.own.handle} is no longer held`); return false; }
  const { diffEntries, sameEntries } = await import("../stage-ledger.mjs");
  let walked;
  try { walked = await walkDoorSnapshot(o, ds, root); } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${e.message}`); return false; }
  const prior = ds.ledger?.entries ?? [];
  if (walked.problems.length) { for (const p of walked.problems.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [${stage}] before the stage: ${p}`); return false; }
  if (!sameEntries(prior, walked.entries)) {
    const d = diffEntries(prior, walked.entries);
    o.err(`${tag} RK_CHECK_ERROR: [${stage}] before the stage the snapshot differs from the ledger of the stages before it (new: ${d.added.join(", ") || "none"}; changed: ${d.changed.join(", ") || "none"}; missing: ${d.removed.join(", ") || "none"})`);
    return false;
  }
  try { await verifyDoorSnapshot(o, ds, root, { ledger: prior }); } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] before the stage: ${e.message}`); return false; }
  return true;
}

/**
 * The end of a stage: what its children wrote may remain only as ledger entries. A staged entry is immutable for the rest of the run
 * (the bytes the compile read are the bytes sealed); a generated entry may be rewritten or removed and is re-recorded here. Returns
 * `{ ok, entries }` and replaces the door's ledger with the entries just recorded.
 */
export async function closeSnapshotStage({ o, ds, root, stage, step = "stage", tag = "rightkit-gate:" }) {
  if (!ds.own.held()) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] the build ownership ${ds.own.handle} is no longer held`); return { ok: false, entries: ds.ledger?.entries ?? [] }; }
  const { entryKey } = await import("../stage-ledger.mjs");
  let walked;
  try { walked = await walkDoorSnapshot(o, ds, root); } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${e.message}`); return { ok: false, entries: ds.ledger?.entries ?? [] }; }
  const prior = new Map((ds.ledger?.entries ?? []).map((e) => [e.path, e]));
  const now = new Map(walked.entries.map((e) => [e.path, e]));
  const problems = [...walked.problems];
  for (const [p, e] of prior) if (e.kind === "staged" && (!now.has(p) || entryKey(now.get(p)) !== entryKey(e))) problems.push(`${p} is a staged sidecar output and ${now.has(p) ? "changed" : "was removed"} after it was staged (a staged entry is immutable for the door run)`);
  if (problems.length) { for (const p of problems.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${p}`); return { ok: false, entries: ds.ledger?.entries ?? [] }; }
  // The compile's own verify (snapshot-check.mjs, 5.09d) returned the entries it admitted: they must be the entries this walk found.
  if (Array.isArray(ds.compileAdmitted) && stage === "compile") {
    const { admittedMismatch } = await import("../stage-ledger.mjs");
    const bad = admittedMismatch(ds.compileAdmitted, walked.entries);
    if (bad) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] the compile's snapshot verify admitted other ledger entries than the door's own walk recorded: ${bad}`); return { ok: false, entries: ds.ledger?.entries ?? [] }; }
  }
  ds.compileAdmitted = null;
  const entries = walked.entries.map((e) => {
    const was = prior.get(e.path);
    if (was?.kind === "staged") return { ...e, kind: "staged", producer: was.producer };
    return { ...e, kind: "generated", producer: was && entryKey(was) === entryKey(e) ? was.producer : { stage, step } };
  });
  try {
    const verified = await verifyDoorSnapshot(o, ds, root, { ledger: entries, admitNew: true });
    // The verify side (item 5 row 5.09d) returns the entries it admitted; if it does, they must be the ones this walk recorded.
    if (Array.isArray(verified?.ledger)) {
      const { admittedMismatch } = await import("../stage-ledger.mjs");
      const bad = admittedMismatch(verified.ledger, entries);
      if (bad) throw new Error(`the snapshot verify admitted other ledger entries than the door's own walk recorded: ${bad}`);
    }
  } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${e.message}`); return { ok: false, entries: ds.ledger?.entries ?? [] }; }
  ds.ledger = { entries };
  return { ok: true, entries };
}

/**
 * Sidecars are staged ONCE per door run, after the sync and before the first stage that builds the Tauri crate. The step is
 * right-release's own `stage-sidecar --json` run inside the snapshot (the split); this gate then walks the snapshot itself and the
 * step's additions must equal the files it reported plus its receipt (anything else is an unattributed write, RK_CHECK_ERROR). The
 * staged files become immutable `staged` ledger entries. Returns true when nothing is owed or staging passed.
 */
export async function stageSidecarsOnce({ o, ds, root, git = null, configs, appDir, cli, commonDir, commit, tree, stage, executor = null, tag = "rightkit-gate:" }) {
  if (ds.sidecarsStaged) return true;
  ds.sidecarsStaged = true;
  if (!configs.length) return true;
  const { attributionProblems, reportedPaths } = await import("../stage-ledger.mjs");
  const source = sourceSplitOf({ root, ds, appDir });
  const { TAURI_CONFIG: inherited, ...cleanEnv } = o.env; void inherited; // never set or pass one (refused at the door's start)
  for (const config of configs) {
    const before = ds.ledger?.entries ?? [];
    // The stage-sidecar child's Cargo calls (its builds and metadata queries) are carried on a broker host (carried-cargo.mjs).
    const carrier = await carrierForChildren({ o, ds, root, git, executor, stage, tag });
    if (carrier.refusal) { o.err(`${tag} BLOCKED: ${carrier.refusal}; nothing was staged`); return false; }
    const run = startRun({ commonDir, commit, tree, stage, source });
    let r;
    try {
      o.out(`${tag} [${stage}] right-release stage-sidecar --config ${path.join(ds.path, config)} (from the snapshot ${ds.path})`);
      r = (o.spawnCapture ?? ((cmd, a, opts) => spawnInherit(cmd, a, { ...opts, encoding: "utf8", maxBuffer: 1 << 26 })))(process.execPath, [cli, "stage-sidecar", "--config", path.join(ds.path, config), "--json"], { cwd: snapshotAppRootOf(ds.path, appDir), env: carriedEnv(carrier, { ...cleanEnv, ...run.env }) });
    } finally { endRun(run); }
    const carriedOk = await carrier.finish({ stage });
    if (r?.stderr) o.err(String(r.stderr).trimEnd());
    if (!r || r.status !== 0) { o.err(`${tag} [${stage}] stage-sidecar for ${config} failed${r?.error ? ` (${r.error.message})` : ""}`); return false; }
    if (!carriedOk) return false;
    let result;
    try { result = JSON.parse(String(r.stdout ?? "").trim()); } catch { o.err(`${tag} RK_CHECK_ERROR: [${stage}] stage-sidecar --json for ${config} did not print its result; its writes cannot be attributed`); return false; }
    let walked;
    try { walked = await walkDoorSnapshot(o, ds, root); } catch (e) { o.err(`${tag} RK_CHECK_ERROR: [${stage}] ${e.message}`); return false; }
    const underApp = (rel) => (appDir === "." ? rel : `${appDir}/${rel}`).replaceAll("\\", "/");
    const receiptAbs = typeof result.receiptPath === "string" ? path.relative(ds.path, result.receiptPath).split(path.sep).join("/") : null;
    const problems = [...walked.problems, ...attributionProblems({ before, after: walked.entries, reported: [...reportedPaths(result)].map(underApp), receipt: receiptAbs })];
    if (problems.length) { for (const p of problems.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [${stage}] stage-sidecar ${config}: ${p}`); return false; }
    const priorPaths = new Set(before.map((e) => e.path));
    const priorByPath = new Map(before.map((e) => [e.path, e]));
    ds.ledger = { entries: walked.entries.map((e) => (priorPaths.has(e.path) && priorByPath.get(e.path).sha256 === e.sha256 ? priorByPath.get(e.path) : { ...e, kind: "staged", producer: { stage: "sidecar", step: config } })) };
  }
  return true;
}

/**
 * The Cargo binding of a unit stage built from a snapshot (row 7.06a; decision 2 Q3, as for any npm unit): a unit's test or pack that
 * reaches Cargo must never write `target/` into the snapshot. On a broker host the workspaces are read from the commit's own manifests
 * (git objects, never the checkout: the broker pins every output). On a direct host every tracked Cargo.toml of the commit is asked, in the
 * CHECKOUT, for its workspace and `target_directory` (a direct host has no broker to pin it; Linux compiles directly, so this one reads
 * the checkout's manifests and its Cargo configuration for where the output goes). Linux and any POSIX host with a direct executor put the runtime wrapper (cargo-target-shim.mjs)
 * first on PATH and name it in CARGO, so each cargo call binds its own workspace's checkout target or is refused. A broker host
 * (`o.snapshotBroker`) uses the same wrapper in broker mode, one carried request per call with its own receipt. A direct Windows host has
 * no wrapper: one workspace is pinned by CARGO_TARGET_DIR, several are refused. Returns `{ env, note, cleanup }` or `{ refusal }`.
 */
async function bindUnitCargo({ o, git, root, ds, executor, which }) {
  const listed = git("ls-tree", "-r", "--name-only", "-z", ds.commit);
  if (failed(listed)) return { refusal: `cannot list the files of ${ds.commit.slice(0, 12)}: ${why(listed)}` };
  const manifests = text(listed).split("\0").filter((f) => f === "Cargo.toml" || f.endsWith("/Cargo.toml"));
  if (!manifests.length) return { env: {}, note: null, cleanup() {} };
  let checkoutReal = root; try { checkoutReal = fs.realpathSync.native(root); } catch { /* as given */ }
  const cargo = executor.cargo ?? ["cargo"];
  const targetDirs = {};
  if (executor.kind === "broker") {
    // A broker host never asks Cargo anything about the checkout: the broker pins every output directory (the receipt's `outputDir`), so
    // all this binding needs is which directories of the commit are Cargo workspaces, and the commit's own manifests say so (read with the
    // object database, never the editable checkout: GATE-CONTRACT "Commit-only build"). The same rule the carried seam applies to a call
    // (cargo-target-shim.mjs locateWorkspaceByWalk): the nearest enclosing manifest that declares `[workspace]`, else the nearest manifest.
    const tracked = new Set(manifests);
    const bodies = new Map();
    const declaresWorkspace = (m) => {
      if (!bodies.has(m)) { const r = git("show", `${ds.commit}:${m}`); bodies.set(m, failed(r) ? null : text(r)); }
      return bodies.get(m);
    };
    for (const m of manifests) {
      let dir = path.posix.dirname(m); let nearest = null; let found = null;
      for (;;) {
        const candidate = dir === "." ? "Cargo.toml" : `${dir}/Cargo.toml`;
        if (tracked.has(candidate)) {
          nearest ??= dir;
          const body = declaresWorkspace(candidate);
          if (body === null) return { refusal: `cannot read ${candidate} of ${ds.commit.slice(0, 12)}` };
          if (/^\s*\[workspace(?:\.[^\]]*)?\]/m.test(body)) { found = dir; break; }
        }
        if (dir === ".") break;
        dir = path.posix.dirname(dir);
      }
      targetDirs[found ?? nearest ?? path.posix.dirname(m)] = null;
    }
  }
  for (const m of executor.kind === "broker" ? [] : manifests) {
    const file = path.join(root, ...m.split("/"));
    if (!o.exists(file)) return { refusal: `the checkout has no ${m}, so its Cargo output directory cannot be resolved` };
    const r = o.spawn(cargo[0], [...cargo.slice(1), "metadata", "--offline", "--format-version", "1", "--no-deps", "--manifest-path", file], { cwd: path.dirname(file), env: o.env });
    let doc = null;
    try { doc = failed(r) ? null : JSON.parse(text(r)); } catch { doc = null; }
    if (!doc || typeof doc.target_directory !== "string" || !path.isAbsolute(doc.target_directory) || typeof doc.workspace_root !== "string") return { refusal: `cargo metadata in the checkout reported no absolute target_directory for ${m}${failed(r) ? ` (${why(r)})` : ""}` };
    const cargoDir = posix(path.relative(checkoutReal, (() => { try { return fs.realpathSync.native(doc.workspace_root); } catch { return doc.workspace_root; } })())) || ".";
    targetDirs[cargoDir] = path.resolve(doc.target_directory);
  }
  const workspaces = Object.keys(targetDirs);
  if (o.platform === "win32" && executor.kind !== "broker") {
    if (workspaces.length > 1) return { refusal: `the commit holds ${workspaces.length} Cargo workspaces (${workspaces.join(", ")}) and a direct Windows host has no per-call wrapper to bind each to its own target directory` };
    return { env: { CARGO_TARGET_DIR: Object.values(targetDirs)[0] }, note: `output directory for the Cargo workspace: ${Object.values(targetDirs)[0]} (the checkout's)`, cleanup() {} };
  }
  const shimModule = await import("../cargo-target-shim.mjs");
  let broker = null; let realCargo = null;
  if (executor.kind === "broker") {
    if (!o.snapshotBroker) return { refusal: "a broker host reaches Cargo from a unit stage only through the snapshot carrier (deps.snapshotBroker), which this gate was started without" };
    let sb;
    try { sb = await carrierBroker({ o, root, ds, which }); } catch (error) { return { refusal: `the snapshot carrier cannot be built (${error.code ?? "error"}: ${error.message})` }; }
    const marked = sb.markBrokerSubmit?.();
    if (marked?.broker?.submitted !== true || marked.handle !== sb.handle) return { refusal: `the build ownership did not record the broker submit${sb.handle ? ` under ${sb.handle}` : ""}` };
    const { brokerSection } = await import("../snapshot-broker.mjs");
    broker = { checkoutDirs: Object.fromEntries(workspaces.map((w) => [w, path.join(checkoutReal, ...(w === "." ? [] : w.split("/")))])), section: (o.brokerSection ?? brokerSection)(sb) };
  } else {
    realCargo = which("cargo").find((c) => path.isAbsolute(c));
    if (!realCargo) return { refusal: "no cargo on PATH to wrap for the unit stages" };
  }
  let installed; let dir = null;
  try { installed = (o.installCargoShim ?? shimModule.installCargoShim)({ base: os.tmpdir(), snapshotRoot: ds.path, targetDirs, broker, realCargo, platform: o.platform, onDir: (d) => { dir = d; } }); }
  catch (error) { if (dir) fs.rmSync(dir, { recursive: true, force: true }); return { refusal: `the runtime cargo wrapper cannot be installed (${error.message})` }; }
  const scan = shimModule.shimScanEnv(o.env, installed);
  const delta = Object.fromEntries(Object.entries(scan).filter(([k, v]) => o.env[k] !== v));
  const { CARRIED_CARGO_MAP_ENV } = await import("../carried-cargo.mjs");
  return {
    env: { ...delta, ...(installed.mode === "broker" ? { [CARRIED_CARGO_MAP_ENV]: installed.mapFile } : {}) }, note: `runtime cargo wrapper ${installed.shim} (${installed.mode}): each cargo call a unit stage reaches binds its workspace's checkout target directory or is refused`,
    // Every call the wrapper decided is in the gate-owned ledger: a refused or unverified call fails the stage even when the test ignored cargo's exit (7.10, decision C).
    check: async () => { const { readLedger, UNSCOPED } = await import("../cargo-ledger.mjs"); return readLedger(installed.ledgerDir, UNSCOPED).problems ?? []; },
    cleanup: () => { fs.rmSync(installed.dir ?? dir, { recursive: true, force: true }); },
  };
}

/**
 * `--to <stage> --unit <selector> [--commit <rev>]` on the snapshot route (row 7.06a): select a commit (HEAD by default), take the
 * host's build ownership, sync its snapshot, run class A of the snapshot (`check --commit <sha> --full-a --snapshot <dir>`), and build
 * the unit from the snapshot (`--workspace <snapshot>`). Commit and tree come from the selection; no HEAD or status is read. The unit
 * manifest and the local-build grant are read from the selected commit, so one present only in the checkout is refused. Each stage is
 * bracketed by the ledger verify (7.08); Cargo's output is pinned to the checkout's target directory (7.09); `--to release` keeps its
 * publication path and takes only commit and tree from the selection.
 */
async function runUnitSnapshot(o, args, root, git, io, option) {
  const tag = "rightkit-gate:";
  const selector = option("--unit");
  if (o.env.TAURI_CONFIG !== undefined) { o.err(`${tag} BLOCKED: RK_TAURI_CONFIG_OVERRIDE: TAURI_CONFIG is set in the environment; a commit-only build refuses it; nothing ran`); return 1; }
  const match = /^(?:npm:@rightkit\/([a-z0-9-]+)|cargo:(rightkit-[a-z0-9-]+))$/.exec(selector);
  const manifest = match && (match[1] ? `packages/${match[1]}/package.json` : `crates/${match[2]}/Cargo.toml`);
  if (!manifest) { o.err(`${tag} BLOCKED: unit needs npm:@rightkit/<name> or cargo:rightkit-<name>`); return 1; }
  const snap = await import("../snapshot.mjs");
  const { acquireBuildOwnership, ownershipFile } = await import("../build-ownership.mjs");
  let selected;
  try { selected = snap.resolveCommit(root, option("--commit") ?? "HEAD"); }
  catch (e) { o.err(`${tag} cannot resolve ${option("--commit") ?? "HEAD"} to a commit: ${e.message}`); return 2; }
  const commit = selected.commit;
  // The canonical manifest and the local-build grant must be tracked in the SELECTED COMMIT (read with the object database, never the checkout).
  for (const file of [".rightkit-local-build.json", manifest]) if (failed(git("cat-file", "-e", `${commit}:${file}`))) { o.err(`${tag} BLOCKED: unit and local grant must be tracked canonical files of ${commit.slice(0, 12)} (${file} is not)`); return 1; }
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
  if (executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
  { const decided = routePlan(o, { to: option("--to"), stages: [], devTest: true, executor, capabilities: brokerCapabilityState(o, { to: option("--to"), executor }) });
    if (decided.refusal?.code === "RK_BROKER_CAPABILITY_MISSING") { o.err(`${tag} BLOCKED: ${decided.refusal.code}: ${decided.refusal.message}; nothing ran`); return 1; } }
  if (executor.kind === "broker" && !o.snapshotBroker) { o.err(`${tag} BLOCKED: RK_SNAPSHOT_CARRIER_NOT_WIRED: a broker host builds a unit snapshot only through the snapshot carrier; nothing ran`); return 1; }
  const common = git("rev-parse", "--git-common-dir");
  if (failed(common) || !text(common).trim()) { o.err(`${tag} cannot read the git directory`); return 2; }
  const snapshotsAt = o.snapshotRoot ?? snap.snapshotRoot({ platform: o.platform, env: o.env });
  let own;
  try { own = (o.acquireOwnership ?? acquireBuildOwnership)({ file: ownershipFile({ platform: o.platform, beside: o.snapshotRoot }), stage: `door:unit:${option("--to")}`, snapshot: path.join(snapshotsAt, snap.repositoryOf(root).key), commit, platform: o.platform, env: o.env }); }
  catch (e) { if (e.code !== "busy" && e.code !== "refused") throw e; o.err(`${tag} BLOCKED: ${e.message}; nothing ran${e.code === "refused" ? ", nothing created" : ""}`); return 1; }
  o.out(`${tag} build ownership ${own.joined ? "joined" : "held"} ${own.handle}`);
  try {
    let synced;
    try { synced = (o.syncSnapshot ?? snap.syncSnapshot)({ checkout: root, rev: commit, root: snapshotsAt }); }
    catch (e) { o.err(`${tag} ${e.code === "error" ? "RK_CHECK_ERROR" : "BLOCKED"}: snapshot ${e.message}; nothing ran`); return e.code === "error" ? 2 : 1; }
    o.out(`${tag} snapshot ${synced.path} is commit ${synced.commit} tree ${synced.tree} (written=${synced.written} removed=${synced.removed} unchanged=${synced.unchanged})`);
    const ds = { own, path: synced.path, commit: synced.commit, tree: synced.tree, root: snapshotsAt, ledger: { entries: [] } };
    // A unit's test or pack that reaches Cargo must never write `target/` into the snapshot (npm units included): see bindUnitCargo.
    const bound = await bindUnitCargo({ o, git, root, ds, executor, which });
    if (bound.refusal) { o.err(`${tag} BLOCKED: RK_SNAPSHOT_OUTPUT_UNPINNED: ${bound.refusal}; nothing ran`); return 1; }
    if (bound.note) o.out(`${tag} ${bound.note}`);
    const envExtra = bound.env;
    try {
    let checkout = root; try { checkout = fs.realpathSync.native(root); } catch { /* as given */ }
    const source = { checkout, commit: ds.commit, tree: ds.tree };
    const reportOut = option("--receipt-out");
    const { runUnitGate, resolveUnit } = await import("../unit-gate.mjs");
    const { localBuildGrantedAt } = await import("../local-publish.mjs");
    return await runUnitGate({ selector, to: option("--to"), root: ds.path, commonDir: path.resolve(root, text(common).trim()), commit: ds.commit, tree: ds.tree, executor, env: o.env, platform: o.platform, source, envExtra,
      resolve: (sel, r) => resolveUnit(sel, r, { granted: () => localBuildGrantedAt(source) }),
      ...(o.loadUnitOwner ? { loadOwner: o.loadUnitOwner } : {}),
      ...(o.unitGateDeps ?? {}), // test seam: fetchReceipt, stageCommands, verifyReceipts, selfGit
      stageGuard: { start: (stage) => openSnapshotStage({ o, ds, root, stage: `unit:${stage}`, tag }), end: async (stage) => {
        if (!(await closeSnapshotStage({ o, ds, root, stage: `unit:${stage}`, step: `unit:${stage}`, tag })).ok) return false;
        const calls = bound.check ? await bound.check() : [];
        for (const p of calls.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [unit:${stage}] a cargo call the unit stage made did not complete: ${typeof p === "string" ? p : JSON.stringify(p)}`);
        return calls.length === 0;
      } },
      check: (tier) => (o.runCheck ?? (() => main(["check", "--commit", ds.commit, "--full-a", "--snapshot", ds.path, "--rule-tier", tier], { ...io, cwd: root, buildOwnership: own })))(tier), out: o.out, err: o.err,
      ...(reportOut ? { writeReport: (report) => o.writeFile(path.resolve(o.cwd, reportOut), `${JSON.stringify(report, null, 2)}\n`) } : {}),
    });
    } finally { bound.cleanup(); }
  } finally {
    const r = own.release();
    if (r?.released === false && !own.joined) o.err(`${tag} build ownership ${own.handle} kept: ${r.why}`);
  }
}

/**
 * Whether the door takes the snapshot route on this host: every host but hosted CI, whose runner checkout is the commit. Linux
 * compiles it directly; Mac and Dell compile it through the RustRight snapshot carrier. There is no other route and no flag or
 * environment variable that selects one (rightkit#1301: the checkout route is deleted).
 */
export function snapshotRouteSelected(o) {
  return o.env?.GITHUB_ACTIONS !== "true";
}

/** The app's root inside the snapshot: `<snapshot>/<appDir>`, the snapshot itself for an app at the repository root. */
export function snapshotAppRootOf(snapshotDir, appDir = ".") {
  return appDir === "." ? snapshotDir : path.join(snapshotDir, ...appDir.split("/"));
}

/** What the door run records for a right-release child (source-split.mjs): the checkout it ran in and the snapshot it may read. */
function sourceSplitOf({ root, ds, appDir }) {
  let checkout = root;
  try { checkout = fs.realpathSync.native(root); } catch { /* the path as given */ }
  return { checkout, snapshot: ds.path, commit: ds.commit, tree: ds.tree, appDir };
}

/**
 * Why the snapshot route cannot start on this host, or null. The route is the executor's: `direct` hosts (Linux always, any host
 * with no broker) take it as they are. A broker host compiles the snapshot only through the RustRight snapshot carrier, which the
 * gate reaches through `deps.snapshotBroker` (production wires it on Mac and Dell: `productionEntryDeps`); a gate started without
 * it is refused (RK_SNAPSHOT_CARRIER_NOT_WIRED: tests only, never production), and a broker that cannot be reached is refused too
 * (RK_SNAPSHOT_BROKER_UNAVAILABLE). Neither ever falls back to compiling the checkout.
 */
export function snapshotRouteRefusal(o) {
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile });
  if (executor.kind === "refuse") return { code: "RK_SNAPSHOT_BROKER_UNAVAILABLE", message: executor.problem };
  if (executor.kind === "broker" && !o.snapshotBroker) return { code: "RK_SNAPSHOT_CARRIER_NOT_WIRED", message: `a ${o.platform} host compiles a commit snapshot only through the RustRight snapshot carrier (item 7 rows 7.20 to 7.24), and this gate was started without it` };
  return null;
}

/** The refusal codes a route probe can answer with: the codes the stages themselves print (`BLOCKED: <code>: ...`). */
export const ROUTE_REFUSAL_CODES = Object.freeze(["RK_TAURI_CONFIG_OVERRIDE", "RK_SNAPSHOT_BROKER_UNAVAILABLE", "RK_SNAPSHOT_CARRIER_NOT_WIRED", "RK_BROKER_CAPABILITY_MISSING", "RK_BROKER_UNAVAILABLE"]);
/** The stages `--route` can answer for (`release` writes to the publisher and has no route of its own on a host). */
export const ROUTE_PROBE_STAGES = Object.freeze(["check", "compile", "package", "qualify", "dev", "test"]);
export const ROUTE_SCHEMA = "rightkit-gate.route/1";

/**
 * The capabilities a broker must advertise for the carried stages (rightkit#1301 B2). RustRight reports them in `rustright doctor --json`
 * as `.broker.capabilities` (the HANDSHAKE's `capabilities`: `journal-v1`, `snapshot-readonly-v1`, `snapshot-adapter-v1`, ...; constants
 * in rustright-protocol response.rs). The two required strings ship with RustRight's upcoming change: today's broker (78eda7e) reports no
 * `.broker.capabilities`, so this release refuses every stage that builds until that broker is installed. `snapshot-adapter-v1`
 * (rustright#190, rightkit#1301 B10) is the broker's acceptance of a snapshot-bound adapter job (pnpm, node, QA) inside a gate snapshot,
 * under which a Tauri CLI's compiles run nested: without it every pnpm step of a snapshot stage (an app's dev and package steps, an npm
 * unit's compile and package) is refused by the broker, so the gate refuses first. `stage-grant-v1` is no longer required: the gate mints
 * no stage grant (one model: every compile of a snapshot stage is a carried request or nested under one).
 */
export const REQUIRED_BROKER_CAPABILITIES = Object.freeze(["snapshot-readonly-v1", "snapshot-adapter-v1"]);

/**
 * The broker's capabilities, asked without a build through `rustright doctor --json`: `{ ok: true, capabilities }` or `{ ok: false, problem }`.
 * A missing client, a client that cannot run, a hung (20 s), unparsable or field-less answer is `ok: false`, which the route refuses (fail
 * closed). A doctor that reports an unhealthy host still prints its JSON: the field decides, not the exit status.
 */
export function queryBrokerCapabilities(o) {
  const client = whichIn(o)("rustright").find((c) => path.isAbsolute(c));
  if (!client) return { ok: false, problem: "the RustRight client is not on PATH" };
  const r = o.spawn(client, ["doctor", "--json"], { env: o.env, encoding: "utf8", timeout: 20_000, windowsHide: true });
  if (r?.error || r?.status === null || r?.status === undefined) return { ok: false, problem: `\`rustright doctor --json\` did not finish: ${why(r)}` };
  let doc;
  try { doc = JSON.parse(text(r)); } catch { return { ok: false, problem: `\`rustright doctor --json\` printed no JSON${r.status ? ` (exit ${r.status})` : ""}` }; }
  const caps = doc?.broker?.capabilities;
  if (!Array.isArray(caps) || !caps.every((c) => typeof c === "string")) return { ok: false, problem: "`rustright doctor --json` reports no `.broker.capabilities` (a broker that predates snapshot-readonly-v1 and snapshot-adapter-v1)" };
  return { ok: true, capabilities: caps };
}

/**
 * What a route decision needs of the broker: `null` when nothing is asked (not a broker executor, hosted CI, `--to check`, or a gate
 * whose broker is an injected fixture without a declared capability query), else the answer. The production carrier asks the real
 * client; a test's injected broker declares its own through `o.brokerCapabilities`.
 */
export function brokerCapabilityState(o, { to, executor }) {
  if (executor?.kind !== "broker" || to === "check" || o.env.GITHUB_ACTIONS === "true") return null;
  if (typeof o.brokerCapabilities === "function") return o.brokerCapabilities(o);
  return typeof o.snapshotBroker?.create === "function" ? queryBrokerCapabilities(o) : null;
}

/**
 * Which route this host's door takes for `--to <stage>` and whether it refuses, WITHOUT running anything: the decisions of `runStages`
 * in the order it makes them. `runStages` takes its own pre-ownership route and refusal from this very function (rightkit#1297 L1), so
 * the probe and a real run cannot drift (the door-route parity test compares them over the carrier, executor, platform and Tauri
 * combinations):
 *   1. hosted CI (GITHUB_ACTIONS) builds the runner's checkout, which is the commit: no snapshot route;
 *   2. every other host takes the snapshot route (Linux direct; Mac and Dell through the carrier; there is no checkout route);
 *   3. the snapshot route refuses an inherited TAURI_CONFIG;
 *   4. then `snapshotRouteRefusal` (skipped for `--to check`, which compiles nothing): an unreachable broker, a gate without its carrier;
 *   5. then an executor that cannot start (a broker-managed host whose broker is unreachable), on any route.
 * `executor` is a resolved executor. Returns { route, refusal } with
 * refusal null or { code, stage, message }. Reads no checkout, takes no ownership, syncs no snapshot and spawns nothing itself.
 */
export function routePlan(o, { to, stages, devTest, executor, capabilities = null }) {
  const refuse = (code, stage, message) => ({ code, stage, message });
  const hosted = o.env.GITHUB_ACTIONS === "true";
  const snapshot = !hosted && snapshotRouteSelected(o);
  const route = hosted ? "hosted" : "snapshot";
  if (snapshot) {
    if (o.env.TAURI_CONFIG !== undefined) return { route, refusal: refuse("RK_TAURI_CONFIG_OVERRIDE", to, "TAURI_CONFIG is set in the environment; it would replace the commit's Tauri configuration (externalBin included) for every build below, so a commit-only build refuses it") };
    const refused = to === "check" ? null : snapshotRouteRefusal(o);
    if (refused) return { route, refusal: refuse(refused.code, to, refused.message) };
    // The broker must accept what the carried stages ask of it (rightkit#1301 B2): a missing or failing capability query refuses.
    if (capabilities && executor?.kind === "broker" && to !== "check") {
      const missing = capabilities.ok ? REQUIRED_BROKER_CAPABILITIES.filter((c) => !capabilities.capabilities.includes(c)) : REQUIRED_BROKER_CAPABILITIES;
      if (!capabilities.ok || missing.length) return { route, refusal: refuse("RK_BROKER_CAPABILITY_MISSING", to, `the broker ${capabilities.ok ? `does not advertise ${missing.join(", ")}` : `cannot be asked for its capabilities (${capabilities.problem})`} (\`rustright doctor --json\` \`.broker.capabilities\`); this release builds only against a broker that accepts snapshot-bound read-only Cargo and snapshot-bound adapter jobs (rightkit#1301, rustright#188, rustright#190)`) };
    }
  }
  if (executor?.kind === "refuse") return { route, refusal: refuse("RK_BROKER_UNAVAILABLE", to, executor.problem) };
  return { route, refusal: null };
}

/**
 * `rightkit-gate --to <stage> --route [--platform p] [--commit rev]`: prints one JSON line (`rightkit-gate.route/1`) naming the route
 * the stage would take and the refusal it would meet, and runs nothing: no ownership, no snapshot sync, no check, no run record, no
 * receipt, no right-release. Exit 0 when it answered (a refusal is an answer), 2 when it could not compute one.
 */
async function runRouteProbe(o, args, root, git, { to, platform, stages, devTest, appDir }) {
  const tag = "rightkit-gate: --route:";
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  const snap = await import("../snapshot.mjs");
  let selected;
  try { selected = snap.resolveCommit(root, option("--commit") ?? "HEAD"); }
  catch (e) { o.err(`${tag} cannot resolve ${option("--commit") ?? "HEAD"} to a commit: ${e.message}`); return 2; }
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile });
  let plan;
  const capabilities = brokerCapabilityState(o, { to, executor });
  try { plan = routePlan(o, { to, stages, devTest, executor, capabilities }); }
  catch (e) { o.err(`${tag} ${e.message}`); return 2; }
  // Read-only: GIT_OPTIONAL_LOCKS=0, so the probe's `git status` never refreshes the index or contends with a writer (rightkit#1297 L4).
  const dirty = o.spawn("git", ["status", "--porcelain", "--untracked-files=no"], { cwd: root, env: { ...o.env, GIT_OPTIONAL_LOCKS: "0" } });
  const version = JSON.parse(fs.readFileSync(new URL("../package.json", import.meta.url), "utf8")).version;
  o.out(JSON.stringify({
    schema: ROUTE_SCHEMA, gate: version, to, platform: platform ?? null, host: o.platform, app: appDir, commit: selected.commit,
    executor: executor.kind, carrier: o.snapshotBroker ? (o.snapshotBroker.kind ?? "injected") : null, route: plan.route,
    brokerCapabilities: capabilities?.ok ? capabilities.capabilities : null, stages: devTest ? ["check", to] : stages, refusal: plan.refusal, checkoutClean: failed(dirty) ? null : !text(dirty).trim(),
  }));
  return 0;
}

// ---- the carrier of a snapshot stage's right-release children (carried-cargo.mjs; rightkit#1301) --------------------------------------

/**
 * The environment of a carried stage's child: the base, the stage's carrier and the caller's extras, without an output directory.
 * The broker pins every Cargo output, so the Tauri CLI and every other child must not look for bundles in a directory the environment
 * names (rightkit#1301 F5): CARGO_TARGET_DIR and CARGO_BUILD_TARGET_DIR are dropped, in any case.
 */
export function carriedEnv(carrier, base, extra = {}) {
  if (!carrier.carried) return { ...base, ...carrier.env, ...extra };
  const strip = (env) => {
    const out = { ...env };
    for (const key of Object.keys(out)) {
      if (/^(CARGO_TARGET_DIR|CARGO_BUILD_TARGET_DIR)$/i.test(key)) delete out[key];
    }
    return out;
  };
  return { ...strip(base), ...carrier.env, ...strip(extra) };
}

const whichIn = (o) => (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };

/**
 * The gate's snapshot broker for this door run: `o.snapshotBroker` is the production carrier statement (`{ kind, create }`, which
 * builds the broker once: the host's enrolled pins, the declared inputs read from the snapshot, the build ownership and the RustRight
 * client) or, in a test, a ready broker object. Built once per door run.
 */
async function carrierBroker({ o, root, ds, which = whichIn(o) }) {
  if (ds.carrierBroker) return ds.carrierBroker;
  const carrier = o.snapshotBroker;
  let sb = carrier;
  if (typeof carrier?.create === "function") {
    const client = which("rustright").find((c) => path.isAbsolute(c));
    sb = carrier.create({ env: o.env, platform: o.platform, checkout: root, commit: { sha: ds.commit, tree: ds.tree }, snapshot: { path: ds.path, expectedTree: ds.tree }, ownership: ds.own, client, gateFile: callingGatePath(), spawn: o.spawn });
  }
  ds.carrierBroker = sb;
  return sb;
}

/** The commit's Cargo workspaces in the snapshot, each with its counterpart in the primary checkout: `{ [cargoDir]: checkout path }`, from the files alone. */
async function snapshotCargoWorkspaces({ git, ds, root }) {
  const listed = git("ls-tree", "-r", "--name-only", "-z", ds.commit);
  if (failed(listed)) throw new Error(`cannot list the files of ${ds.commit.slice(0, 12)}: ${why(listed)}`);
  const manifests = text(listed).split("\0").filter((f) => f === "Cargo.toml" || f.endsWith("/Cargo.toml"));
  const { locateWorkspaceByWalk } = await import("../cargo-target-shim.mjs");
  let checkout = root; try { checkout = fs.realpathSync.native(root); } catch { /* as given */ }
  const dirs = {};
  for (const m of manifests) {
    const file = path.join(ds.path, ...m.split("/"));
    let rel;
    try { rel = posix(path.relative(ds.path, path.dirname(locateWorkspaceByWalk(file, path.dirname(file))))) || "."; } catch { rel = posix(path.dirname(m)); }
    if (rel === ".." || rel.startsWith("../")) rel = posix(path.dirname(m)) || ".";
    dirs[rel] = path.join(checkout, ...(rel === "." ? [] : rel.split("/")));
  }
  return dirs;
}

/**
 * The carrier of one snapshot stage's right-release child on a broker host: the stage's workspaces, the broker section, one map and
 * ledger. Returns `{ env, refusal, finish({ stage }), cleanup }`: `env` is added to the
 * child's environment; `finish` runs after the child ended, fails on any call that was refused, unverified or unfinished and on any
 * receipt the gate cannot verify again itself, and removes the carrier. A direct executor, or no snapshot, has nothing to carry.
 */
async function carrierForChildren({ o, ds, root, git, executor, stage, tag = "rightkit-gate:" }) {
  const none = { env: {}, carried: false, refusal: null, async finish() { return true; }, cleanup() {} };
  if (!ds || executor?.kind !== "broker") return none;
  let base = null;
  try {
    if (!git) throw new Error("the gate's git reader is missing");
    const carried = await import("../carried-cargo.mjs");
    const { brokerSection } = await import("../snapshot-broker.mjs");
    const sb = await carrierBroker({ o, root, ds });
    // Broker work may exist from here on: the ownership records it first.
    const marked = sb.markBrokerSubmit?.();
    if (marked?.broker?.submitted !== true || marked.handle !== sb.handle) throw new Error(`the build ownership did not record the broker submit${sb.handle ? ` under ${sb.handle}` : ""}`);
    const checkoutDirs = await snapshotCargoWorkspaces({ git, ds, root });
    base = fs.mkdtempSync(path.join(os.tmpdir(), "rk-carrier-"));
    // The carrier is the map and the ledger on every platform: no PATH wrapper and no stage grant. The child's pnpm, node and QA steps are carried
    // adapter jobs and its Cargo calls carried requests; whatever the Tauri CLI compiles runs nested under the adapter job that started it, and the
    // broker's signed list of those children is what the gate verifies and reads the bundle root from (carried-cargo.mjs).
    const installed = (o.installCarrier ?? carried.installCarrier)({ base, snapshotRoot: ds.path, checkoutDirs, section: (o.brokerSection ?? brokerSection)(sb), known: Object.fromEntries(ds.carriedOutputs ?? []), platform: o.platform, env: o.env });
    o.out(`${tag} [${stage}] carrier (${installed.mode}): ${Object.keys(checkoutDirs).length} workspace(s); each right-release cargo call and each pnpm, node or QA step is one carried request with its own signed receipt, and a compile a step's child makes is nested under it`);
    let finished = false;
    const cleanup = () => { try { installed.cleanup?.(); } catch { /* removed below */ } fs.rmSync(base, { recursive: true, force: true }); };
    return {
      env: installed.env, carried: true, refusal: null, cleanup,
      async finish({ stage: at = stage } = {}) {
        if (finished) return true;
        finished = true;
        let problems;
        try {
          problems = await installed.problems(sb);
          // The output directories the stage's verified receipts name: the package stage's sealed manifest must record only these.
          for (const dir of installed.outputDirs?.() ?? []) ((ds.carriedByStage ??= {})[at] ??= new Set()).add(dir);
          // And by workspace, for the next stage's children: a receipt already names the output directory, so none asks Cargo for it.
          for (const [root, dir] of installed.outputsByWorkspace?.() ?? []) (ds.carriedOutputs ??= new Map()).set(root, dir);
        } catch (error) { problems = [`the cargo ledger cannot be read (${error.message})`]; } finally { cleanup(); }
        for (const p of problems.slice(0, 10)) o.err(`${tag} RK_CHECK_ERROR: [${at}] a broker call the right-release child made did not complete: ${typeof p === "string" ? p : JSON.stringify(p)}`);
        return problems.length === 0;
      },
    };
  } catch (error) {
    if (base) fs.rmSync(base, { recursive: true, force: true });
    return { ...none, refusal: `${error.code ?? "RK_CARGO_CARRIER"}: the stage's cargo carrier cannot be installed (${error.message})` };
  }
}

/**
 * The checkout's Cargo output directory for the snapshot of `commit`: `{ dir }` (null when the commit has no Cargo manifest) or
 * `{ refusal }`. Each tracked Cargo.toml of the commit is asked, in the CHECKOUT with the stage's environment, for its
 * `target_directory` (which honours CARGO_TARGET_DIR and `.cargo/config.toml` build.target-dir) and its workspace root; one workspace
 * yields its directory, several are refused rather than pointed at one shared directory (GATE-CONTRACT: mixed workspaces never share a target).
 */
function cargoOutputPin({ o, git, root, commit, executor }) {
  const listed = git("ls-tree", "-r", "--name-only", "-z", commit);
  if (failed(listed)) return { refusal: `cannot list the files of ${commit.slice(0, 12)}: ${why(listed)}` };
  const manifests = text(listed).split("\0").filter((f) => f === "Cargo.toml" || f.endsWith("/Cargo.toml"));
  const cargo = executor.cargo ?? ["cargo"];
  const workspaces = new Map();
  for (const m of manifests) {
    const file = path.join(root, ...m.split("/"));
    if (!o.exists(file)) return { refusal: `the checkout has no ${m}, so its Cargo output directory cannot be resolved` };
    const r = o.spawn(cargo[0], [...cargo.slice(1), "metadata", "--offline", "--format-version", "1", "--no-deps", "--manifest-path", file], { cwd: path.dirname(file), env: o.env });
    let doc = null;
    try { doc = failed(r) ? null : JSON.parse(text(r)); } catch { doc = null; }
    if (!doc || typeof doc.target_directory !== "string" || !path.isAbsolute(doc.target_directory) || typeof doc.workspace_root !== "string") return { refusal: `cargo metadata in the checkout reported no absolute target_directory for ${m}${failed(r) ? ` (${why(r)})` : ""}` };
    workspaces.set(path.resolve(doc.workspace_root), path.resolve(doc.target_directory));
  }
  if (workspaces.size > 1) return { refusal: `the commit holds ${workspaces.size} Cargo workspaces (${[...workspaces.keys()].map((w) => posix(path.relative(root, w)) || ".").join(", ")}); one directory cannot serve them all, and right-release's own Cargo calls carry no per-workspace wrapper` };
  return { dir: workspaces.size ? [...workspaces.values()][0] : null };
}

/**
 * rightkit-gate --to dev|test [--commit <rev>] [-- <right-release dev|test arguments>]: the local compiles, on a commit like every other
 * stage (docs/GATE-CONTRACT.md "Commit-only build": `--to dev` and `--to test` build a commit; `--worktree` never admits a build). The
 * door has selected the commit and synced its snapshot under ownership (runStages); this runs full class A of that snapshot
 * (`check --commit <sha> --full-a --snapshot <dir>`, never `--worktree`), then right-release dev|test with the snapshot's app root as
 * its cwd under a live door run, which carries the source split. Hosted CI, whose runner checkout is the commit, keeps its own
 * clean checkout. The snapshot is verified before right-release starts and after it ends (E6): a result that covers no known bytes
 * is refused.
 */
async function runDevTest(o, args, root, git, io, to, appRoot, appDir) {
  const tag = "rightkit-gate:";
  const separator = args.indexOf("--");
  const passthrough = separator >= 0 ? args.slice(separator + 1) : [];
  // `--to dev --platform mac` (the documented form) hands the platform to right-release as `--platform mac`; a platform given after
  // `--` wins (Pulse, 2026-10-27: the flag before `--` was dropped and right-release refused "requires --platform").
  const gateArgs = separator >= 0 ? args.slice(0, separator) : args;
  const platformAt = gateArgs.indexOf("--platform");
  if (platformAt >= 0 && gateArgs[platformAt + 1] && !passthrough.includes("--platform")) passthrough.unshift("--platform", gateArgs[platformAt + 1]);
  const ds = o.doorSnapshot ?? null;
  const commit = ds ? ds.commit : text(git("rev-parse", "HEAD")).trim();
  const tree = ds ? ds.tree : text(git("rev-parse", "HEAD^{tree}")).trim();
  const common = text(git("rev-parse", "--git-common-dir")).trim();
  if (!/^[0-9a-f]{40}$/.test(commit) || !common) { o.err(`${tag} cannot read HEAD and the git directory`); return 2; }
  // Hosted CI's runner checkout is the commit; anywhere else the snapshot is, and the checkout may hold anything.
  if (!ds) {
    const dirty = git("status", "--porcelain", "--untracked-files=no");
    if (failed(dirty) || text(dirty).trim()) { o.err(`${tag} BLOCKED: the checkout differs from HEAD; every stage works on the commit, so commit or stash first`); return 1; }
  }
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
  if (executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
  o.out(`${tag} --to ${to} for ${commit.slice(0, 12)}${appDir === "." ? "" : ` (app ${appDir})`}: check (class A) -> right-release ${to}; executor ${executor.kind}${ds ? `; snapshot ${ds.path}` : ""}`);
  // Full class A of the commit before right-release compiles anything: of the snapshot, the checks that execute repository code included.
  const checked = await (o.runCheck ?? (() => main(["check", "--commit", ds ? commit : "HEAD", "--full-a", ...(ds ? ["--snapshot", ds.path] : [])], { ...io, cwd: root, ...(ds ? { buildOwnership: ds.own } : {}) })))();
  if (checked !== 0) { o.err(`${tag} --to ${to} stopped at check: nothing compiled`); return 1; }
  // The snapshot is exactly tree + dependencies + the ledger before right-release starts; what it wrote may remain only as ledger
  // entries (a generated `gen/schemas`, a dist), recorded by this gate's own walk (stage-ledger.mjs, decision 2 Q1).
  if (ds && !(await openSnapshotStage({ o, ds, root, stage: to, tag }))) { o.err(`${tag} --to ${to} stopped: the snapshot is not the commit's bytes plus the ledger; nothing compiled`); return 2; }
  // Cargo would resolve `target/` (or a relative build.target-dir) against the snapshot: its output directory is resolved in the checkout
  // and passed absolute, so the warm outputs stay the checkout's and the snapshot holds no target/ (docs/GATE-CONTRACT.md "Output
  // directory & cache"). One Cargo workspace is pinned as a whole; several would share one directory (refused, naming them).
  // A broker host's output is the broker's (the receipts' `outputDir`); only a direct host pins the checkout's target directory.
  const pin = ds && executor.kind !== "broker" ? cargoOutputPin({ o, git, root, commit, executor }) : { dir: null };
  if (pin.refusal) { o.err(`${tag} BLOCKED: RK_SNAPSHOT_OUTPUT_UNPINNED: ${pin.refusal}; nothing compiled`); return 1; }
  if (pin.dir) o.out(`${tag} output directory for the Cargo workspace: ${pin.dir} (the checkout's)`);
  const commonDir = path.resolve(root, common);
  const source = ds ? sourceSplitOf({ root, ds, appDir }) : null;
  // right-release dev and test, and the Tauri CLI and every other child they start, reach Cargo only through the stage's carrier on a broker host.
  const carrier = await carrierForChildren({ o, ds, root, git, executor, stage: to, tag });
  if (carrier.refusal) { o.err(`${tag} BLOCKED: ${carrier.refusal}; nothing compiled`); return 1; }
  const run = startRun({ commonDir, commit, tree, stage: to, ...(source ? { source } : {}) });
  try {
    const cli = fileURLToPath(new URL("../cli/right-release.mjs", import.meta.url));
    // right-release reads its config from its cwd: the app root (appRootOf), inside the snapshot on the snapshot route, not the
    // directory the command was typed in (apps/foo/src is apps/foo; Astra, third pass).
    // The door's own RightKit runs every sub-step: a dev target's `right-release web-build|stage-sidecar` steps resolve `right-release`
    // on PATH, which found the app's node_modules copy (HeardRight on the Dell, 2026-10-27: the app's 0.6.8 guard refused what the
    // door's 0.6.9 admitted). An installed gate (node_modules/@rightkit/release/bin) has its package's bin shims two levels up.
    const r = (o.spawnInherit ?? ((cmd, a, opts) => spawnInherit(cmd, a, { ...opts, stdio: "inherit" })))(process.execPath, [cli, to, ...passthrough], { cwd: source ? snapshotAppRootOf(ds.path, appDir) : appRoot, env: carriedEnv(carrier, { ...o.env, ...run.env }, { ...doorPath({ ...o, env: { ...o.env, ...carrier.env } }), ...(pin.dir ? { CARGO_TARGET_DIR: pin.dir } : {}) }) });
    const carriedOk = await carrier.finish({ stage: to });
    if (ds && !(await closeSnapshotStage({ o, ds, root, stage: to, step: to, tag })).ok) { o.err(`${tag} --to ${to} stopped: the result covers no known bytes`); return 2; }
    return r.status === 0 && carriedOk ? 0 : 1;
  } finally { endRun(run); carrier.cleanup(); }
}

/** A direct (Linux) executor's environment (PATH without RustRight shim directories) for every child this run starts. */
function applyExecutorEnv(o, executor) {
  if (executor?.env && o.env) Object.assign(o.env, executor.env);
  if (executor?.note && o.out) o.out(`rightkit-gate: ${executor.note}`);
}

/** PATH with the door's own `right-release` shims first (`<node_modules>/.bin` beside an installed gate), or nothing for a checkout. */
export function doorPath(o, gateFile = o.gateFile ?? fileURLToPath(import.meta.url), platform = o.platform ?? process.platform) {
  const bin = path.resolve(path.dirname(gateFile), "..", "..", "..", ".bin");
  const shim = path.join(bin, platform === "win32" ? "right-release.cmd" : "right-release");
  if (!(o.exists ?? fs.existsSync)(shim)) return {};
  const key = platform === "win32" ? Object.keys(o.env ?? {}).find((k) => k.toUpperCase() === "PATH") ?? "Path" : "PATH";
  return { [key]: `${bin}${path.delimiter}${(o.env ?? {})[key] ?? ""}` };
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
    out: (m) => console.log(m), err: (m) => console.error(m), cwd: process.cwd(), env: process.env, platform: process.platform,
    // Where this gate runs from (the door puts its own shims first on PATH when it is an installed package: doorPath).
    gateFile: fileURLToPath(import.meta.url), ...io,
  };
  const args = [...argv];
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  // `clippy`: the Clippy route (../clippy-route.mjs; docs/GATE-CONTRACT.md §1 "Clippy route & batching"). Dispatched before the global
  // flags below because everything after `clippy` is cargo's own argv, verbatim (`-v`, `-h` and `--version` are cargo's).
  if (args[0] === "clippy") return (await import("../clippy-route.mjs")).clippyCommand(o, args.slice(1), { main, io, absoluteEnv: SNAPSHOT_ABSOLUTE_ENV });
  if (args.includes("--version") || args.includes("-v")) {
    o.out(JSON.parse(fs.readFileSync(new URL("../package.json", import.meta.url), "utf8")).version);
    return 0;
  }
  if (args.includes("--help") || args.includes("-h")) { o.out(USAGE); return 0; }
  // `--pre-push` (the plan mode of the retired pre-push hook) no longer exists: commit and push run no gate (docs/GATE-CONTRACT.md).
  if (args.includes("--pre-push")) { o.err(`rightkit-gate: unknown option "--pre-push"\n${USAGE}`); return 2; }
  // `snapshot`: sync the selected commit into the gate-owned source directory and verify it is exactly the commit's tree
  // (../snapshot.mjs; docs/GATE-CONTRACT.md §1). Reads the checkout's commits only, never its working files.
  // `--verify --env <digest>` also requires the installed dependency environment to still be the digest `env-digest --snapshot` recorded.
  if (args[0] === "snapshot") {
    const sc = await import("../snapshot-check.mjs");
    return (await import("../snapshot.mjs")).snapshotCommand(o, args.slice(1), { environmentDigest: ({ checkout, dir, commit }) => classAEnvironmentDigest({ root: dir, gatePackage: o.gatePackage, files: sc.commitFiles(checkout, commit, o.spawn).files, spawn: o.spawn }) });
  }
  // `live-scans`: re-run only the live remote scans (pnpm-audit) of a checked tree, answering the broker's verdict-reuse refresh
  // (../snapshot-check.mjs; GATE-CONTRACT §1, E18; rightkit#1257).
  if (args[0] === "live-scans") return (await import("../snapshot-check.mjs")).liveScansCommand(o, args.slice(1), { discoverPnpmRoots, discoverNpmUnits });
  const sub = args[0] && !args[0].startsWith("-") ? args[0] : null;
  if (sub && sub !== "install-hook" && sub !== "check" && sub !== "stage" && sub !== "env-digest") { o.err(`rightkit-gate: unknown command ${JSON.stringify(sub)}\n${USAGE}`); return 2; }
  // `check` is the deterministic stage (Adrian, 2026-10-25: deterministic always first, one door): the app-check rules, then every
  // class A scan (lockfiles, cargo-deny, cargo-audit, secrets, licences, manifests, workflow lint, syntax...) over the whole tree of the
  // commit, never a diff. Nothing compiles. The RustRight broker runs exactly this before it admits any compile. `--list`,
  // `--print-baseline` and `--only` stay app-check-only views.
  if ((sub === "check" || (!sub && args.includes("--all"))) && args.includes("--snapshot") && !o.buildOwnership) {
    // Host-wide build ownership (../build-ownership.mjs; GATE-CONTRACT §1): held from before the first snapshot read through the
    // end of the check, so no other request can sync the snapshot while class A runs in it. A nested gate whose
    // RIGHTKIT_BUILD_OWNERSHIP names the current owner joins it. Busy (or liveness unknown) runs nothing.
    const { acquireBuildOwnership, ownershipFile } = await import("../build-ownership.mjs");
    let own;
    try { own = (o.acquireOwnership ?? acquireBuildOwnership)({ file: ownershipFile({ platform: o.platform, beside: o.snapshotRoot }), stage: sub ?? "compile", snapshot: option("--snapshot") ?? null, commit: option("--commit") ?? null, platform: o.platform, env: o.env }); }
    catch (e) { if (e.code !== "busy" && e.code !== "refused") throw e; o.err(`rightkit-gate: RK_CHECK_ERROR: ${e.message}; nothing ran${e.code === "refused" ? ", nothing created" : ""}`); return 2; }
    o.out(`rightkit-gate: build ownership ${own.joined ? "joined" : "held"} ${own.handle}${own.reclaimed ? ` (reclaimed from ${own.reclaimed.from}: ${own.reclaimed.proofs.join("; ")})` : ""}`);
    try { return await main(argv, { ...io, buildOwnership: own }); }
    finally { const r = own.release(); if (r?.released === false && !own.joined) o.err(`rightkit-gate: build ownership ${own.handle} kept: ${r.why}`); }
  }
  if (sub === "check") {
    // `--snapshot <dir>`: full class A of the gate-owned snapshot of a selected commit (../snapshot-check.mjs). The checkout is only
    // where the commit comes from: its HEAD and status are never read, and the snapshot must be exactly the commit's tree before and
    // after the check, or the check is RK_CHECK_ERROR (exit 2).
    let snapshotDir = null; let snapshotCommit = null; let snapshotCheckout = null; let sc = null;
    if (args.includes("--snapshot")) {
      sc = await import("../snapshot-check.mjs");
      try {
        const at = option("--repo") ? path.resolve(o.cwd, option("--repo")) : o.cwd;
        const top0 = o.spawn("git", ["rev-parse", "--show-toplevel"], { cwd: at });
        if (failed(top0) || !text(top0).trim()) throw new sc.CheckError(`${at} is not a git checkout`);
        snapshotCheckout = fs.realpathSync.native(path.resolve(text(top0).trim()));
        snapshotDir = sc.snapshotOption(args, { checkout: snapshotCheckout });
        snapshotCommit = sc.commitOf(snapshotCheckout, option("--commit"), o.spawn);
        sc.verifySnapshotDir({ checkout: snapshotCheckout, dir: snapshotDir, ...snapshotCommit, when: "before the check" });
        o.out(`rightkit-gate: snapshot ${snapshotDir} is commit ${snapshotCommit.commit} tree ${snapshotCommit.tree} (verified before the check)`);
      } catch (e) { o.err(`rightkit-gate: ${sc.CHECK_ERROR}: ${e.message}`); return 2; }
    }
    if (args.filter((arg) => arg === "--rule-tier").length > 1 || (args.includes("--rule-tier") && option("--rule-tier") === undefined)) {
      o.err("rightkit-gate: check requires exactly one value for --rule-tier"); return 2;
    }
    const requestedTier = option("--rule-tier") ?? "compile";
    if (!["compile", "release"].includes(requestedTier)) { o.err(`rightkit-gate: check --rule-tier must be compile or release, not ${JSON.stringify(requestedTier)}`); return 2; }
    // Full admission runs every deterministic scan; its app rules run at the tier the stage owes (compile unless named): dev, test
    // and package compile on the compile-tier rules, qualify and release name the release tier (rightkit#1296).
    const fullAdmission = args.includes("--full-a") && !args.includes("--list") && !args.includes("--print-baseline") && !args.includes("--only");
    const stripSnapshot = (list) => { const i = list.indexOf("--snapshot"); return i < 0 ? list : [...list.slice(0, i), ...list.slice(i + 2)]; };
    const appArgs = stripSnapshot(args.slice(1).filter((a) => a !== "--full-a"));
    if (fullAdmission && !appArgs.includes("--rule-tier")) appArgs.push("--rule-tier", requestedTier);
    const appCheck = await (o.appCheck ?? (await import("../app-check.mjs")).main)(appArgs, { out: o.out, err: o.err, label: "check", cwd: o.cwd });
    if (args.includes("--list") || args.includes("--print-baseline") || args.includes("--only") || o.classA === false) {
      // A view of the app rules, never a class A verdict: say so, so no caller can take its exit 0 for a full check.
      // A green partial view exits PARTIAL_EXIT (3), never 0, so a caller that admits only exit 0 rejects it by status alone.
      if (args.includes("--list") || args.includes("--print-baseline")) return appCheck;
      o.err("rightkit-gate: PARTIAL: app rules only (--only); no class A scan ran, so this is not a class A result");
      return appCheck === 0 ? PARTIAL_EXIT : appCheck;
    }
    if (appCheck !== 0) {
      o.err("rightkit-gate: BLOCKED: app rules failed; no further scans ran");
      return appCheck;
    }
    const repoArg = option("--repo");
    const checkCwd = repoArg ? path.resolve(o.cwd, repoArg) : o.cwd;
    // The app rules' tier is the stage's (rightkit#1296); class A's deterministic scans are the same at either tier.
    const ruleTier = requestedTier;
    const classA = await main(["--class-a", "--rule-tier", ruleTier, ...(args.includes("--full-a") ? ["--full-a"] : []), ...(args.includes("--worktree") ? ["--worktree"] : ["--commit", snapshotCommit?.commit ?? option("--commit") ?? "HEAD"]), ...(snapshotDir ? ["--snapshot", snapshotDir] : [])], { ...io, cwd: checkCwd });
    // RightKit checking itself: its own rules (line endings, consumer recipes, npm surface, version copies and the version feed, dispatcher
    // pins, consumer locks, generated docs, temp paths: scripts/preflight.mjs) and its exports' import safety are class A for the RightKit repository.
    let selfRules = 0;
    const top = o.spawn("git", ["rev-parse", "--show-toplevel"], { cwd: checkCwd });
    const root = failed(top) ? null : path.resolve(text(top).trim());
    const self = o.selfPath ?? fileURLToPath(import.meta.url);
    // RightKit's own scripts are the checked repository's code: they run only after the app rules and every class A scan passed,
    // never after a failure, so a red tree executes nothing of its own.
    // Compared as real, long paths (and case-blind on Windows): git prints the long form of an 8.3 short path such as RUNNER~1.
    const canon = (p) => { let c; try { c = fs.realpathSync.native(p); } catch { c = path.resolve(p); } return process.platform === "win32" ? c.toLowerCase() : c; };
    // In snapshot mode RightKit's own scripts are the snapshot's (the commit's code), never the live checkout's.
    const selfRoot = snapshotDir ?? root;
    const rightkitRepository = root && (() => {
      try { return JSON.parse(o.readFile(path.join(selfRoot, "packages", "release", "package.json"))).name === "@rightkit/release"; }
      catch { return false; }
    })();
    if (rightkitRepository && (!o.exists(path.join(selfRoot, "scripts", "preflight.mjs")) || !o.exists(path.join(selfRoot, "scripts", "ci", "exports-import-effects.mjs")))) {
      o.err("rightkit-gate: BLOCKED: RightKit's preflight or exports check is missing");
      selfRules = 1;
    }
    if (root && selfRules === 0 && snapshotDir && rightkitRepository && appCheck === 0 && classA === 0) {
      // The commit's own preflight reads the commit from the checkout's object database; its exports probe imports from the snapshot,
      // whose dependencies class A installed, and reports beside (never over) the commit-bound report publication reads.
      const launch = o.spawnInherit ?? ((cmd, a, opts) => spawnInherit(cmd, a, { ...opts, stdio: "inherit" }));
      const r = launch(process.execPath, [path.join(snapshotDir, "scripts", "preflight.mjs"), "--repo", root, "--commit", snapshotCommit.commit], { cwd: root, env: o.env });
      selfRules = r.status === 0 ? 0 : 1;
      const effects = path.join(snapshotDir, "scripts", "ci", "exports-import-effects.mjs");
      if (selfRules === 0 && o.exists(effects)) {
        const common = o.spawn("git", ["rev-parse", "--path-format=absolute", "--git-common-dir"], { cwd: root });
        const report = path.join(failed(common) ? path.join(root, ".git") : text(common).trim(), "rightkit-gate", "import-effects", `snapshot-${snapshotCommit.commit}.json`);
        const e = launch(process.execPath, [effects, "--root", snapshotDir, "--no-export", "--report", report], { cwd: snapshotDir, env: o.env });
        if (e.status !== 0) selfRules = 1;
      }
    } else if (root && selfRules === 0 && !snapshotDir && (rightkitRepository || canon(self).startsWith(canon(root) + path.sep)) && o.exists(path.join(root, "scripts", "preflight.mjs")) && appCheck === 0 && classA === 0) {
      const launch = o.spawnInherit ?? ((cmd, a, opts) => spawnInherit(cmd, a, { ...opts, stdio: "inherit" }));
      const selector = args.includes("--worktree") ? ["--worktree"] : ["--commit", option("--commit") ?? "HEAD"];
      const r = launch(process.execPath, [path.join(root, "scripts", "preflight.mjs"), "--repo", root, ...selector], { cwd: root, env: o.env });
      selfRules = r.status === 0 ? 0 : 1;
      // And its exports import cleanly (scripts/ci/exports-import-effects.mjs: every exported module imported in a fresh, permission-
      // limited process; an import that prints, exits or reads its argv fails), which publish.yml's scans job ran alone before. That
      // check probes the checkout's HEAD, so it runs only when HEAD is the requested commit (or the working tree was asked for);
      // any other commit is refused rather than proven by other bytes.
      const effects = path.join(root, "scripts", "ci", "exports-import-effects.mjs");
      if (selfRules === 0 && o.exists(effects)) {
        const requested = args.includes("--worktree") ? null : option("--commit") ?? "HEAD";
        const at = (rev) => { const v = o.spawn("git", ["rev-parse", "--verify", "--quiet", `${rev}^{commit}`], { cwd: root }); return failed(v) ? null : text(v).trim(); };
        // The probe exports HEAD, so a working tree whose tracked or untracked (unignored) files differ from HEAD is refused too.
        const dirty = () => { const st = o.spawn("git", ["status", "--porcelain", "--untracked-files=all"], { cwd: root }); return failed(st) || text(st).trim() !== ""; };
        if (requested !== null && at(requested) !== at("HEAD")) {
          o.err(`rightkit-gate: BLOCKED: the exports import check probes the checkout's HEAD (${String(at("HEAD")).slice(0, 12)}), not the requested ${requested}; check out that commit to check it`);
          selfRules = 1;
        } else {
          // A working-tree check of a dirty checkout probes the working tree's own bytes (--worktree: staged into a throwaway index
          // and exported like a commit), never HEAD's, so local development is checked on what it actually is.
          const worktreeProbe = requested === null && dirty();
          // Like CI's scans job: exports that resolve to build output are INCOMPLETE here and handed, through --report, to the
          // sealed-byte check that publication runs; without --report the probe exits 3 and a green tree could never pass.
          const common = o.spawn("git", ["rev-parse", "--path-format=absolute", "--git-common-dir"], { cwd: root });
          const report = path.join(failed(common) ? path.join(root, ".git") : text(common).trim(), "rightkit-gate", "import-effects", "report.json");
          const e = launch(process.execPath, [effects, "--report", report, ...(worktreeProbe ? ["--worktree"] : [])], { cwd: root, env: o.env });
          if (e.status !== 0) selfRules = 1;
        }
      }
    }
    // A RustRight broker's check names the tree it captured for the compile: the declared inputs there (ignored files the
    // commit does not hold) are judged too, as the bytes the compile will read.
    let inputs = 0;
    const captured = o.env.RIGHTKIT_COMPILE_INPUTS;
    if (captured) {
      const { judgeCompileInputs } = await import("../compile-inputs.mjs");
      const judged = judgeCompileInputs(path.resolve(captured));
      for (const i of judged.inputs) o.out(`rightkit-gate: compile input ${i.path}: ${i.files} file(s), sha256 ${i.sha256}`);
      for (const f of judged.findings) o.err(`rightkit-gate: compile input FAIL ${f}`);
      if (!judged.inputs.length && !judged.findings.length) o.out("rightkit-gate: no declared compile inputs in the captured tree");
      inputs = judged.findings.length ? 1 : 0;
    }
    if (snapshotDir) {
      try { sc.verifySnapshotDir({ checkout: snapshotCheckout, dir: snapshotDir, ...snapshotCommit, when: "after the check" }); }
      catch (e) { o.err(`rightkit-gate: ${sc.CHECK_ERROR}: ${e.message}; the result covers no known bytes`); return 2; }
      o.out(`rightkit-gate: snapshot ${snapshotDir} is still tree ${snapshotCommit.tree} after the check`);
    }
    return Math.max(appCheck, classA, selfRules, inputs);
  }
  const classAOnly = args.includes("--class-a");
  // --full-a: every class A check, the ones that execute the repository's code (tests, typecheck, lint, generated docs) included,
  // on the live working tree, before anything compiles. The door's dev/compile stages require it: a content-only check never
  // authorizes a build (Adrian: class A is everything mechanical and deterministic; reviewer audit 2026-10-28).
  const fullA = classAOnly && args.includes("--full-a");
  // Full class A of a gate-owned snapshot (`check --snapshot`, which verified it): the snapshot is the commit's bytes, so neither the
  // checkout's HEAD nor its cleanliness matters, and the repository's code runs in the snapshot, never in the checkout.
  const snapshotA = classAOnly ? (option("--snapshot") ?? null) : null;
  // The door's compile of a selected commit (`--all --commit <sha> --snapshot <dir>`, joint plan step 4c): every scan, class B's
  // compiles included, reads the verified gate-owned snapshot, never the checkout; outputs go to the checkout's own target
  // directory. A direct host only, until the broker's snapshot carrier lands (step 4d).
  const snapshotB = !classAOnly && args.includes("--all") ? (option("--snapshot") ?? null) : null;
  if (snapshotB && !option("--commit")) { o.err(`${tag} --all --snapshot <dir> needs --commit <sha>`); return 2; }
  if (fullA && !args.includes("--worktree") && !snapshotA) {
    // A commit is checked in full only where its bytes are the live checkout: it is HEAD and nothing tracked or untracked differs
    // (the unit and app stages already refuse a dirty tree). Then the working tree is that commit, and the checks run there.
    const at = (...a) => o.spawn("git", a, { cwd: o.cwd });
    const want = at("rev-parse", "--verify", `${option("--commit") ?? "HEAD"}^{commit}`), head = at("rev-parse", "--verify", "HEAD^{commit}");
    const dirty = at("status", "--porcelain", "--untracked-files=all");
    if (failed(want) || failed(head) || text(want).trim() !== text(head).trim()) { o.err("rightkit-gate: --full-a checks a commit only when it is HEAD of this checkout: the repository's checks run on the live tree"); return 2; }
    if (failed(dirty) || text(dirty).trim()) { o.err("rightkit-gate: --full-a on a commit needs a clean checkout (the live tree must be exactly that commit); commit or remove the changes"); return 2; }
    const i = args.indexOf("--commit"); if (i >= 0) args.splice(i, 2);
    args.push("--worktree");
  }
  const classACommit = classAOnly && !args.includes("--worktree") ? (option("--commit") ?? "HEAD") : null;
  if (args.includes("--base") && (!option("--base") || option("--base").startsWith("--"))) { o.err("rightkit-gate: --base needs a ref"); return 2; }

  const top = o.spawn("git", ["rev-parse", "--show-toplevel"], { cwd: o.cwd });
  if (failed(top) || !text(top).trim()) { o.err("rightkit-gate: not inside a git repository"); return 2; }
  const root = path.resolve(text(top).trim());
  const git = (...a) => o.spawn("git", a, { cwd: root });
  if (sub === "install-hook") return await installHook(o, root);
  // `env-digest`: only the environment identity a full class A verdict depends on (classAEnvironmentDigest), no scans. The broker
  // recomputes it when a queued compile starts and re-runs the check if it changed since the verdict.
  if (sub === "env-digest") {
    // `env-digest --snapshot <dir> --commit <sha>`: the same identity over a verified snapshot (the broker's START recheck of a
    // snapshot-aware verdict): the commit's files, the snapshot's installed dependencies.
    if (args.includes("--snapshot")) {
      const sc = await import("../snapshot-check.mjs");
      try {
        const dir = option("--snapshot"); const rev = option("--commit");
        if (!dir || !path.isAbsolute(dir) || !rev || rev.startsWith("--")) throw new sc.CheckError("env-digest --snapshot needs an absolute directory and --commit <sha>");
        const checkout = fs.realpathSync.native(root);
        const { commit, tree } = sc.commitOf(checkout, rev, o.spawn);
        const real = fs.realpathSync.native(dir);
        sc.verifySnapshotDir({ checkout, dir: real, commit, tree, when: "for env-digest" });
        o.out(`env=${classAEnvironmentDigest({ root: real, gatePackage: o.gatePackage, files: sc.commitFiles(checkout, commit, o.spawn).files, spawn: o.spawn })}`);
        return 0;
      } catch (e) { o.err(`rightkit-gate: ${e.code === sc.CHECK_ERROR ? `${sc.CHECK_ERROR}: ` : "env-digest: "}${e.message}`); return 2; }
    }
    const listed = git("ls-files", "-z", "--cached", "--others", "--exclude-standard");
    if (failed(listed)) { o.err(`rightkit-gate: env-digest cannot list the working tree: ${why(listed)}`); return 2; }
    try { const files = text(listed).split("\0").filter(Boolean).map(posix); o.out(`env=${classAEnvironmentDigest({ root, gatePackage: o.gatePackage, files, spawn: o.spawn })}`); }
    catch (error) { o.err(`rightkit-gate: env-digest: ${error.message}`); return 2; }
    return 0;
  }
  if (args.includes("--plan") && option("--to") !== undefined) { o.err("rightkit-gate: --plan plans the class A scans and runs nothing; it takes no --to stage (Astra, second pass)"); return 2; }
  // `--unit` is a gate flag only before a `--` separator: after it the arguments belong to right-release (an app's `--to dev -- …`).
  const separator = args.indexOf("--");
  const unitFlag = args.includes("--unit") && (separator < 0 || args.indexOf("--unit") < separator);
  // `--route` is a gate flag only before a `--` separator (after it the arguments belong to right-release). It answers which route a
  // stage takes and runs nothing, so it takes no unit, receipt or release stage (rightkit#1297).
  const gateArgs = separator < 0 ? args : args.slice(0, separator);
  if (gateArgs.includes("--route")) {
    const routeTo = option("--to");
    const clash = ["--unit", "--receipt-in", "--receipt-out"].filter((f) => gateArgs.includes(f));
    if (routeTo === undefined) { o.err(`rightkit-gate: --route needs --to ${ROUTE_PROBE_STAGES.join("|")}\n${USAGE}`); return 2; }
    if (routeTo === "release") { o.err("rightkit-gate: --route answers for --to check|compile|package|qualify|dev|test; release uploads and has no route of its own"); return 2; }
    if (clash.length) { o.err(`rightkit-gate: --route runs nothing and takes no ${clash.join(" or ")}`); return 2; }
    o.routeProbe = true;
  }
  if (option("--to") !== undefined && unitFlag) {
    const selector = option("--unit");
    const validArgs = args.length % 2 === 0 && args.every((a, i) => i % 2 ? !a.startsWith("--") : ["--to", "--unit", "--receipt-out", "--commit"].includes(a)) && ["--to", "--unit", "--receipt-out", "--commit"].every((flag) => args.filter((a) => a === flag).length <= 1);
    if (!validArgs || !selector) {
      o.err("rightkit-gate: unit stages take only --to, --unit and optional --receipt-out and --commit"); return 2;
    }
    // Row 7.06a: on every host but hosted CI a unit stage builds the selected commit's snapshot (Linux direct, Mac and Dell through the carrier).
    if (!o.doorSnapshot && snapshotRouteSelected(o)) return await runUnitSnapshot(o, args, root, git, io, option);
    // Hosted CI only: the runner's checkout is the commit.
    if (option("--commit") !== undefined) { o.err("rightkit-gate: BLOCKED: --commit selects a commit for the snapshot route; hosted CI builds the runner's checkout, which is the commit"); return 2; }
    const head = git("rev-parse", "HEAD"), treeResult = git("rev-parse", "HEAD^{tree}"), common = git("rev-parse", "--git-common-dir"), dirty = git("status", "--porcelain", "--untracked-files=no");
    const commit = text(head).trim(), tree = text(treeResult).trim();
    if (failed(head) || failed(treeResult) || failed(common) || !text(common).trim() || !/^[0-9a-f]{40}$/.test(commit) || !/^[0-9a-f]{40}$/.test(tree) || failed(dirty) || text(dirty).trim()) { o.err("rightkit-gate: BLOCKED: unit stages need clean committed HEAD and tree"); return 1; }
    const match = /^(?:npm:@rightkit\/([a-z0-9-]+)|cargo:(rightkit-[a-z0-9-]+))$/.exec(selector);
    const manifest = match && (match[1] ? `packages/${match[1]}/package.json` : `crates/${match[2]}/Cargo.toml`);
    if (!manifest || failed(git("ls-files", "--error-unmatch", "--", ".rightkit-local-build.json", manifest))) { o.err("rightkit-gate: BLOCKED: unit and local grant must be tracked canonical files"); return 1; }
    const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
    const executor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
    if (executor.kind === "refuse") { o.err(`rightkit-gate: BLOCKED: ${executor.problem}`); return 1; }
    const reportOut = option("--receipt-out");
    // Loaded here, not at the top: the unit path reads Cargo manifests (smol-toml), which an installed copy of this package without
    // node_modules does not carry; the app paths of this gate must keep loading there (app-check.test: the installed copy).
    const { runUnitGate } = await import("../unit-gate.mjs");
    return await runUnitGate({ selector, to: option("--to"), root, commonDir: path.resolve(root, text(common).trim()), commit, tree, executor, env: o.env, platform: o.platform,
      check: (tier) => main(["check", "--commit", "HEAD", "--full-a", "--rule-tier", tier], { ...io, cwd: root }), out: o.out, err: o.err,
      ...(reportOut ? { writeReport: (report) => o.writeFile(path.resolve(o.cwd, reportOut), `${JSON.stringify(report, null, 2)}\n`) } : {}),
    });
  }
  if (unitFlag) { o.err("rightkit-gate: --unit requires --to"); return 2; }
  if (option("--to") !== undefined) return await runStages(o, args, root, git, io);
  if (sub === "stage") return stageForPublisher(o, args, root, git);

  const planOnly = args.includes("--plan");
  let base = option("--base") ?? "origin/main";
  const tag = "rightkit-gate:";

  // git pre-push: "<local ref> <local sha> <remote ref> <remote sha>" per pushed ref. A plan reads no stdin.
  // git's push lines are split on single ASCII spaces only (parsePushLines): a ref name may hold Unicode whitespace. A line that does not read as four fields
  // (git hands the local field over verbatim, so a rev expression such as `HEAD@{1 minute ago}` or `HEAD^{/one thing}` holds spaces)
  // refuses the push on every host and in every repository, never a line that disappears: its pushed commit could not be compared with
  // HEAD and its remote ref could not be judged. `pushedRemoteShas` are the remote commits being replaced.
  // A push of ref deletions alone carries no content: nothing to check, nothing to plan (the hook skips its check the same way).
  let deletionsOnly = false;
  if (!planOnly && !classAOnly) {
    const head = text(git("rev-parse", "HEAD")).trim();
    const parsed = parsePushLines(o.readStdin());
    // Every line is read before any shortcut: a malformed line hides what is pushed, so it blocks even beside deletions (Astra, third pass).
    if (parsed.malformed.length) {
      o.err(`${tag} BLOCKED: pre-push stdin line ${parsed.malformed[0].number} is not "<local ref> <local sha> <remote ref> <remote sha>" (four fields separated by single spaces), so the pushed refs cannot all be read and the pushed commit cannot be compared with HEAD; push a plain ref (git push <remote> HEAD:<branch>)`);
      return 1;
    }
    deletionsOnly = parsed.lines.length > 0 && parsed.lines.every((l) => ZERO.test(l.localSha));
    if (deletionsOnly) { o.out(`${tag} only deletions are pushed; nothing to check, nothing planned`); return 0; }
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
  const executor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
  // A broker-managed host whose broker is unreachable refuses the moment a Cargo manifest is in the snapshot (below): a repository
  // with no Rust owes nothing to the broker (a git hook's minimal PATH, a JS-only app), and the class A scans never compile.
  const cargoCommand = executor.cargo ?? ["cargo"];
  // A broker host (Mac, Dell) compiles a commit snapshot through the RustRight snapshot carrier: `deps.snapshotBroker`, built in the
  // snapshot-compile region below (item 7 rows 7.20 to 7.24). A broker-managed host whose broker cannot be reached is refused with
  // the executor's own problem, never compiled around.
  if (snapshotB && executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}; nothing ran`); return 1; }
  // The snapshot: the commit or tree being checked (check --commit), HEAD for the gate, or the working tree (check --worktree, --plan).
  const allFilesMode = classAOnly || args.includes("--all");
  const snapshot = planOnly ? null : classAOnly ? classACommit : snapshotB ? option("--commit") : "HEAD";
  // Every effectful gate entry owes the app rules, not only `check` (Astra, 2026-10-26): a bare or --all run on HEAD runs them
  // first, in the tier asked for (compile by default), and plans nothing while they are red. --plan and --class-a (which runs them in
  // `check`) are untouched.
  if (snapshot && !classAOnly) {
    const appRules = await (o.appCheck ?? (await import("../app-check.mjs")).main)(["--repo", root, "--commit", snapshot, "--rule-tier", option("--rule-tier") ?? "compile"], { out: o.out, err: o.err, label: "check", cwd: o.cwd });
    if (appRules !== 0) { const id = git("rev-parse", String(snapshot)); o.err(`${tag} BLOCKED: the app rules are red for ${(failed(id) ? String(snapshot) : text(id).trim()).slice(0, 12)}; no scan was planned or run`); return appRules; }
  }

  const ac = o.abort ?? new AbortController();
  let exportRoot = null;
  let snapshotGitConfig = null;
  let cargoShimDir = null;
  const cleanup = () => {
    if (exportRoot) fs.rmSync(exportRoot, { recursive: true, force: true });
    if (cargoShimDir) fs.rmSync(cargoShimDir, { recursive: true, force: true });
    if (snapshotGitConfig) { fs.rmSync(snapshotGitConfig, { force: true }); try { fs.rmdirSync(path.dirname(snapshotGitConfig)); } catch { /* not empty: leave it, the next exact sync prunes it */ } }
  };
  const signals = [];
  try {
    let repo = root;
    let snapshotBCommit = null;
    if (snapshot && snapshotB) {
      // Direct host: no broker grant to satisfy, so no config-only .git; the snapshot is scanned and compiled in place.
      repo = snapshotB;
    } else if (snapshot && snapshotA) {
      // The verified gate-owned snapshot is the commit's bytes: it is scanned in place, no export.
      repo = snapshotA;
      // As for an export (rightkit#1215): rustright binds the tracked .rightkit-local-build.json grant to the origin URL in .git/config,
      // so a locked cargo metadata in the snapshot needs a config-only .git naming exactly the checkout's origin. It is not a
      // repository (no HEAD, no objects), the seeded identity never reads .git, and it is removed when the check ends.
      const origin = o.spawn("git", ["config", "--get", "remote.origin.url"], { cwd: root });
      const originUrl = failed(origin) ? "" : text(origin).trim();
      if (originUrl && !o.exists(path.join(repo, ".git"))) {
        o.mkdir(path.join(repo, ".git"), { recursive: true });
        snapshotGitConfig = path.join(repo, ".git", "config");
        o.writeFile(snapshotGitConfig, `[remote "origin"]\n\turl = ${originUrl}\n`);
      }
    } else if (snapshot) {
      exportRoot = fs.mkdtempSync(path.join(exportTmpRoot({ env: o.env, platform: o.platform, exists: o.exists }), "rk-gate-"));
      repo = path.join(exportRoot, "repo");
      // Every file of the commit or tree: read-tree into a private index, then checkout-index. `git archive` would leave out files
      // marked export-ignore in .gitattributes, so they would never be scanned; this export has no such exclusion.
      const indexEnv = { ...o.env, GIT_INDEX_FILE: path.join(exportRoot, "index") };
      const made = o.spawn("git", ["read-tree", snapshot], { cwd: root, env: indexEnv });
      // git builds each path as <prefix><index path> with "/" separators and creates only the directories below the prefix, so the
      // export root must exist and the prefix use "/": a "C:\\...\\repo\\" prefix made checkout-index fail on Windows
      // (publish run 40282215043: "cannot create directory at ...\\repo\\.github").
      fs.mkdirSync(repo, { recursive: true });
      const x = failed(made) ? made : o.spawn("git", ["checkout-index", "--all", "--force", `--prefix=${repo.split(path.sep).join("/")}/`], { cwd: root, env: indexEnv });
      if (failed(made) || failed(x)) { o.err(`${tag} cannot export ${snapshot}: ${why(made) || why(x)}`); return 2; }
      // rustright binds the tracked .rightkit-local-build.json grant to the origin URL in .git/config (rustright-policy local_build.rs),
      // and the export has no .git, so cargo metadata in it was refused on every broker host and no push passed the hook (rightkit#1215).
      // A config-only .git carries exactly the source checkout's origin and is not a repository to git (no HEAD, no objects), so nothing
      // can run git against the export. No origin (a checkout without a remote): nothing is written and rustright decides as before.
      const origin = o.spawn("git", ["config", "--get", "remote.origin.url"], { cwd: root });
      const originUrl = failed(origin) ? "" : text(origin).trim();
      if (originUrl) {
        o.mkdir(path.join(repo, ".git"), { recursive: true });
        o.writeFile(path.join(repo, ".git", "config"), `[remote "origin"]\n\turl = ${originUrl}\n`);
      }
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
      if (executor.kind === "refuse" && manifests.length) { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
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

    // The snapshot compile: verified as the commit's tree before anything runs, dependencies installed into it offline from the
    // checkout's store, and every Cargo workspace's output pinned to the target directory cargo resolves for it IN THE CHECKOUT
    // (CARGO_TARGET_DIR or build.target-dir honoured), so the warm outputs are the checkout's and the snapshot holds no target/.
    let targetDirs = null; let cargoShim = null; let snapshotBroker = null;
    // The direct (Linux) compile's evidence (item 7 rows 7.09, 7.18, 7.19): how each output directory was resolved, the declared inputs
    // digested in the snapshot, and the stage's own timings.
    let outputResolution = {}; let directDeclared = []; let directDeclaredNotes = [];
    const directTiming = { bindStart: 0, bindEnd: 0, cargoStart: 0, cargoEnd: 0, afterStart: 0, afterEnd: 0, diagnostics: 0 };
    if (snapshotB) {
      const sc = await import("../snapshot-check.mjs");
      try {
        const checkout = fs.realpathSync.native(root);
        directTiming.bindStart = Date.now();
        snapshotBCommit = sc.commitOf(checkout, String(snapshot), o.spawn);
        // With a ledger the verify returns an object ({ tree, ledger }), without one the tree id: only the refusal (a throw) matters here.
        sc.verifySnapshotDir({ checkout, dir: snapshotB, ...snapshotBCommit, when: "before the compile", ...(o.snapshotLedger ? { ledger: o.snapshotLedger } : {}) });
        directTiming.bindEnd = Date.now();
        // A direct host digests the declared compile inputs in place (the snapshot's own .rightkit/compile-inputs.json, never the
        // checkout's); a malformed manifest refuses before anything compiles. The broker digests them for a broker host.
        if (executor.kind === "direct") {
          const inputsModule = await import("../compile-inputs.mjs");
          const judged = inputsModule.judgeCompileInputs(snapshotB);
          // Only a malformed manifest refuses here (the broker's request does the same); a declared path the snapshot does not hold, or a
          // finding in one, is recorded with the diagnostics and left to the check stage that judges the captured inputs.
          const malformed = judged.findings.filter((f) => f.startsWith(`${inputsModule.COMPILE_INPUTS_FILE}:`));
          if (malformed.length) throw Object.assign(new Error(`the snapshot's declared compile inputs are malformed: ${malformed.join("; ")}`), { code: "RK_COMPILE_INPUTS" });
          directDeclared = judged.inputs;
          directDeclaredNotes = judged.findings.map((f) => `declared compile input: ${f}`);
        }
        o.out(`${tag} snapshot ${snapshotB} is commit ${snapshotBCommit.commit} tree ${snapshotBCommit.tree} (verified before the compile)`);
        for (const r of sc.installSnapshotDependencies({ checkout, snapshot: snapshotB, roots: npmRoots, env: o.env, spawn: o.spawn })) o.out(`${tag} ${sc.describeInstall(r)}`);
      } catch (error) { o.err(`${tag} ${error.code === "RK_CHECK_ERROR" ? "RK_CHECK_ERROR" : "BLOCKED"}: ${error.message}`); return error.code === "RK_CHECK_ERROR" ? 2 : 1; }
      // A broker host (item 7 rows 7.20 to 7.24): the compile goes through the RustRight snapshot carrier. The broker pins the output
      // directory (its managed generation target), so no checkout target is resolved here; the gate's broker is built once, with the
      // host's enrolled pins, the declared inputs read from the snapshot and the build ownership; a missing piece refuses before
      // anything compiles (an app repository with no enrollment included), never a compile around the broker.
      if (executor.kind === "broker" && manifests.length) {
        try {
          const { createSnapshotBroker } = await import("../snapshot-broker.mjs");
          const client = which("rustright").find((c) => path.isAbsolute(c));
          snapshotBroker = createSnapshotBroker({ env: o.env, platform: o.platform, checkout: root, commit: { sha: snapshotBCommit.commit, tree: snapshotBCommit.tree }, snapshot: { path: snapshotB, expectedTree: snapshotBCommit.tree }, ownership: o.buildOwnership, client, gateFile: callingGatePath(), spawn: o.spawn });
        } catch (error) { o.err(`${tag} BLOCKED: ${error.code ?? "RK_SNAPSHOT_BROKER"}: ${error.message}; nothing compiled`); return 1; }
        o.out(`${tag} broker snapshot compile: pins ${snapshotBroker.pins.keyId.slice(0, 12)} from ${snapshotBroker.pinsSource}, ${snapshotBroker.declaredInputs.length} declared input(s) read from the snapshot, ownership ${snapshotBroker.handle}`);
      }
      targetDirs = {};
      for (const ws of snapshotBroker ? [] : rust.workspaces) {
        const manifest = path.join(root, ws.cargoDir, "Cargo.toml");
        if (!o.exists(manifest)) { o.err(`${tag} BLOCKED: the checkout has no ${posix(path.relative(root, manifest))}, so the output directory of workspace ${ws.cargoDir} cannot be resolved; nothing compiled`); return 1; }
        // Resolved in the PRIMARY CHECKOUT with the stage's environment, before the source location changes (row 7.09: CARGO_TARGET_DIR,
        // a relative or absolute build.target-dir, or the default target/ all resolve as for a developer's own build); the cwd and
        // environment used are recorded. A directory inside the snapshot is refused before anything compiles (a second output).
        let resolved;
        try { resolved = resolveTargetDirectory({ spawn: (c, a, opts) => o.spawn(c, a, opts), cargoCommand, manifest, env: o.env, checkoutRoot: root, snapshotRoot: snapshotB, workspace: ws.cargoDir }); }
        catch (error) { o.err(`${tag} BLOCKED: ${error.message}; nothing compiled`); return 1; }
        targetDirs[ws.cargoDir] = resolved.dir; outputResolution[ws.cargoDir] = resolved.inputs;
        o.out(`${tag} output directory for workspace ${ws.cargoDir}: ${targetDirs[ws.cargoDir]} (the checkout's)`);
      }
      // CARGO_HOME, sccache & temp are inherited as absolute values, never snapshot-relative (GATE-CONTRACT "Output directory & cache"):
      // a relative one would be resolved inside the snapshot, a second cache or output location. Refused before anything compiles.
      const relativeEnv = SNAPSHOT_ABSOLUTE_ENV.filter((name) => typeof o.env[name] === "string" && o.env[name] !== "" && !path.isAbsolute(o.env[name]));
      if (relativeEnv.length) { o.err(`${tag} BLOCKED: ${relativeEnv.map((n) => `${n}=${o.env[n]}`).join(", ")} ${relativeEnv.length > 1 ? "are" : "is"} relative; a snapshot compile inherits CARGO_HOME, sccache and temp only as absolute paths, never resolved inside the snapshot; nothing compiled`); return 1; }
      // The runtime cargo wrapper (item 7): first on PATH and named in CARGO for every scan, it binds each real cargo invocation to its
      // own workspace's checkout target directory or refuses it (RK_CARGO_*), whatever path reaches cargo. Direct POSIX hosts only (a
      // broker host never reaches here; a direct Windows host keeps the fail-closed static walk). Removed when the gate ends (cleanup).
      if (o.platform !== "win32" && rust.workspaces.length) {
        const realCargo = which("cargo").find((c) => path.isAbsolute(c));
        if (!realCargo && !snapshotBroker) { o.err(`${tag} BLOCKED: no cargo on PATH to wrap for the snapshot compile; nothing compiled`); return 1; }
        try {
          const shimModule = await import("../cargo-target-shim.mjs");
          let broker = null;
          if (snapshotBroker) {
            // Broker mode (the Mac, decision C): every cargo a test reaches is its own carried request. The gate's own snapshot-to-checkout
            // map is the only source of a request's primaryCwd, and broker work may exist from here on: the ownership records it first.
            const checkoutRoot = fs.realpathSync.native(root);
            const checkoutDirs = Object.fromEntries(rust.workspaces.map((ws) => [ws.cargoDir, path.join(checkoutRoot, ...(ws.cargoDir === "." ? [] : ws.cargoDir.split("/")))]));
            const marked = snapshotBroker.markBrokerSubmit();
            if (marked?.broker?.submitted !== true || marked.handle !== snapshotBroker.handle) throw new Error(`the build ownership did not record the broker submit under ${snapshotBroker.handle}`);
            const { brokerSection } = await import("../snapshot-broker.mjs");
            broker = { checkoutDirs, section: brokerSection(snapshotBroker) };
          }
          cargoShim = shimModule.installCargoShim({ base: os.tmpdir(), snapshotRoot: snapshotB, targetDirs, broker, realCargo: realCargo ?? null, platform: o.platform, onDir: (dir) => { cargoShimDir = dir; } });
          cargoShim.scanEnv = shimModule.shimScanEnv(o.env, cargoShim);
        } catch (error) { o.err(`${tag} BLOCKED: the runtime cargo wrapper cannot be installed (${error.message}); nothing compiled`); return 1; }
        o.out(`${tag} runtime cargo wrapper ${cargoShim.shim} (identity ${cargoShim.identity.slice(0, 16)}, ${cargoShim.mode}) -> ${cargoShim.mode === "broker" ? "the RustRight broker, one carried request per call" : realCargo}: each cargo invocation ${cargoShim.mode === "broker" ? "is carried with its own receipt, verified by the gate," : "binds its workspace's checkout target directory"} or is refused`);
      }
    }

    // Class B scans (compile, lint, tests) run in the live checkout, so it must equal HEAD where they read it: an uncommitted fix could
    // otherwise hide a committed failure. Class A reads only the snapshot.
    if (!planOnly && !classAOnly && !snapshotB) {
      const npmRootFiles = ["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc", ...ESLINT_CONFIG_FILES, ...BIOME_CONFIG_FILES];
      const inputs = [...new Set([...rust.units.map((u) => u.dir), ...npm.map((u) => u.dir), ...rust.workspaces.map((w) => w.cargoDir), "ci", "scripts", "vendor",
        // The root manifest, lock and deny.toml only where a workspace is rooted there; a crate-per-workspace repository binds each crate's own
        // (its directory is an input above, which holds its Cargo.toml, Cargo.lock, deny.toml, .cargo and .rightkit).
        ...(rust.workspaces.some((w) => w.cargoDir === ".") ? ["Cargo.toml", "Cargo.lock", "deny.toml"] : []),
        ".cargo", "rust-toolchain.toml", "rust-toolchain", "clippy.toml", "rustfmt.toml", ...npmRootFiles, ...npmRoots.filter((r) => r !== ".").flatMap((r) => npmRootFiles.map((f) => `${r}/${f}`))])];
      // Batched like cargoInputsDiffer: one argv with every input path overflows Windows' command line in a large repository.
      const dirtyRuns = pathspecBatches(inputs).map((batch) => git("status", "--porcelain", "--untracked-files=all", "--", ...batch));
      const dirtyFailed = dirtyRuns.find((r) => failed(r));
      const dirtyFiles = dirtyFailed ? [`git status failed: ${why(dirtyFailed)}`] : dirtyRuns.flatMap((r) => text(r).split(/\r?\n/)).filter(Boolean).filter((l) => !l.includes("node_modules"));
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

    // A broker host: rustright refuses every cargo command but a locked metadata outside the app's primary checkout
    // (RK_NOT_PRIMARY_CHECKOUT), so class A's cargo scans cannot read the export there. They run in the primary checkout, allowed only
    // when every Cargo input in it is blob-identical to the snapshot's; a live edit to a manifest, lock, deny.toml or .cargo/ refuses
    // the check with the paths named. A direct host (cloud, CI) keeps reading the export. (rightkit#1215, third cause)
    let cargoLiveRoot = null;
    if (classAOnly && snapshot && executor.kind === "broker" && manifests.length) {
      const differing = cargoInputsDiffer({ root, snapshot, manifests, spawn: (c, a, opts) => o.spawn(c, a, opts) });
      if (differing.length) {
        const resolved = git("rev-parse", "--verify", String(snapshot));
        o.err(`${tag} BLOCKED: the broker scans Cargo inputs in the primary checkout, and these differ from ${(failed(resolved) ? String(snapshot) : text(resolved).trim()).slice(0, 12)}: ${differing.join(", ")}; commit or restore them, then run the check again`);
        return 1;
      }
      // A snapshot check's cargo scans then read the checkout's WORKSPACE, which can hold more than its tracked inputs: an untracked
      // member, or a stray directory a members glob (crates/*) matches. Cargo's own member list there must equal the commit's (from
      // the snapshot), or the verdict would not be the commit's (Oracle review of 79c4427b). A checkout cargo cannot read refuses too.
      if (snapshotA) {
        const memberDiff = [];
        const membersOf = (base, dir) => workspaceMetadata({ root: base, spawn: (c, a, opts) => o.spawn(c, a, opts), cargoCommand, dir, realpath: fs.realpathSync }).members.map((m) => m.manifest).sort();
        for (const ws of rust.workspaces) {
          let inCommit; let inCheckout;
          try { inCommit = membersOf(repo, ws.cargoDir); } catch (e) { memberDiff.push(`${ws.cargoDir} (commit): ${e.message}`); continue; }
          try { inCheckout = membersOf(root, ws.cargoDir); } catch (e) { memberDiff.push(`${ws.cargoDir} (checkout): ${e.message}`); continue; }
          const extra = inCheckout.filter((m) => !inCommit.includes(m)); const missing = inCommit.filter((m) => !inCheckout.includes(m));
          if (extra.length || missing.length) memberDiff.push(`${ws.cargoDir}: checkout-only ${extra.join(", ") || "none"}; commit-only ${missing.join(", ") || "none"}`);
        }
        if (memberDiff.length) {
          o.err(`${tag} BLOCKED: the broker scans Cargo in the primary checkout, and cargo's workspace there is not the commit's: ${memberDiff.join("; ")}; remove or commit what differs, then run the check again`);
          return 1;
        }
      }
      cargoLiveRoot = root;
    }
    // One Cargo workspace and lock per crate (rightkit#1285): the reach between crates is the manifests' path dependencies, read from the
    // gated snapshot. A crate that is selected selects its transitive dependents (checked, not re-linted); a vendored source it patches
    // in selects the crate. Neither is derivable from a single lock, so a layout that cannot read them is refused, never planned narrower.
    let crateDependents = null; let externalInputs = null;
    const crateUnits = rust.units.filter((u) => u.cargoDir === `crates/${u.crate}` && u.name === u.crate);
    if (crateUnits.length) {
      try {
        const { crateWorkspaces, pathDependencies, reverseDependents } = await import("../crate-workspaces.mjs");
        const io = { readFile: (f) => o.readFile(f), exists: (f) => o.exists(f), readdir: (d) => o.readdir(d) };
        const known = new Set(crateUnits.map((u) => u.name));
        const graph = new Map();
        for (const [name, set] of pathDependencies(repo, io, crateWorkspaces(repo, io))) if (known.has(name)) graph.set(name, new Set([...set].filter((d) => known.has(d))));
        crateDependents = (names) => reverseDependents([...names].filter((n) => known.has(n)), graph);
        externalInputs = vendorOwners(crateUnits, (u) => o.readFile(path.join(repo, u.dir, "Cargo.toml")));
      } catch (e) { o.err(`${tag} BLOCKED: cannot read the path dependencies between the crate workspaces (${e.message}); their reverse dependents would go unchecked, so nothing is planned`); return 1; }
    }
    // A broker snapshot compile: its cargo scans are carried requests that name the commit's snapshot and run from the primary checkout
    // (rustright refuses cargo anywhere else); the checkout is the request's working directory, never its source.
    if (snapshotBroker && manifests.length) cargoLiveRoot = root;
    let fullPlan;
    fullPlan = planScans({
      // Class A of a commit or tree reads only that snapshot: every scan, cargo's included, runs in the export, never in the live
      // checkout (which may hold other bytes). Compiles (class B) use the live checkout, whose tree the dirty check pinned to HEAD.
      changed, contentRoot: repo, liveRoot: (classAOnly && snapshot) || snapshotB ? repo : root, trustedRoot: snapshotA ?? snapshotB ?? root, includeTests: true, rustScope, cargoCommand, targetDirs, cargoShim: Boolean(cargoShim),
      tier: fullA ? "release" : (option("--rule-tier") ?? "compile"), cargoLiveRoot,
      // The quiet tests (port, pipe, desktop and wall-clock bound) are proven by the CI shards, one crate per runner, unless
      // RK_GATE_QUIET_TESTS=local asks for the serial quiet lane here (no CI, or a CI outage).
      // Full class A runs every test before a compile: the quiet suites run here in the quiet lane, never left to CI.
      // The door's snapshot compile already executes the repository's own code (its tests) in the verified snapshot, guarded like
      // full class A, so the docs generator runs there too instead of being deferred (step 4c review).
      quietTests: fullA || o.env.RK_GATE_QUIET_TESTS === "local" ? "local" : "ci", packageFeatures, liveExecution: (fullA && (args.includes("--worktree") || Boolean(snapshotA))) || Boolean(snapshotB),
      rustUnits: rust.units, npmUnits: npm, npmRoots, cargoDir: ".", rustLocks, crateFeatures, crateDependents, externalInputs,
      // The broker (or, on a direct host, the gate itself) admits compiles; no separate grant file decides it here.
      localCompile: true,
      exists: o.exists, readFile: o.readFile, platform: o.platform,
    });
    // A broker-managed host whose broker is unreachable runs no class B scan: npm tests and scripts can compile native code the
    // snapshot lists no manifest for (Astra, second pass); the class A scans never compile and still run.
    // Class A tests run code too (Astra, second pass: a test script can compile native code its text does not show), so a host whose
    // broker is unreachable runs neither; the gate stops rather than skip them.
    if (executor.kind === "refuse" && fullPlan.scans.some((x) => x.class === "B" || (x.kind === "test" && (!classAOnly || fullA)))) { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
    let plan = fullPlan;
    const jobs = Math.max(1, Number(o.env.RK_GATE_JOBS ?? 4) || 4);
    const testJobs = Math.max(1, Number(o.env.RK_GATE_TEST_JOBS ?? jobs) || jobs);
    const classBJobs = Math.max(1, Number(o.env.RK_GATE_CLASS_B_JOBS ?? 3) || 3);

    if (planOnly) {
      o.out(`${tag} plan: ${plan.scans.length} scan(s) for ${changed.length} changed file(s) vs ${base.slice(0, 12)} (reads the working tree; nothing runs)`);
      // A crate that is its own workspace is named by its unit (rust units below); only the other workspaces are listed.
      const wsList = rust.workspaces.filter((w) => !(w.units.length === 1 && w.units[0].name === w.units[0].crate && w.cargoDir === `crates/${w.units[0].crate}`)).map((w) => w.cargoDir);
      const wsNote = wsList.length === 1 ? (wsList[0] === "." ? "" : ` (workspace ${wsList[0]})`) : wsList.length > 1 ? ` (workspaces ${wsList.join(", ")})` : "";
      o.out(`${tag} rust units: ${rust.units.map((u) => `${u.name}=${u.dir}`).join(", ") || "none"}${wsNote}; npm units: ${npm.map((u) => `${u.name}=${u.dir}`).join(", ") || "none"}`);
      for (const s of plan.scans) o.out(`  [${s.class}] ${s.id} :: ${s.command}${plan.deferred.includes(s.id) ? " [deferred: no local compile grant]" : ""}`);
      for (const n of plan.notes) o.out(`${tag} note: ${n}`);
      return 0;
    }

    for (const n of plan.notes) o.out(`${tag} note: ${n}`);
    o.out(`${tag} executor: ${executor.kind}${executor.kind === "broker" ? ` (${cargoCommand.join(" ")})` : " (no broker on this host: cargo runs directly, through the same gates)"}`);
    const quietCount = plan.scans.filter((s) => s.quiet === true).length;
    o.out(`${tag} ${plan.scans.length} scan(s) for ${changed.length} changed file(s) vs ${base.slice(0, 12)}; class A scans ${jobs} at a time, class B scans ${classBJobs} component(s) at a time${quietCount ? `, ${quietCount} quiet test scan(s) last under the broker's quiet lease` : ""}`);
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
    // The result cache keys on the gated commit's object ids; the stdin check above made HEAD the pushed commit.
    let cache = null;
    // Full class A on the working tree (the door, and broker admission of the live checkout) caches on the LIVE bytes: every
    // tracked and untracked (not ignored) file is hashed raw, so the door's run and a repeat admission of the same bytes hit the
    // cache, and any edit misses it.
    const liveFullA = fullA && args.includes("--worktree");
    // Full class A of a snapshot binds like the live tree does: its bytes are the commit's (blob ids from the object database), its
    // environment includes the dependencies installed into it below.
    const snapFullA = fullA && Boolean(snapshotA);
    const boundFullA = liveFullA || snapFullA;
    const liveBase = snapFullA ? snapshotA : root;
    // The live bytes full class A judges: every tracked and untracked (not ignored) file, hashed raw. Computed whether or not the
    // cache is used; a failure to compute it blocks (the verdict must name the bytes it covers).
    const liveTree = () => {
      const listed = git("ls-files", "-z", "--cached", "--others", "--exclude-standard");
      if (failed(listed)) throw new Error(`cannot list the working tree: ${why(listed)}`);
      const files = [...new Set(text(listed).split("\0").filter(Boolean).map(posix))].filter((f) => { try { return fs.statSync(path.join(root, f)).isFile(); } catch { return false; } }).sort();
      const ids = new Map();
      for (const batch of pathspecBatches(files, 60000)) {
        const r = o.spawn("git", ["hash-object", "--no-filters", "--stdin-paths"], { cwd: root, input: `${batch.join("\n")}\n` });
        if (failed(r)) throw new Error(`git hash-object failed: ${why(r)}`);
        const out = text(r).split(/\r?\n/).filter(Boolean);
        if (out.length !== batch.length) throw new Error("git hash-object returned a short list");
        batch.forEach((f, i) => ids.set(f, out[i]));
      }
      const digestOf = (list) => createHash("sha256").update(list.map((f) => `${f}\t${ids.get(f)}\n`).join("")).digest("hex");
      return { files, ids, digestOf, digest: digestOf(files) };
    };
    let live = null;
    if (liveFullA) {
      try { live = liveTree(); } catch (error) { o.err(`${tag} BLOCKED: cannot bind full class A to the working tree's bytes (${error.message})`); return 1; }
    }
    let snapshotCommitId = null;
    if (snapFullA) {
      const sc = await import("../snapshot-check.mjs");
      try {
        const checkout = fs.realpathSync.native(root);
        const ct = sc.commitOf(checkout, String(snapshot), o.spawn);
        snapshotCommitId = ct;
        sc.verifySnapshotDir({ checkout, dir: snapshotA, ...ct, when: "before class A" });
        live = sc.commitFiles(checkout, ct.commit, o.spawn);
        // Dependencies for the checks that execute repository code: installed into the snapshot from the commit's lockfile and the
        // primary checkout's existing store (coordinator decision A, rightkit#1257). A store miss stops here, naming the package.
        const installed = sc.installSnapshotDependencies({ checkout, snapshot: snapshotA, roots: npmRoots, env: o.env, spawn: o.spawn });
        for (const r of installed) o.out(`${tag} ${sc.describeInstall(r)}`);
      } catch (error) { o.err(`${tag} ${error.code === "RK_CHECK_ERROR" ? "RK_CHECK_ERROR" : "BLOCKED"}: ${error.message}`); return error.code === "RK_CHECK_ERROR" ? 2 : 1; }
    }
    const liveSnapshot = live?.digest ?? null;
    // The environment, installed dependencies included, is bound at the start and compared at the end, so a dependency that
    // changes while class A runs voids the verdict.
    // envStartWithCaches records, for audit, the tool caches as the run found them (what its checks could read); the pass line
    // carries it next to the env the verdict binds.
    let envAtStart = null; let envStartWithCaches = null; let cachesAtStart = null;
    if (boundFullA) {
      const orphans = orphanedPnpmStores({ root: liveBase, files: live.files });
      if (orphans.length) { o.err(`${tag} BLOCKED: orphaned nested pnpm store(s) ${orphans.join(", ")}: left by a standalone install in a workspace member; they can hold stale dependencies and make concurrent vitest runs clear their shared module cache under each other. Move them aside (or reinstall) and run again.`); return 1; }
      try { envAtStart = classAEnvironmentDigest({ root: liveBase, gatePackage: o.gatePackage, files: live.files, spawn: o.spawn, includeCaches: false }); envStartWithCaches = classAEnvironmentDigest({ root: liveBase, gatePackage: o.gatePackage, files: live.files, spawn: o.spawn }); cachesAtStart = toolCacheManifest({ root: liveBase, files: live.files }); }
      catch (error) { o.err(`${tag} BLOCKED: ${error.message}`); return 1; }
    }
    if (boundFullA && !args.includes("--no-cache") && o.env.RK_GATE_CACHE !== "0") {
      try {
        const common = git("rev-parse", "--git-common-dir");
        if (failed(common)) throw new Error("no git directory");
        const { files, ids, digestOf } = live;
        const under = (dir) => (dir === "." || dir === "" ? files : files.filter((f) => f === dir || f.startsWith(`${dir}/`)));
        const objectId = (p) => (ids.has(p) ? ids.get(p) : under(p).length ? digestOf(under(p)) : null);
        const store = fileScanCache(path.join(path.resolve(root, text(common).trim()), "rightkit-gate-cache"));
        const listFiles = (dir) => under(dir);
        const readBlob = (p) => fs.readFileSync(path.join(liveBase, p), "utf8");
        // The verdict also depends on what is not in the tree: the gate's own code and advisory data, the node and pnpm that run
        // the checks, the installed dependencies (pnpm's install state) and the platform. Any change there is a cache miss.
        const environment = classAEnvironmentDigest({ root: liveBase, gatePackage: o.gatePackage, files, spawn: o.spawn });
        cache = { ...store, digest: `${buildCacheDigest({ objectId, headTree: digestOf(files), rustUnits: rust.units, npmUnits: npm, cargoDir: ".", rustLocks, crateFeatures, listFiles, readBlob })}:${environment}` };
      } catch { cache = null; }
    }
    if (!classAOnly && !args.includes("--no-cache") && o.env.RK_GATE_CACHE !== "0") {
      try {
        const headSha = snapshotBCommit?.commit ?? text(git("rev-parse", "HEAD")).trim();
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
    // The direct compile's freshness census (row 7.19): the fingerprint units before the scans, and the fingerprint log of each cargo
    // process that compiles (CARGO_LOG on that process only, read from its own standard error). A broker host's census is the broker's.
    const census = snapshotB && !snapshotBroker ? createFreshnessCensus({ targetDirs }) : null;
    directTiming.cargoStart = Date.now();
    census?.start();
    const scanResult = await (o.runScans ?? runScans)(plan, {
      classes: classAOnly ? ["A"] : ["A", "B"], jobs, testJobs, classBJobs, signal: ac.signal, ...(cache ? { cache } : {}),
      ...(census ? { spawn: census.wrapSpawn(defaultSpawnAsync) } : {}),
      // The runtime cargo wrapper is first on PATH and named in CARGO for every scan of a snapshot compile; CARGO (a path named by the
      // wrapper's identity) is in every scan's cache key, so no result cached without the wrapper or with another map is reused.
      ...(cargoShim ? { env: cargoShim.scanEnv, ...(cargoShim.sentinel ? { cargoSentinel: cargoShim.sentinel } : {}), cargoLedger: { dir: cargoShim.ledgerDir } } : {}),
      // The broker snapshot compile's seam (item 7 rows 7.20 to 7.24): the commit, snapshot, ownership, enrolled pins and receipt reader.
      ...(snapshotBroker ? { snapshotBroker } : {}),
      // `check` (and the broker's compile admission, which runs it on a captured copy of the tree without node_modules) judges the
      // content: it never executes the repository's own code. The class A checks that do (tests, typecheck, lint, generated docs:
      // `executes`) run in the full gate (--all, the push hook, the door's compile stage), after the content checks and before any compile.
      ...(classAOnly && !fullA ? { exclude: (s) => s.executes === true } : {}),
      // A snapshot check names its commit; it never reads the checkout's HEAD.
      quietLane: quietLeaseLane(o, { tag, reason: `rightkit-gate-quiet-tests@${snapshotCommitId ? snapshotCommitId.commit.slice(0, 12) : snapshotBCommit ? snapshotBCommit.commit.slice(0, 12) : text(git("rev-parse", "--short", "HEAD")).trim()}` }),
      // On a broker-managed host the quiet suites need the lease or they are refused (never run unleashed).
      requireQuietLease: executor.kind === "broker",
      // Failing scans' complete output: never inside the repository, on the host's scratch root (RIGHTKIT_SCRATCH_ROOT, else the
      // managed data root's runs/), in a folder unique to this run so evidence is never overwritten.
      scanLogDir: (() => {
        try {
          const base = o.env.RIGHTKIT_SCRATCH_ROOT || path.join(resolveManagedRoots({ env: o.env }).dataRoot, "runs");
          const stamp = new Date().toISOString().replace(/[:.]/g, "-");
          return path.join(base, "rightkit-gate-scan-logs", `${path.basename(root)}-${stamp}-${process.pid}`);
        } catch { return null; }
      })(),
      onResult: (row, p) => o.out(`${tag} [${p.done}/${p.total}] ${(row.cached ? "cached" : row.status).padEnd(7)} ${row.id} (${p.seconds}s)${row.status === "fail" ? ` :: ${String(row.detail ?? "").slice(0, 300)}` : ""}`),
    });
    directTiming.cargoEnd = Date.now();
    const freshness = census ? census.finish() : null;
    directTiming.diagnostics = Date.now() - directTiming.cargoEnd;
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
    // What a full class A verdict binds, green or red: the bytes it judged, unchanged from start to end, and the environment it judged
    // in (gate, tools, installed dependencies, tool caches), unchanged too. A verdict whose binding moved covers nothing: a green one is
    // refused and a red one names no reusable deterministic cause (rightkit#1257). Returns { env, notes } or { prefix, problem, exit }.
    const judgedBinding = async () => {
      if (liveFullA) {
        let digestNow = null;
        try { digestNow = liveTree().digest; } catch { digestNow = null; }
        if (digestNow !== liveSnapshot) return { prefix: "BLOCKED", exit: 1, problem: "the working tree changed while class A ran; the verdict does not cover the current bytes. Run again." };
      }
      if (snapFullA) {
        try { (await import("../snapshot-check.mjs")).verifySnapshotDir({ checkout: fs.realpathSync.native(root), dir: snapshotA, ...snapshotCommitId, when: "after class A" }); }
        catch (error) { return { prefix: "RK_CHECK_ERROR", exit: 2, problem: `${error.message}; the verdict covers no known bytes` }; }
      }
      if (!live) return { env: null, notes: [] };
      let envDigest; let envNoCaches;
      try { envDigest = classAEnvironmentDigest({ root: liveBase, gatePackage: o.gatePackage, files: live.files, spawn: o.spawn }); envNoCaches = classAEnvironmentDigest({ root: liveBase, gatePackage: o.gatePackage, files: live.files, spawn: o.spawn, includeCaches: false }); }
      catch (error) { return { prefix: "BLOCKED", exit: 1, problem: error.message }; }
      const notes = [];
      if (cachesAtStart !== null) {
        let drift;
        try { drift = toolCacheDrift(cachesAtStart, toolCacheManifest({ root: liveBase, files: live.files })); } catch (error) { return { prefix: "BLOCKED", exit: 1, problem: error.message }; }
        if (drift.changed.length) return { prefix: "BLOCKED", exit: 1, problem: `${drift.changed.length} tool cache file(s) that existed when class A started changed or vanished while it ran, so the checks may have read bytes the verdict cannot name: ${drift.changed.slice(0, 5).join(", ")}${drift.changed.length > 5 ? ", …" : ""}. Run again.` };
        if (drift.added.length) notes.push(`note: ${drift.added.length} tool cache file(s) appeared while class A ran (by it or another process), bound in env as they stand: ${drift.added.slice(0, 5).join(", ")}${drift.added.length > 5 ? ", …" : ""}`);
      }
      if (envAtStart !== null && envNoCaches !== envAtStart) return { prefix: "BLOCKED", exit: 1, problem: "the class A environment (gate, tools or installed dependencies) changed while class A ran; the verdict does not cover it. Run again." };
      return { env: envDigest, notes };
    };
    if (snapshotB) {
      // The compile covers the commit only if the snapshot still is its tree after every scan ran (E6).
      directTiming.afterStart = Date.now();
      try {
        // With the door's ledger the verify returns { tree, ledger: admitted } (item 5 row 5.09d, snapshot-check.mjs); the entries it admitted
        // are handed to the door, which holds them against its own walk at the stage's end (closeSnapshotStage). Without a ledger it returns the tree id.
        const verified = (await import("../snapshot-check.mjs")).verifySnapshotDir({ checkout: fs.realpathSync.native(root), dir: snapshotB, ...snapshotBCommit, when: "after the compile", ...(o.snapshotLedger ? { ledger: o.snapshotLedger, admitNew: true } : {}) });
        if (o.snapshotLedger) o.onCompileLedger?.(Array.isArray(verified?.ledger) ? verified.ledger : null);
      }
      catch (error) { o.err(`${tag} RK_CHECK_ERROR: ${error.message}; the compile covers no known bytes`); return 2; }
      directTiming.afterEnd = Date.now();
      o.out(`${tag} snapshot ${snapshotB} is still tree ${snapshotBCommit.tree} after the compile`);
      // A broker host's evidence is the broker's own signed receipts (each carried scan's row.snapshotEvidence, verified above); the
      // gate writes no receipt that says `executor: direct` for it.
      if (snapshotBroker) o.out(`${tag} broker compile: ${scanResult.scans.filter((r) => r.snapshotEvidence).length} carried request(s) with verified signed receipts; no direct receipt on a broker host`);
      else {
        // The direct host's own receipt (GATE-CONTRACT "Broker interface", row 7.18): the contract's returned-evidence names minus the
        // broker-only ones (admission, queue, reusedFrom): the run, the commit, the snapshot's tree at start and exit, the declared
        // inputs digested in place, the verdict, the freshness census (7.19), the timings, the exit status, the structured
        // diagnostics, the resolved output directory (7.09, with its inputs) and `refreshed` (nothing is reused on a direct host).
        // The gate refuses its own receipt unless it verifies, so a consumer never meets a receipt with a name missing.
        const requestId = randomUUID();
        const exitCode = failures.length ? 1 : 0;
        const diagnosticsDir = (() => {
          try { return path.join(o.env.RIGHTKIT_SCRATCH_ROOT || path.join(resolveManagedRoots({ env: o.env }).dataRoot, "runs"), "rightkit-gate-direct-diagnostics"); } catch { return path.join(os.tmpdir(), "rightkit-gate-direct-diagnostics"); }
        })();
        let directReceipt;
        try {
          const wrote = Date.now();
          const diagnosticsPath = writeDirectDiagnostics({ dir: diagnosticsDir, requestId, commit: { sha: snapshotBCommit.commit, tree: snapshotBCommit.tree }, exit: exitCode, scans: scanResult.scans, failures, deferrals, notes: [...scanResult.notes, ...directDeclaredNotes], freshness });
          directReceipt = buildDirectReceipt({
            requestId, issuedAt: new Date().toISOString(), commit: { sha: snapshotBCommit.commit, tree: snapshotBCommit.tree },
            snapshotPath: snapshotB, treeAtStart: snapshotBCommit.tree, treeAtExit: snapshotBCommit.tree, ownershipHandle: o.buildOwnership?.handle ?? null,
            declaredInputDigests: directDeclared, verdict: directVerdict({ scans: scanResult.scans, failures, deferrals }), freshness,
            timingsMs: { bind: directTiming.bindEnd - directTiming.bindStart, cargo: directTiming.cargoEnd - directTiming.cargoStart, afterExitBind: directTiming.afterEnd - directTiming.afterStart, diagnostics: directTiming.diagnostics + (Date.now() - wrote) },
            exit: exitCode, diagnosticsPath, outputDirs: targetDirs ?? {}, outputResolution,
            cargoWrapper: cargoShim ? { identity: cargoShim.identity, workspaces: cargoShim.workspaces } : null,
          });
        } catch (error) { o.err(`${tag} BLOCKED: RK_DIRECT_RECEIPT: the direct receipt cannot be written (${error.message}); the compile's evidence is incomplete`); return 1; }
        const receiptProblems = verifyDirectReceipt(directReceipt);
        if (receiptProblems.length) { o.err(`${tag} BLOCKED: RK_DIRECT_RECEIPT: the direct receipt does not verify: ${receiptProblems.join("; ")}`); return 1; }
        o.onDirectReceipt?.(directReceipt);
        o.out(`${tag} direct receipt ${JSON.stringify(directReceipt)}`);
      }
    }
    if (failures.length) {
      o.err(`${tag} BLOCKED (${failures.length})`);
      for (const f of failures) o.err(`  - ${f}`);
      // A red full class A names its cause when every failure is one a re-run of the same inputs reproduces AND the bytes and
      // environment it was judged in held from start to end, so the broker may reuse it (rightkit#1257); a live scan, a timeout, a
      // missing tool, anything undecided, or a binding that moved names none.
      if (boundFullA) {
        const binding = await judgedBinding();
        if (binding.problem) {
          o.err(`${tag} ${binding.prefix}: ${binding.problem} No deterministic verdict is named.`);
          if (binding.exit === 2) return 2;
        } else {
          for (const n of binding.notes) o.out(`${tag} ${n}`);
          const marker = (await import("../snapshot-check.mjs")).deterministicMarker(scanResult, binding.env);
          if (marker) o.out(marker);
        }
      }
      o.err(classAOnly ? "Class A is red: nothing compiles until it is green." : "Fix and push again. Never bypass with --no-verify.");
      return 1;
    }
    if (fullA) {
      // The verdict is for the bytes that were checked, in the environment that was checked: either moving makes it a verdict for nothing.
      const binding = await judgedBinding();
      if (binding.problem) { o.err(`${tag} ${binding.prefix}: ${binding.problem}`); return binding.exit; }
      for (const n of binding.notes) o.out(`${tag} ${n}`);
      // The bytes this verdict covers, for the broker to compare with the snapshot it compiles: the tracked files only (what a
      // captured snapshot holds), and every file including untracked ones. Each is sha256 over sorted "<path>\t<git blob id of the
      // raw bytes>\n" lines (git hash-object --no-filters), so the broker can recompute it over its snapshot without this gate.
      if (live) {
        // A snapshot holds only the commit's files: every one is tracked.
        const trackedList = snapFullA ? null : git("ls-files", "-z", "--cached");
        if (trackedList && failed(trackedList)) { o.err(`${tag} BLOCKED: cannot list tracked files to name the checked bytes`); return 1; }
        const tracked = new Set(snapFullA ? live.files : text(trackedList).split("\0").filter(Boolean).map(posix));
        o.out(`${tag} class A bytes tracked=${live.digestOf(live.files.filter((f) => tracked.has(f)))} all=${live.digest}${envStartWithCaches ? ` env-at-start=${envStartWithCaches}` : ""} env=${binding.env}`);
      }
      o.out(`${tag} class A full pass: ${scanResult.scans.length} deterministic check(s), the ones that execute repository code included, over ${changed.length} file(s) of ${base}`);
      return 0;
    }
    if (classAOnly) {
      // `check` judged the content only: the class A checks that execute the repository's code did not run here, so this is not a
      // full class A pass, and it says so (reviewer, 2026-10-27). The full gate runs them before any compile.
      const notRun = plan.scans.filter((s) => s.class === "A" && s.executes === true).map((s) => s.id);
      if (notRun.length) o.out(`${tag} class A partial: ${notRun.length} check(s) that execute repository code (tests, typecheck, lint, generated docs) run in the full gate, not in check: ${notRun.slice(0, 8).join(", ")}${notRun.length > 8 ? ", …" : ""}`);
      o.out(`${tag} class A content pass: ${scanResult.scans.length} deterministic scan(s) over ${changed.length} file(s) of ${base}`);
      return 0;
    }
    o.out(`${tag} pass (${changed.length} changed file(s) vs ${base}); ${deferrals.length} scan(s) deferred, not passed${scanResult.cacheHits.length ? `; ${scanResult.cacheHits.length} reused from the result cache` : ""}`);
    return 0;
  } finally {
    for (const [sig, handler] of signals) process.off(sig, handler);
    cleanup();
  }
}

// The inherited locations a snapshot compile requires as absolute paths (GATE-CONTRACT "Output directory & cache").
const SNAPSHOT_ABSOLUTE_ENV = ["CARGO_HOME", "RUSTUP_HOME", "SCCACHE_DIR", "TMPDIR", "TMP", "TEMP"];

/**
 * What the production entry (`node rightkit-gate.mjs ...`) hands `main` as its injected dependencies, and what `--route` reports: the
 * snapshot carrier on Mac and Dell (rightkit#1301: right-release's own Cargo calls, the Tauri CLI's and cargo-about's are carried, so
 * every stage compiles the commit's snapshot through the broker); nothing on Linux, which compiles direct with no broker. Tests inject
 * their own carrier through `main`; the contract test (door-route.test.mjs) runs the real entry. `platform` is the host whose deps are
 * asked for (the real entry passes none).
 */
export async function productionEntryDeps({ platform = process.platform } = {}) {
  if (directPlatform(platform)) return {};
  const { PRODUCTION_SNAPSHOT_CARRIER } = await import("../snapshot-broker.mjs");
  return { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER };
}

const invokedDirectly = (() => {
  try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; }
})();
/** This gate's own resolved entry file: the broker admits a compile only when the request names the pinned trusted gate by this
 * path (rustright reads it from RUSTRIGHT_CALLING_GATE; a missing or different path is refused). */
export const callingGatePath = () => fs.realpathSync(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  // Every child this run starts that can reach rustright (scans, unit stages, right-release dev/test, their builds) inherits it.
  process.env.RUSTRIGHT_CALLING_GATE = callingGatePath();
  delete process.env.RIGHTKIT_CALLING_GATE; // one spelling, never both
  // Production carries the snapshot carrier on Mac and Dell (rightkit#1301): every child that reaches Cargo is carried, so every stage
  // compiles the commit's snapshot. Linux takes the snapshot route direct, with no carrier. Tests inject theirs.
  productionEntryDeps().then((deps) => main(process.argv.slice(2), deps)).then((code) => process.exit(code), (e) => { console.error(`rightkit-gate: ${e?.stack ?? e}`); process.exit(2); });
}
