#!/usr/bin/env node
// rightkit-gate: the mandatory local deterministic gate for any repository that uses RightKit (Adrian 2026-10-22:
// "local deterministic checks ... mandatory for all builds"). Runs from the pre-push hook that `rightkit-gate install-hook`
// writes, so it never depends on an agent remembering. Every applicable scan for the pushed change runs through the one
// scan module (../deterministic-scans.mjs); Rust units come from `cargo metadata`, npm units from pnpm-workspace.yaml,
// so the layout is the repository's own. CI still compiles, lints and tests everything.
//
// Content checks run on an export of HEAD (exactly what is pushed), never on uncommitted work in the checkout.
//
//   rightkit-gate [--base <ref>] [--plan] [--no-cache]
//                                             run the gate (default base origin/main); --plan prints the scans and runs none;
//                                             --no-cache (or RK_GATE_CACHE=0) reruns every scan
//   rightkit-gate install-hook [--force]      write .githooks/pre-push and set core.hooksPath
//
// From the pre-push hook, git's ref lines arrive on stdin: the commit being pushed must be the checked-out HEAD (broker scans
// read the live checkout), and the remote's current commit is the diff base. To push an earlier commit of the local stack,
// check it out first (a detached HEAD is fine), push HEAD:<branch>, return.
// ci/gate.config.json `rust` (optional) sets per-crate feature sets, exactly as CI's gate reads it.
// Result cache: a passing class B scan is reused only when its scan id, command, tool version, environment and input digest
// (the gated commit's git object ids for the crate's workspace closure, the shared build inputs and ci/gate.config.json; the
// whole HEAD tree for npm, node and docs scans) are all identical. Stored in <git common dir>/rightkit-gate-cache; a hit prints
// `cached` and is listed in the result's cacheHits. Failures, deferrals and class A scans are never cached.
// RK_GATE_JOBS (default 4) sets how many compile scans run side by side; tests stay one at a time.
// SIGINT/SIGTERM stop scheduling, kill every running scan's process tree and exit non-zero listing the scans not run.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { WORKSPACE_RUST_FILE, canonicalJson, defaultSpawn, fileScanCache, metadataArgs, planScans, runScans, scanDeferrals, scanFailures } from "../deterministic-scans.mjs";
import { changedLockNames, parseLock, reach } from "../lock-graph.mjs";

const ZERO = /^0+$/;
const GRANT_FILE = ".rightkit-local-build.json";
const SAFE_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const posix = (p) => String(p).replaceAll("\\", "/");
const text = (r) => String(r?.stdout ?? "");
const failed = (r) => !r || Boolean(r.error) || r.status !== 0;
const why = (r) => String(r?.error?.message ?? r?.stderr ?? "").trim().split(/\r?\n/).slice(-3).join(" | ");

const USAGE = `usage: rightkit-gate [--base <ref>] [--plan] [--no-cache] | rightkit-gate install-hook [--force] | --version`;

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

