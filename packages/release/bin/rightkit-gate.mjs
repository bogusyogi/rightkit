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
//   rightkit-gate check [--repo <dir>] [--commit <rev> | --worktree] [--rule-tier compile|release] [--only <rule,...>] [--list] [--print-baseline] [--baseline <file>]
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
//                                             node_modules/.bin, then any workspace package's (each package that declares @rightkit/release by
//                                             name when the root does not declare it), then pnpm exec, then node at a stable path inside the
//                                             repository (hookGatePlan: from the declarations, never from what is installed). Where @rightkit/release is
//                                             installed it also writes the advisory ownership pre-commit step (`right-release hooks install`
//                                             is an alias of this command)
//
// From the pre-push hook, git's ref lines arrive on stdin: the commit being pushed must be the checked-out HEAD (broker scans
// read the live checkout), and the remote's current commit is the diff base. To push an earlier commit of the local stack,
// check it out first (a detached HEAD is fine), push HEAD:<branch>, return.
// The executor is chosen by the gate, never the caller (gate-executor.mjs resolveExecutor): the RustRight broker where this machine
// has it (`rustright` on PATH, or its managed cargo shim), a refusal on a broker-managed machine whose broker is unreachable, and
// direct cargo on any other host (a cloud container, a CI runner). Every host runs the same gates in the same order (app rules, class A,
// the compile scans); an unmanaged host is never told to find a broker. The pre-push hook judges every pushed ref.
// ci/gate.config.json `rust` (optional) sets per-crate feature sets, exactly as CI's gate reads it; `npm.<unit>` sets `quietTests` and
// `requiresBroker: { "reason": "..." }` (a test the package scripts do not show reaching the broker: node -e, node scripts, sh -c, pnpm --dir).
// Result cache: a passing class B scan is reused only when its scan id, command, tool version, environment and input digest
// (the gated commit's git object ids for the crate's workspace closure, the shared build inputs, ci/gate.config.json and every
// path the closure's sources include from outside their crate; the whole HEAD tree for Rust tests unless ci/gate.config.json
// rust.<crate>.testInputs lists the paths they read, for npm, node and docs scans, and whenever a reference cannot be resolved
// statically) are all identical. Stored in <git common dir>/rightkit-gate-cache; a hit prints
// `cached` and is listed in the result's cacheHits. Failures, deferrals and class A scans are never cached.
// RK_GATE_JOBS (default 4) sets class A scan concurrency; class B runs one scan at a time per checkout.
// A crate whose ci/gate.config.json entry says `quietTests: true` has its test scans run last,
// one at a time, under the RustRight quiet lease (`rustright quiet acquire`, RUSTRIGHT_QUIET_LEASE in their environment).
// SIGINT/SIGTERM stop scheduling, kill every running scan's process tree and exit non-zero listing the scans not run.
import fs from "node:fs";
import { createHash } from "node:crypto";
import { resolveManagedRoots } from "../managed-roots.mjs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { BIOME_CONFIG_FILES, ESLINT_CONFIG_FILES, WORKSPACE_RUST_FILE, ancestorConfigChanged, ancestorDirsOf, canonicalJson, defaultSpawn, digestScanSet, fileScanCache, findRustfmt, metadataArgs, planScans, runScans, scanDeferrals, scanFailures } from "../deterministic-scans.mjs";
import { changedLockNames, parseLock, reach } from "../lock-graph.mjs";
import { resolveExecutor } from "../gate-executor.mjs";
import { directPlatform } from "../managed-cargo.mjs";
import { bindingOf, endRun, readReceipt, RECEIPT_SCHEMA, receiptKey, sameBinding, sameSource, sealedFor, stagesTo, startRun, STAGES, writeReceipt } from "../gate-stages.mjs";
import { spawnSync as spawnInherit } from "../background-process.mjs";
import { readDiskPolicy, scratchRoots } from "../disk-policy.mjs";

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
const USAGE = `usage: rightkit-gate --to check|compile|package|qualify|release [--platform mac|win|ios|android] [--tier patch|update] [--receipt-in <file>] [--receipt-out <file>] | rightkit-gate --to check|compile|package|qualify|release --unit npm:@rightkit/<name>|cargo:rightkit-<name> [--receipt-out <file>] | rightkit-gate --to dev|test [-- <right-release args>] | rightkit-gate stage --platform mac|win [--tier patch|update] | rightkit-gate [--base <ref>] [--plan] [--no-cache] [--pre-push] | rightkit-gate install-hook [--force] | rightkit-gate env-digest | rightkit-gate snapshot [--commit <rev>] [--verify] [--json] | rightkit-gate live-scans --repo <root> | rightkit-gate env-digest [--snapshot <dir> --commit <sha>] | rightkit-gate check [--repo <dir>] [--commit <rev> | --worktree] [--rule-tier compile|release] [--only <rule,...>] [--list] [--print-baseline] [--baseline <file>] | rightkit-gate check --commit <sha> --full-a --snapshot <dir> | --version`;

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
export function classAEnvironmentDigest({ root, files, spawn = defaultSpawn, includeCaches = true }) {
  // Only a file that does not exist is "absent"; any other read failure throws: an input whose bytes cannot be read is never
  // identified by a placeholder that would match the same failure later.
  // `optional`: only an install-state file may legitimately not exist; a gate file found by the walk must be readable.
  const fileDigest = (f, { optional = false } = {}) => { try { return createHash("sha256").update(fs.readFileSync(f)).digest("hex"); } catch (error) { if (optional && (error?.code === "ENOENT" || error?.code === "ENOTDIR")) return "absent"; throw new Error(`cannot read ${f} for the class A environment (${error?.code ?? error?.message})`); } };
  const gatePkg = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
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
 *
 * `packages` (default none) is the multi-root form: the repository-relative directories of the packages that declare @rightkit/release when the
 * repository root does not declare it (CutRight's apps/cutright, HeardRight's tauri-app-next). Each one's node_modules/.bin/rightkit-gate is tried
 * right after the root's (the glob above stops three levels down), and the pnpm fallback runs in the first of them (`pnpm --dir <package> exec`;
 * pnpm at the root of such a repository has no rightkit-gate to run). With none, the text is exactly what 0.6.4 and 0.6.5 wrote
 * (fixtures/published-gate-hooks.json holds its digest, and a repository whose root installs the package must keep getting it, byte for byte,
 * or the committed hook of every app copied from the template stops being "already current"). `nodePath` and every package are written into
 * a shell script: installHook passes only plain relative paths (hookGatePlan).
 */
export function hookText(nodePath, packages = [], legacy = false) {
  const bins = packages.map((dir) => ` ${dir}/node_modules/.bin/rightkit-gate`).join("");
  const pnpmExec = packages.length ? `pnpm --dir ${packages[0]} exec rightkit-gate` : "pnpm exec rightkit-gate";
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
# The gate binary: the root install, ${packages.length ? "the install of each package that declares @rightkit/release, any other workspace package's install, then pnpm exec in the first of them" : "any workspace package's install, then pnpm exec"}.
found=""
for bin in node_modules/.bin/rightkit-gate${bins} */node_modules/.bin/rightkit-gate */*/node_modules/.bin/rightkit-gate */*/*/node_modules/.bin/rightkit-gate; do
  if [ -x "$bin" ]; then found="$bin"; break; fi
done
${legacy ? "" : `if command -v rightkit-host-gate >/dev/null 2>&1; then
  echo "rightkit-gate: using host trusted gate" >&2
  rightkit_gate() { rightkit-host-gate "$@"; }
elif [ -f "$HOME/Library/Application Support/RightKit/build-control/config.json" ] || [ -f /d/.rightkit-managed/config.json ]; then
  echo "rightkit-gate: managed host is missing rightkit-host-gate" >&2
  exit 1
el`}if [ -n "$found" ]; then
  echo "rightkit-gate: using $found" >&2
  rightkit_gate() { "$found" "$@"; }
elif command -v pnpm >/dev/null 2>&1; then
  echo "rightkit-gate: using ${pnpmExec}" >&2
  rightkit_gate() { ${pnpmExec} "$@"; }
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
 * The tracked package.json files (the root one when git cannot list) that matter to the hook: `provider` is the first that declares
 * @rightkit/release or is that package itself, `declaring` the directories of every one that declares it ("." for the root; the package itself is
 * not one: it installs nothing of itself).
 */
function releasePackages(o, root, git) {
  let files = ["package.json"];
  const listed = git("ls-files", "-z", "--", ":(glob)**/package.json");
  if (!failed(listed)) files = text(listed).split("\0").filter(Boolean).map(posix);
  let provider = null; const declaring = [];
  for (const file of files) {
    const abs = path.join(root, file);
    if (!o.exists(abs)) continue;
    let manifest;
    try { manifest = JSON.parse(o.readFile(abs).replace(/^\uFEFF/, "")); } catch { continue; }
    if (manifest?.name === "@rightkit/release") { provider ??= file; continue; }
    if (["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"].some((section) => manifest?.[section] && Object.hasOwn(manifest[section], "@rightkit/release"))) {
      provider ??= file;
      declaring.push(path.posix.dirname(file));
    }
  }
  return { provider, declaring: [...new Set(declaring)].sort() };
}

// Where a package installs the gate, below its own directory. pnpm links it there for a package that declares @rightkit/release, so the path is the
// same for every version (node_modules/.pnpm/@rightkit+release@x.y.z/... is the store copy it points at, and goes stale with each bump).
const GATE_IN_PACKAGE = "node_modules/@rightkit/release/bin/rightkit-gate.mjs";
// What may be written into the committed shell script: a plain relative path of letters, digits and _ - . @ + /, no `..`, no `.` segment, no
// leading dash (it would read as an option of `pnpm --dir`).
const HOOK_PATH_SEGMENT = /^(?!\.{1,2}$)[A-Za-z0-9_@.+-]+$/;
export const hookPathIsPlain = (value) => typeof value === "string" && value !== "" && !value.startsWith("-") && value.split("/").every((segment) => HOOK_PATH_SEGMENT.test(segment));

/**
 * What the pre-push hook is told about where the gate lives: { nodePath, packages, unsafe }. `nodePath` is the last resort the hook runs with
 * node, `packages` the extra directories hookText searches (multi-root form), `unsafe` the declaring packages whose directory cannot be written
 * into a shell script. `declaring` is releasePackages(...).declaring; `gateFile` is where this gate runs from.
 *
 * A function of what the repository's package.json files declare, never of what is installed: the same tree gives the same hook on a fresh
 * clone, an exports checkout, after `pnpm install` and after a version bump, so `install-hook` (a `prepare` script) never meets a hook it
 * wrote earlier as one that "already exists and differs". It takes no `exists`, so that cannot creep back in. Always a stable,
 * version-independent path relative to the repository, never one outside it, absolute, or inside the pnpm store:
 *   1. The root declares @rightkit/release (whatever else does): its link and no extra packages, the hook 0.6.4 and 0.6.5 wrote (Pulse declares
 *      it at the root and in hub/; RightKit itself at the root).
 *   2. Only packages declare it (CutRight's apps/cutright, HeardRight's tauri-app-next): the link of the first of them in sorted order, whichever is
 *      installed, and every one of them is searched.
 *   3. No package.json declares it (a checkout of the package itself that nothing depends on), or only under a directory the hook cannot name: if
 *      this gate runs from inside the repository and not from a node_modules, its path from the root, else the root's link, never a path
 *      outside the repository. This is the one case that reads where the gate runs from, because the repository says nothing.
 */
export function hookGatePlan({ root, gateFile, declaring }) {
  const plain = declaring.filter((d) => d !== "." && hookPathIsPlain(d));
  const unsafe = declaring.filter((d) => d !== "." && !hookPathIsPlain(d));
  if (declaring.includes(".")) return { nodePath: GATE_IN_PACKAGE, packages: [], unsafe };
  if (plain.length) return { nodePath: `${plain[0]}/${GATE_IN_PACKAGE}`, packages: plain, unsafe };
  const relative = posix(path.relative(root, gateFile));
  const inside = relative !== "" && !relative.startsWith("..") && !path.isAbsolute(relative) && !relative.split("/").includes("node_modules") && hookPathIsPlain(relative);
  return { nodePath: inside ? relative : GATE_IN_PACKAGE, packages: [], unsafe };
}

// The advisory ownership pre-commit step (`right-release hooks install`, ownership-hooks.mjs) runs the pinned @rightkit/release, so
// it is written only where that is installed. Its own hook and the repository's .git/hooks/pre-commit both keep running.
const PINNED_CLI = "node_modules/@rightkit/release/cli/right-release.mjs";
const FOREIGN_PRE_COMMIT = "pre-commit.rightkit-foreign";

async function installHook(o, root, force) {
  const git = (...a) => o.spawn("git", a, { cwd: root });
  const hooksDir = path.join(root, ".githooks");
  const hookPath = path.join(hooksDir, "pre-push");
  const { provider, declaring } = releasePackages(o, root, git);
  const { nodePath, packages, unsafe } = hookGatePlan({ root, gateFile: o.gateFile, declaring });
  if (unsafe.length) o.err(`rightkit-gate: WARNING: the pre-push hook does not look for the gate in ${unsafe.join(", ")}: not a plain directory name (letters, digits and _ - . @ + only, with / between); it still finds a root or workspace install by glob.`);
  const wanted = hookText(nodePath, packages);
  const hasHook = o.exists(hookPath);
  const current = hasHook ? o.readFile(hookPath).replace(/\r\n/g, "\n") : null;
  if (hasHook && current !== wanted && current !== hookText(nodePath, packages, true) && !force) {
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
  o.out(`rightkit-gate: commit the hooks with executable mode (git add --chmod=+x ${files.join(" ")}); the pre-push hook finds rightkit-gate in node_modules/.bin of the repository root, ${packages.length ? `else of ${packages.join(", ")} (the packages that declare @rightkit/release), else of any workspace package, else through \`pnpm --dir ${packages[0]} exec\`` : "else of any workspace package, else through \`pnpm exec\`"}.`);
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
  if (to === "dev" || to === "test") return await runWorktreeCompile(o, args, root, git, io, to, appRoot);
  let stages = stagesTo(to);
  if (!stages) { o.err(`${tag} --to must be one of ${STAGES.join(", ")}`); return 2; }
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
  const commit = text(git("rev-parse", "HEAD")).trim();
  const tree = text(git("rev-parse", "HEAD^{tree}")).trim();
  const common = text(git("rev-parse", "--git-common-dir")).trim();
  if (!/^[0-9a-f]{40}$/.test(commit) || !tree || !common) { o.err(`${tag} cannot read HEAD and the git directory`); return 2; }
  const commonDir = path.resolve(root, common);
  const dirty = git("status", "--porcelain", "--untracked-files=no");
  if (failed(dirty) || text(dirty).trim()) { o.err(`${tag} BLOCKED: the checkout differs from HEAD; every stage works on the commit, so commit or stash first`); return 1; }

  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
  if (executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
  const version = (cmd, a) => { const r = o.spawn(cmd, a, { cwd: root }); return failed(r) ? null : text(r).trim().split(/\r?\n/)[0]; };
  const listed = git("ls-tree", "-r", "--name-only", "-z", "HEAD");
  const lockFiles = {};
  for (const f of failed(listed) ? [] : text(listed).split("\0").filter((x) => /(^|\/)(Cargo\.lock|pnpm-lock\.yaml)$/.test(x))) {
    const shown = git("show", `HEAD:${f}`); if (!failed(shown)) lockFiles[f] = text(shown);
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
  const rightRelease = (stage, cliArgs) => {
    const run = startRun({ commonDir, commit, tree, stage });
    try {
      o.out(`${tag} [${stage}] right-release ${cliArgs.join(" ")}`);
      const r = (o.spawnInherit ?? ((cmd, a, opts) => spawnInherit(cmd, a, { ...opts, stdio: "inherit" })))(process.execPath, [cli, ...cliArgs], { cwd: appRoot, env: { ...o.env, ...run.env } });
      return r.status === 0;
    } finally { endRun(run); }
  };

  o.out(`${tag} --to ${to} for ${commit.slice(0, 12)}${appDir === "." ? "" : ` (app ${appDir})`}: ${stages.join(" -> ")}; executor ${executor.kind}`);
  // A later stage on the sealed bytes: check, compile and package already passed for them (in this receipt) and the bytes on disk are
  // the ones they sealed, so they are not run again.
  const sealedNow = receipt.stages.package?.ok ? (o.sealedFor ?? sealedFor)({ root: appRoot, commit, platform }) : null;
  const sealedIntact = Boolean(sealedNow && sealedNow.id === receipt.stages.package.sealed && sealedNow.digest === receipt.stages.package.manifestSha256);
  // Every deterministic rule and scan must run before compilation, for every door stage.
  const earlyTier = "release";
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
    let ok = false; const at = new Date().toISOString(); let extra = {};
    if (stage === "check") {
      ok = (await (o.runCheck ?? (() => main(["check", "--commit", "HEAD", "--full-a", "--rule-tier", earlyTier], { ...io, cwd: root })))(earlyTier)) === 0;
      extra = { ruleTier: earlyTier };
    }
    else if (stage === "compile") {
      // A Tauri crate with externalBin needs its sidecars in src-tauri/binaries before it compiles (its build script refuses
      // otherwise), so every release config that declares `sidecars` is staged first, through right-release under this door run.
      const listedConfigs = git("ls-tree", "-r", "--name-only", "-z", "HEAD");
      const sidecarConfigs = (failed(listedConfigs) ? [] : text(listedConfigs).split("\0").filter((f) => /(^|\/)right-release\.config\.(json|mjs)$/.test(f)).filter((f) => appDir === "." || f.startsWith(`${appDir}/`)))
        .filter((f) => { try { return /\bsidecars\b/.test(o.readFile(path.join(root, f))); } catch { return false; } });
      ok = true;
      // right-release runs in appRoot (nested apps), so the repository-relative config is handed over absolute (Astra, second pass).
      for (const config of sidecarConfigs) if (!rightRelease(stage, ["stage-sidecar", "--config", path.join(root, config)])) { ok = false; break; }
      if (ok) ok = (await (o.runCompile ?? (() => main(["--all"], { ...io, cwd: root, readStdin: () => "" })))()) === 0;
    }
    else if (stage === "package") {
      // Source QA (right-release qa --stage source) exists for mac and win only; a mobile package is the signed build alone.
      ok = (mobile || rightRelease(stage, ["qa", "--stage", "source", "--platform", platform])) && rightRelease(stage, ["build", "--platform", platform]);
      const sealed = ok ? (o.sealedFor ?? sealedFor)({ root: appRoot, commit, platform }) : null;
      if (ok && !sealed) { o.err(`${tag} [package] the build reported success but sealed nothing for ${commit.slice(0, 8)} on ${platform}`); ok = false; }
      if (sealed) extra = { platform, sealed: sealed.id, manifestSha256: sealed.digest };
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
          : await (o.runReleaseCheck ?? (() => main(["check", "--commit", "HEAD", "--full-a", "--rule-tier", "release"], { ...io, cwd: root })))();
        if (releaseTier !== 0) { o.err(`${tag} [release] the release tier (every rule and scan) is red for ${commit.slice(0, 12)}; nothing was uploaded`); ok = false; }
        else ok = rightRelease(stage, ["upload", "--platform", platform, "--release", sealed.sealed, "--apply"]);
        extra = { sealed: sealed.sealed, manifestSha256: sealed.manifestSha256, track: platform === "ios" ? "testflight" : "play-internal", ruleTier: "release" };
      } else if (stage === "qualify") {
        // Qualification owes the release tier: every rule and scan, the ones held out of compile admission included (app-check.mjs TIERS).
        const releaseTier = receipt.stages.check?.ok && receipt.stages.check.ruleTier === "release"
          ? (o.out(`${tag} [qualify] the release tier already passed at check for this exact input; not run again`), 0)
          : await (o.runReleaseCheck ?? (() => main(["check", "--commit", "HEAD", "--full-a", "--rule-tier", "release"], { ...io, cwd: root })))();
        if (releaseTier !== 0) { o.err(`${tag} [qualify] the release tier (every rule and scan) is red for ${commit.slice(0, 12)}; nothing was qualified`); ok = false; }
        else ok = rightRelease(stage, ["qa", "--stage", "installed", "--platform", platform, "--release", sealed.sealed]);
        extra = { sealed: sealed.sealed, manifestSha256: sealed.manifestSha256, ruleTier: "release" };
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

/**
 * rightkit-gate --to dev|test [-- <right-release dev|test arguments>]: the door for the local compiles that work on the uncommitted tree.
 * Class A runs over the working tree first (nothing compiles while it is red), then `right-release dev|test` runs under a live door
 * run, the only way it runs at all. The executor is the same as every other stage: the broker where there is one (its own admission
 * checks again), plain cargo on an unmanaged host, never around a managed host's broker.
 */
async function runWorktreeCompile(o, args, root, git, io, to, appRoot = root) {
  const tag = "rightkit-gate:";
  const separator = args.indexOf("--");
  const passthrough = separator >= 0 ? args.slice(separator + 1) : [];
  // `--to dev --platform mac` (the documented form) hands the platform to right-release as `--platform mac`; a platform given after
  // `--` wins (Pulse, 2026-10-27: the flag before `--` was dropped and right-release refused "requires --platform").
  const gateArgs = separator >= 0 ? args.slice(0, separator) : args;
  const platformAt = gateArgs.indexOf("--platform");
  if (platformAt >= 0 && gateArgs[platformAt + 1] && !passthrough.includes("--platform")) passthrough.unshift("--platform", gateArgs[platformAt + 1]);
  const commit = text(git("rev-parse", "HEAD")).trim();
  const tree = text(git("rev-parse", "HEAD^{tree}")).trim();
  const common = text(git("rev-parse", "--git-common-dir")).trim();
  if (!/^[0-9a-f]{40}$/.test(commit) || !common) { o.err(`${tag} cannot read HEAD and the git directory`); return 2; }
  const which = (cmd) => { const r = o.spawn(o.platform === "win32" ? "where" : "which", [cmd]); return failed(r) ? [] : text(r).split(/\r?\n/).map((l) => l.trim()).filter(Boolean); };
  const executor = (o.resolveExecutor ?? resolveExecutor)({ env: o.env, platform: o.platform, exists: o.exists, which, readFile: o.readFile }); applyExecutorEnv(o, executor);
  if (executor.kind === "refuse") { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
  o.out(`${tag} --to ${to} on the working tree of ${commit.slice(0, 12)}: check (class A) -> right-release ${to}; executor ${executor.kind}`);
  // Full class A on the working tree, the checks that execute repository code included, before right-release compiles anything.
  const checked = await (o.runCheck ?? (() => main(["check", "--worktree", "--full-a"], { ...io, cwd: root })))();
  if (checked !== 0) { o.err(`${tag} --to ${to} stopped at check: nothing compiled`); return 1; }
  const commonDir = path.resolve(root, common);
  const run = startRun({ commonDir, commit, tree, stage: to });
  try {
    const cli = fileURLToPath(new URL("../cli/right-release.mjs", import.meta.url));
    // right-release reads its config from its cwd: the resolved app root (appRootOf), not the directory the command was typed in
    // (apps/foo/src is apps/foo; Astra, third pass).
    // The door's own RightKit runs every sub-step: a dev target's `right-release web-build|stage-sidecar` steps resolve `right-release`
    // on PATH, which found the app's node_modules copy (HeardRight on the Dell, 2026-10-27: the app's 0.6.8 guard refused what the
    // door's 0.6.9 admitted). An installed gate (node_modules/@rightkit/release/bin) has its package's bin shims two levels up.
    const r = (o.spawnInherit ?? ((cmd, a, opts) => spawnInherit(cmd, a, { ...opts, stdio: "inherit" })))(process.execPath, [cli, to, ...passthrough], { cwd: appRoot, env: { ...o.env, ...run.env, ...doorPath(o) } });
    return r.status === 0 ? 0 : 1;
  } finally { endRun(run); }
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
    // Where this gate runs from (install-hook writes a path to it only when it is a plain path inside the repository: hookGatePlan).
    gateFile: fileURLToPath(import.meta.url), ...io,
  };
  const args = [...argv];
  const option = (name) => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
  if (args.includes("--version") || args.includes("-v")) {
    o.out(JSON.parse(fs.readFileSync(new URL("../package.json", import.meta.url), "utf8")).version);
    return 0;
  }
  if (args.includes("--help") || args.includes("-h")) { o.out(USAGE); return 0; }
  // `snapshot`: sync the selected commit into the gate-owned source directory and verify it is exactly the commit's tree
  // (../snapshot.mjs; docs/GATE-CONTRACT.md §1). Reads the checkout's commits only, never its working files.
  if (args[0] === "snapshot") return (await import("../snapshot.mjs")).snapshotCommand(o, args.slice(1));
  // `live-scans`: re-run only the live remote scans (pnpm-audit) of a checked tree, answering the broker's verdict-reuse refresh
  // (../snapshot-check.mjs; GATE-CONTRACT §1, E18; rightkit#1257).
  if (args[0] === "live-scans") return (await import("../snapshot-check.mjs")).liveScansCommand(o, args.slice(1), { discoverPnpmRoots, discoverNpmUnits });
  const sub = args[0] && !args[0].startsWith("-") ? args[0] : null;
  if (sub && sub !== "install-hook" && sub !== "check" && sub !== "stage" && sub !== "env-digest") { o.err(`rightkit-gate: unknown command ${JSON.stringify(sub)}\n${USAGE}`); return 2; }
  // `check` is the deterministic stage (Adrian, 2026-10-25: deterministic always first, one door): the app-check rules, then every
  // class A scan (lockfiles, cargo-deny, cargo-audit, secrets, licences, manifests, workflow lint, syntax...) over the whole tree of the
  // commit, never a diff. Nothing compiles. The RustRight broker runs exactly this before it admits any compile. `--list`,
  // `--print-baseline` and `--only` stay app-check-only views.
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
    // Full admission cannot be downgraded by a caller's tier flag. Partial views retain their requested tier.
    const fullAdmission = args.includes("--full-a") && !args.includes("--list") && !args.includes("--print-baseline") && !args.includes("--only");
    const stripSnapshot = (list) => { const i = list.indexOf("--snapshot"); return i < 0 ? list : [...list.slice(0, i), ...list.slice(i + 2)]; };
    const appArgs = stripSnapshot(args.slice(1).filter((a) => a !== "--full-a"));
    if (fullAdmission) {
      const tierIndex = appArgs.indexOf("--rule-tier");
      if (tierIndex >= 0) appArgs.splice(tierIndex, 2);
      appArgs.push("--rule-tier", "release");
    }
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
    // Admission uses the full tier; a partial diagnostic check may choose either tier.
    const ruleTier = fullAdmission ? "release" : requestedTier;
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
  if (sub === "install-hook") return await installHook(o, root, args.includes("--force"));
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
        o.out(`env=${classAEnvironmentDigest({ root: real, files: sc.commitFiles(checkout, commit, o.spawn).files, spawn: o.spawn })}`);
        return 0;
      } catch (e) { o.err(`rightkit-gate: ${e.code === sc.CHECK_ERROR ? `${sc.CHECK_ERROR}: ` : "env-digest: "}${e.message}`); return 2; }
    }
    const listed = git("ls-files", "-z", "--cached", "--others", "--exclude-standard");
    if (failed(listed)) { o.err(`rightkit-gate: env-digest cannot list the working tree: ${why(listed)}`); return 2; }
    try { const files = text(listed).split("\0").filter(Boolean).map(posix); o.out(`env=${classAEnvironmentDigest({ root, files, spawn: o.spawn })}`); }
    catch (error) { o.err(`rightkit-gate: env-digest: ${error.message}`); return 2; }
    return 0;
  }
  if (args.includes("--plan") && option("--to") !== undefined) { o.err("rightkit-gate: --plan plans the class A scans and runs nothing; it takes no --to stage (Astra, second pass)"); return 2; }
  // `--unit` is a gate flag only before a `--` separator: after it the arguments belong to right-release (an app's `--to dev -- …`).
  const separator = args.indexOf("--");
  const unitFlag = args.includes("--unit") && (separator < 0 || args.indexOf("--unit") < separator);
  if (option("--to") !== undefined && unitFlag) {
    const selector = option("--unit");
    const validArgs = args.length % 2 === 0 && args.every((a, i) => i % 2 ? !a.startsWith("--") : ["--to", "--unit", "--receipt-out"].includes(a)) && ["--to", "--unit", "--receipt-out"].every((flag) => args.filter((a) => a === flag).length <= 1);
    if (!validArgs || !selector) {
      o.err("rightkit-gate: unit stages take only --to, --unit and optional --receipt-out"); return 2;
    }
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
  // The snapshot: the commit or tree being checked (check --commit), HEAD for the gate, or the working tree (check --worktree, --plan).
  const allFilesMode = classAOnly || args.includes("--all");
  const snapshot = planOnly ? null : classAOnly ? classACommit : "HEAD";
  // Every effectful gate entry owes the app rules, not only `check` (Astra, 2026-10-26): a bare, --all or pre-push run on HEAD runs them
  // first, in the tier asked for (compile by default), and plans nothing while they are red. --plan and --class-a (which runs them in
  // `check`) are untouched.
  if (snapshot && !classAOnly) {
    const appRules = await (o.appCheck ?? (await import("../app-check.mjs")).main)(["--repo", root, "--commit", snapshot, "--rule-tier", option("--rule-tier") ?? "compile"], { out: o.out, err: o.err, label: "check", cwd: o.cwd });
    if (appRules !== 0) { const id = git("rev-parse", String(snapshot)); o.err(`${tag} BLOCKED: the app rules are red for ${(failed(id) ? String(snapshot) : text(id).trim()).slice(0, 12)}; no scan was planned or run`); return appRules; }
  }

  const ac = o.abort ?? new AbortController();
  let exportRoot = null;
  let snapshotGitConfig = null;
  const cleanup = () => {
    if (exportRoot) fs.rmSync(exportRoot, { recursive: true, force: true });
    if (snapshotGitConfig) { fs.rmSync(snapshotGitConfig, { force: true }); try { fs.rmdirSync(path.dirname(snapshotGitConfig)); } catch { /* not empty: leave it, the next exact sync prunes it */ } }
  };
  const signals = [];
  try {
    let repo = root;
    if (snapshot && snapshotA) {
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

    // Class B scans (compile, lint, tests) run in the live checkout, so it must equal HEAD where they read it: an uncommitted fix could
    // otherwise hide a committed failure. Class A reads only the snapshot.
    if (!planOnly && !classAOnly) {
      const npmRootFiles = ["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc", ...ESLINT_CONFIG_FILES, ...BIOME_CONFIG_FILES];
      const inputs = [...new Set([...rust.units.map((u) => u.dir), ...npm.map((u) => u.dir), ...rust.workspaces.map((w) => w.cargoDir), "ci", "scripts", "vendor", "Cargo.toml", "Cargo.lock", ".cargo", "rust-toolchain.toml", "rust-toolchain",
        "deny.toml", "clippy.toml", "rustfmt.toml", ...npmRootFiles, ...npmRoots.filter((r) => r !== ".").flatMap((r) => npmRootFiles.map((f) => `${r}/${f}`))])];
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
    let fullPlan;
    fullPlan = planScans({
      // Class A of a commit or tree reads only that snapshot: every scan, cargo's included, runs in the export, never in the live
      // checkout (which may hold other bytes). Compiles (class B) use the live checkout, whose tree the dirty check pinned to HEAD.
      changed, contentRoot: repo, liveRoot: classAOnly && snapshot ? repo : root, trustedRoot: snapshotA ?? root, includeTests: true, rustScope, cargoCommand,
      tier: fullA ? "release" : (option("--rule-tier") ?? "compile"), cargoLiveRoot,
      // The quiet tests (port, pipe, desktop and wall-clock bound) are proven by the CI shards, one crate per runner, unless
      // RK_GATE_QUIET_TESTS=local asks for the serial quiet lane here (no CI, or a CI outage).
      // Full class A runs every test before a compile: the quiet suites run here in the quiet lane, never left to CI.
      quietTests: fullA || o.env.RK_GATE_QUIET_TESTS === "local" ? "local" : "ci", packageFeatures, liveExecution: fullA && (args.includes("--worktree") || Boolean(snapshotA)),
      rustUnits: rust.units, npmUnits: npm, npmRoots, cargoDir: ".", rustLocks, crateFeatures,
      // The broker (or, on a direct host, the gate itself) admits compiles; no separate grant file decides it here.
      localCompile: true,
      exists: o.exists, readFile: o.readFile, platform: o.platform,
    });
    // A broker-managed host whose broker is unreachable runs no class B scan: npm tests and scripts can compile native code the
    // snapshot lists no manifest for (Astra, second pass); the class A scans never compile and still run.
    // Class A tests run code too (Astra, second pass: a test script can compile native code its text does not show), so a host whose
    // broker is unreachable runs neither; the gate stops rather than skip them.
    if (executor.kind === "refuse" && fullPlan.scans.some((x) => x.class === "B" || (x.kind === "test" && (!classAOnly || fullA)))) { o.err(`${tag} BLOCKED: ${executor.problem}`); return 1; }
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
    o.out(`${tag} ${plan.scans.length} scan(s) for ${changed.length} changed file(s) vs ${base.slice(0, 12)}; class A scans ${jobs} at a time, class B scans 1 at a time${quietCount ? `, ${quietCount} quiet test scan(s) last under the broker's quiet lease` : ""}`);
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
      try { envAtStart = classAEnvironmentDigest({ root: liveBase, files: live.files, spawn: o.spawn, includeCaches: false }); envStartWithCaches = classAEnvironmentDigest({ root: liveBase, files: live.files, spawn: o.spawn }); cachesAtStart = toolCacheManifest({ root: liveBase, files: live.files }); }
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
        const environment = classAEnvironmentDigest({ root: liveBase, files, spawn: o.spawn });
        cache = { ...store, digest: `${buildCacheDigest({ objectId, headTree: digestOf(files), rustUnits: rust.units, npmUnits: npm, cargoDir: ".", rustLocks, crateFeatures, listFiles, readBlob })}:${environment}` };
      } catch { cache = null; }
    }
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
      // `check` (and the broker's compile admission, which runs it on a captured copy of the tree without node_modules) judges the
      // content: it never executes the repository's own code. The class A checks that do (tests, typecheck, lint, generated docs:
      // `executes`) run in the full gate (--all, the push hook, the door's compile stage), after the content checks and before any compile.
      ...(classAOnly && !fullA ? { exclude: (s) => s.executes === true } : {}),
      // A snapshot check names its commit; it never reads the checkout's HEAD.
      quietLane: quietLeaseLane(o, { tag, reason: `rightkit-gate-quiet-tests@${snapshotCommitId ? snapshotCommitId.commit.slice(0, 12) : text(git("rev-parse", "--short", "HEAD")).trim()}` }),
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
      try { envDigest = classAEnvironmentDigest({ root: liveBase, files: live.files, spawn: o.spawn }); envNoCaches = classAEnvironmentDigest({ root: liveBase, files: live.files, spawn: o.spawn, includeCaches: false }); }
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

const invokedDirectly = (() => {
  try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; }
})();
/** This gate's own resolved entry file: the broker admits a compile only when the request names the pinned trusted gate by this
 * path (rustright reads it from RIGHTKIT_CALLING_GATE; a missing or different path is refused). */
export const callingGatePath = () => fs.realpathSync(fileURLToPath(import.meta.url));
if (invokedDirectly) {
  // Every child this run starts that can reach rustright (scans, unit stages, right-release dev/test, their builds) inherits it.
  process.env.RIGHTKIT_CALLING_GATE = callingGatePath();
  main(process.argv.slice(2)).then((code) => process.exit(code), (e) => { console.error(`rightkit-gate: ${e?.stack ?? e}`); process.exit(2); });
}