/** npm units: pnpm-workspace.yaml globs over tracked package.json files, else the root package.json. */
export function discoverNpmUnits({ root, tracked, readFile, exists }) {
  let dirs;
  const workspaceFile = path.join(root, "pnpm-workspace.yaml");
  if (exists(workspaceFile)) {
    const globs = parseWorkspaceGlobs(readFile(workspaceFile));
    const inc = globs.filter((g) => !g.startsWith("!")).map(globToRegExp);
    const exc = globs.filter((g) => g.startsWith("!")).map((g) => globToRegExp(g.slice(1)));
    dirs = tracked("package.json").map((p) => path.posix.dirname(p)).filter((d) => d !== "." && inc.some((r) => r.test(d)) && !exc.some((r) => r.test(d)));
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

/**
 * Rust units from `cargo metadata --no-deps --format-version 1 --locked`, run through the resolved cargo command.
 * One Cargo workspace per repository: several are refused, never partly covered. Returns
 * { present, units: [{ name, dir, publish }], cargoDir, uncovered }.
 */
export function discoverRustUnits({ root, spawn, cargoCommand, tracked, readFile, realpath = fs.realpathSync }) {
  const manifests = tracked("Cargo.toml");
  if (!manifests.length) return { present: false, units: [], cargoDir: ".", uncovered: [] };
  const dirOf = (m) => path.posix.dirname(m);
  const roots = [];
  for (const m of [...manifests].sort((a, b) => dirOf(a).split("/").length - dirOf(b).split("/").length || (a < b ? -1 : 1))) {
    const d = dirOf(m);
    if (!roots.some((r) => r === "." || d === r || d.startsWith(`${r}/`))) roots.push(d);
  }
  if (roots.length > 1) throw new Error(`more than one Cargo workspace (${roots.join(", ")}); rightkit-gate covers one per repository, so none is partly covered`);
  const r = spawn(cargoCommand[0], [...cargoCommand.slice(1), ...metadataArgs()], { cwd: path.join(root, roots[0]) });
  if (failed(r)) throw new Error(`cargo metadata failed: ${why(r) || `exit ${r?.status}`}`);
  let meta;
  try { meta = JSON.parse(text(r)); } catch (e) { throw new Error(`cargo metadata output is not JSON: ${e.message}`); }
  const real = (p) => { try { return realpath(p); } catch { return path.resolve(p); } };
  const rootReal = real(root);
  const rel = (abs) => posix(path.relative(rootReal, real(abs))) || ".";
  const cargoDir = rel(meta.workspace_root);
  if (cargoDir === ".." || cargoDir.startsWith("../")) throw new Error(`the Cargo workspace root ${meta.workspace_root} is outside the repository`);
  const units = (meta.packages ?? []).map((p) => {
    const dir = rel(path.dirname(p.manifest_path));
    if (dir === ".." || dir.startsWith("../")) throw new Error(`workspace member ${p.name} lives outside the repository (${dir})`);
    return { name: p.name, dir, publish: !(Array.isArray(p.publish) && p.publish.length === 0) };
  }).sort((a, b) => (a.name < b.name ? -1 : 1));
  const covered = new Set(units.map((u) => (u.dir === "." ? "Cargo.toml" : `${u.dir}/Cargo.toml`)));
  const uncovered = manifests.filter((m) => !covered.has(m) && /^\s*\[package\]/m.test(readFile(path.join(root, m))));
  return { present: true, units, cargoDir, uncovered };
}

/**
 * The crates a lock-only workspace change reaches (the lock graph CI's selection uses), or null when any other workspace Rust
 * file changed or the base has no lock: every crate stays in scope.
 */
export function lockScope({ changed, cargoDir = ".", unitNames, showBase, readLock }) {
  const rel = (f) => (cargoDir === "." ? f : f.startsWith(`${cargoDir}/`) ? f.slice(cargoDir.length + 1) : null);
  const wide = changed.map((f) => [f, rel(f)]).filter(([, r]) => r !== null && WORKSPACE_RUST_FILE.test(r));
  if (!wide.length || wide.some(([, r]) => r !== "Cargo.lock" && !r.startsWith("vendor/"))) return null;
  const before = showBase(cargoDir === "." ? "Cargo.lock" : `${cargoDir}/Cargo.lock`);
  if (before === null) return null;
  let nextText;
  try { nextText = readLock(); } catch { return null; }
  const next = parseLock(nextText);
  const touched = changedLockNames(parseLock(before), next);
  for (const [, r] of wide) if (r.startsWith("vendor/")) touched.add(r.split("/")[1]);
  const memo = new Map(); const scope = new Set();
  for (const [name] of next) {
    if (!unitNames.has(name)) continue;
    if (touched.has(name) || [...touched].some((t) => reach(next, name, memo).has(t))) scope.add(name);
  }
  return scope;
}

/** The pre-push hook: pnpm exec rightkit-gate, or node with a package-relative path when pnpm is absent. */
export function hookText(nodePath) {
  return `#!/bin/sh
# Mandatory local gate before every push (rightkit-gate install-hook). Never bypass with --no-verify.
cd "$(git rev-parse --show-toplevel)" || exit 1
if command -v pnpm >/dev/null 2>&1; then exec pnpm exec rightkit-gate --base origin/main; fi
exec node "${nodePath}" --base origin/main
`;
}

// ---- result cache inputs ------------------------------------------------------------------------------------

// The build inputs that change how every crate compiles, relative to the Cargo workspace root, plus the gate's own config.
const SHARED_RUST_INPUTS = ["Cargo.lock", "Cargo.toml", "rust-toolchain", "rust-toolchain.toml", ".cargo", "deny.toml", "clippy.toml", "rustfmt.toml", "vendor"];
const GATE_CONFIG = "ci/gate.config.json";

/** The crate a cargo scan id belongs to (clippy:x, check:x, check-features:x:0, check-dependent:x, doc:x, test:rust:x, test:rust-serial:x:1), else null. */
export function rustUnitOfScanId(id) {
  const parts = String(id).split(":");
  if (["clippy", "check", "check-features", "check-dependent", "doc"].includes(parts[0])) return parts[1] ?? null;
  if (parts[0] === "test" && (parts[1] === "rust" || parts[1] === "rust-serial")) return parts[2] ?? null;
  return null;
}

/**
 * The input digest function for the result cache: scan => string, or null when the scan must not be cached.
 * `objectId(path)` is the git blob/tree id of a repo-relative path in the gated commit, or null when the path is absent
 * (it throws when git cannot answer, which makes the scan uncacheable); `headTree` is that commit's tree id.
 * A cargo scan of crate X digests the object ids of X and of every workspace member in X's lock closure, the shared build
 * inputs and ci/gate.config.json. With no lock graph the closure is unknown, so the whole tree is used. Class A is never cached.
 */
export function buildCacheDigest({ objectId, headTree, rustUnits = [], cargoDir = ".", rustLock = null }) {
  const memo = new Map(); const reachMemo = new Map();
  const idOf = (p) => { if (!memo.has(p)) memo.set(p, p === "." ? headTree : objectId(p)); return memo.get(p); };
  const inWs = (f) => (cargoDir === "." ? f : `${cargoDir}/${f}`);
  const byName = new Map(rustUnits.map((u) => [u.name, u]));
  const whole = () => canonicalJson({ kind: "tree", tree: headTree });
  return (scan) => {
    if (scan.class !== "B") return null;
    try {
      const unit = rustUnitOfScanId(scan.id);
      if (!unit) return whole();
      if (!(rustLock instanceof Map) || !byName.has(unit)) return whole();
      const closure = [unit, ...[...reach(rustLock, unit, reachMemo)].filter((n) => byName.has(n))].sort();
      return canonicalJson({
        kind: "rust", unit, cargoDir,
        members: closure.map((n) => [n, byName.get(n).dir, idOf(byName.get(n).dir)]),
        shared: [...SHARED_RUST_INPUTS.map(inWs), GATE_CONFIG].sort().map((p) => [p, idOf(p)]),
      });
    } catch { return null; }
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

function installHook(o, root, force) {
  const git = (...a) => o.spawn("git", a, { cwd: root });
  const hookPath = path.join(root, ".githooks", "pre-push");
  const installed = "node_modules/@rightkit/release/bin/rightkit-gate.mjs";
  const nodePath = o.exists(path.join(root, installed)) ? installed : posix(path.relative(root, fileURLToPath(import.meta.url)));
  const wanted = hookText(nodePath);
  const hasHook = o.exists(hookPath);
  const current = hasHook ? o.readFile(hookPath).replace(/\r\n/g, "\n") : null;
  if (hasHook && current !== wanted && !force) {
    o.err(`rightkit-gate: ${path.join(".githooks", "pre-push")} already exists and differs; nothing changed. Review it, or run \`rightkit-gate install-hook --force\` to replace it.`);
    return 1;
  }
  const currentPath = git("config", "--get", "core.hooksPath");
  const existingPath = failed(currentPath) ? "" : text(currentPath).trim();
  if (existingPath && existingPath !== ".githooks" && !force) {
    o.err(`rightkit-gate: core.hooksPath is already ${JSON.stringify(existingPath)}; nothing changed. Run \`rightkit-gate install-hook --force\` to point it at .githooks.`);
    return 1;
  }
  const done = [];
  if (current === wanted) done.push(".githooks/pre-push already current");
  else { o.mkdir(path.dirname(hookPath), { recursive: true }); o.writeFile(hookPath, wanted); try { o.chmod(hookPath, 0o755); } catch { /* not supported here */ } done.push(`${hasHook ? "replaced" : "wrote"} .githooks/pre-push (${nodePath} without pnpm)`); }
  if (existingPath === ".githooks") done.push("core.hooksPath already .githooks");
  else {
    const set = git("config", "core.hooksPath", ".githooks");
    if (failed(set)) { o.err(`rightkit-gate: git config core.hooksPath failed: ${why(set)}`); return 2; }
    done.push(`set core.hooksPath to .githooks${existingPath ? ` (was ${existingPath})` : ""}`);
  }
  for (const line of done) o.out(`rightkit-gate: ${line}`);
  o.out("rightkit-gate: commit the hook with executable mode (git add --chmod=+x .githooks/pre-push); the hook runs `pnpm exec rightkit-gate`, so @rightkit/release must be a dependency of the repository root.");
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
    spawn: defaultSpawn, exists: fs.existsSync, readFile: (f) => fs.readFileSync(f, "utf8"), writeFile: fs.writeFileSync, mkdir: fs.mkdirSync, chmod: fs.chmodSync,
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
    rust = discoverRustUnits({ root, spawn: (c, a, opts) => o.spawn(c, a, opts), cargoCommand, tracked, readFile: readLive });
    npm = discoverNpmUnits({ root, tracked, readFile: readLive, exists: o.exists });
  } catch (e) { o.err(`${tag} BLOCKED: cannot discover the repository's units: ${e.message}`); return 1; }
  for (const m of rust.uncovered) o.out(`${tag} note: ${m} is a crate outside the Cargo workspace (vendored or fixture?); it is not scanned`);

  // Broker scans (cargo, pnpm) read the live checkout, so it must equal HEAD where they read it: an uncommitted fix could
  // otherwise hide a committed failure. Any change or untracked file under those paths blocks the gate.
  if (!planOnly) {
    const inputs = [...new Set([...rust.units.map((u) => u.dir), ...npm.map((u) => u.dir), rust.cargoDir, "ci", "scripts", "vendor", "Cargo.toml", "Cargo.lock", ".cargo", "rust-toolchain.toml", "rust-toolchain",
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

    let crateFeatures = {};
    try { crateFeatures = JSON.parse(o.readFile(path.join(repo, "ci/gate.config.json"))).rust ?? {}; } catch { /* no config: default feature sets */ }
    const lockPath = rust.cargoDir === "." ? "Cargo.lock" : `${rust.cargoDir}/Cargo.lock`;
    let rustLock = null;
    try { rustLock = parseLock(o.readFile(path.join(repo, lockPath))); } catch { if (rust.units.length) o.out(`${tag} note: no ${lockPath}; reverse dependents are not checked`); }
    const rustScope = rust.units.length ? lockScope({
      changed, cargoDir: rust.cargoDir, unitNames: new Set(rust.units.map((u) => u.name)),
      showBase: (f) => { const r = git("show", `${base}:${f}`); return failed(r) ? null : text(r); },
      readLock: () => o.readFile(path.join(repo, lockPath)),
    }) : null;

    const plan = planScans({
      changed, contentRoot: repo, liveRoot: root, includeTests: true, rustScope, rustLock, cargoCommand,
      rustUnits: rust.units, npmUnits: npm, cargoDir: rust.cargoDir, crateFeatures,
      localCompile: o.exists(path.join(root, GRANT_FILE)),
      exists: o.exists, readFile: o.readFile, platform: o.platform,
    });
    const jobs = Math.max(1, Number(o.env.RK_GATE_JOBS ?? 4) || 4);

    if (planOnly) {
      o.out(`${tag} plan: ${plan.scans.length} scan(s) for ${changed.length} changed file(s) vs ${base.slice(0, 12)} (reads the working tree; nothing runs)`);
      o.out(`${tag} rust units: ${rust.units.map((u) => `${u.name}=${u.dir}`).join(", ") || "none"}${rust.cargoDir !== "." ? ` (workspace ${rust.cargoDir})` : ""}; npm units: ${npm.map((u) => `${u.name}=${u.dir}`).join(", ") || "none"}`);
      for (const s of plan.scans) o.out(`  [${s.class}] ${s.id} :: ${s.command}${plan.deferred.includes(s.id) ? " [deferred: no local compile grant]" : ""}`);
      for (const n of plan.notes) o.out(`${tag} note: ${n}`);
      return 0;
    }

    o.out(`${tag} ${plan.scans.length} scan(s) for ${changed.length} changed file(s) vs ${base.slice(0, 12)}; compile scans ${jobs} at a time`);
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
          cache = { ...store, digest: buildCacheDigest({ objectId, headTree: treeId, rustUnits: rust.units, cargoDir: rust.cargoDir, rustLock }) };
        }
      } catch { cache = null; }
    }
    const scanResult = await runScans(plan, {
      classes: ["A", "B"], jobs, signal: ac.signal, ...(cache ? { cache } : {}),
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
