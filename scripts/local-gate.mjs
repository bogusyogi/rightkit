#!/usr/bin/env node
// Mandatory local gate (Adrian, 2026-10-22: "local deterministic checks ... mandatory for all
// builds"). Runs from the pre-push hook (.githooks/pre-push) on every push, so it never depends on
// an agent remembering. Deterministic: every applicable scan for the changed files, run by the one
// scan module (packages/release/deterministic-scans.mjs), plus the scripts/ci test suite. CI still
// compiles, lints and tests everything; this stops pushes that would fail CI's hygiene.
//
// Content checks run on an export of HEAD (exactly what is pushed), never on uncommitted work in
// the shared checkout, so another lane's edits can neither hide nor cause a failure.
//
// Usage: node scripts/local-gate.mjs [--base <ref>]   (default base: origin/main)
// From the pre-push hook, git's ref lines arrive on stdin: the commit being pushed must be the checked-out
// HEAD (broker scans read the live checkout), and the remote's current commit is the diff base. To push an
// earlier commit of the local stack, check it out first (a detached HEAD is fine), push HEAD:main, return.
// RK_GATE_JOBS (default 4) sets how many compile scans run side by side; tests stay one at a time.
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { planScans, runScans, scanDeferrals, scanFailures } from "../packages/release/deterministic-scans.mjs";
import { changedLockNames, parseLock, reach } from "./ci/select.mjs";

const live = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
let base = args.includes("--base") ? args[args.indexOf("--base") + 1] : "origin/main";

const run = (cmd, argv, opts = {}) =>
  spawnSync(cmd, argv, { cwd: live, encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024, ...opts });

// git pre-push: "<local ref> <local sha> <remote ref> <remote sha>" per pushed ref.
{
  const ZERO = /^0+$/;
  const head = run("git", ["rev-parse", "HEAD"]).stdout.trim();
  const lines = process.stdin.isTTY ? [] : readFileSync(0, "utf8").split(/\r?\n/).map((l) => l.trim().split(/\s+/)).filter((f) => f.length === 4);
  for (const [, localSha, remoteRef, remoteSha] of lines) {
    if (ZERO.test(localSha)) continue; // a delete pushes no content
    if (localSha !== head) {
      console.error(`local-gate: BLOCKED: pushing ${localSha.slice(0, 10)} to ${remoteRef}, but HEAD is ${head.slice(0, 10)}; broker scans read the checkout, so check the pushed commit out first (git checkout --detach ${localSha.slice(0, 10)}), push HEAD:main, then return`);
      process.exit(1);
    }
    if (!ZERO.test(remoteSha) && run("git", ["cat-file", "-e", `${remoteSha}^{commit}`]).status === 0) base = remoteSha;
  }
}

const exportRoot = mkdtempSync(path.join(os.tmpdir(), "rk-local-gate-"));
const repo = path.join(exportRoot, "rightkit");
const cleanup = () => rmSync(exportRoot, { recursive: true, force: true });
{
  const tar = path.join(exportRoot, "head.tar");
  const a = run("git", ["archive", "--format=tar", "--prefix=rightkit/", "-o", tar, "HEAD"]);
  const x = a.status === 0 ? run("tar", ["-xf", "head.tar"], { cwd: exportRoot }) : a;
  if (a.status !== 0 || x.status !== 0) {
    console.error(`local-gate: cannot export HEAD: ${(a.stderr || x.stderr || "").trim()}`);
    cleanup();
    process.exit(2);
  }
}

const failures = [];
const fail = (check, detail) => failures.push(`${check}: ${detail}`);

// Files changed in the commits being pushed.
const diff = run("git", ["diff", "--name-only", "--diff-filter=ACMR", `${base}...HEAD`]);
if (diff.status !== 0) {
  console.error(`local-gate: cannot diff against ${base}: ${diff.stderr.trim()}`);
  cleanup();
  process.exit(2);
}
const changed = [...new Set(diff.stdout.split(/\r?\n/).map((f) => f.trim()).filter(Boolean))]
  .filter((f) => existsSync(path.join(repo, f)));

// Deterministic scans: rustfmt, node --check, JSON/TOML/YAML parse, ownership catalog, generated docs,
// actionlint, lockfiles, cargo metadata/deny/audit/package list, pnpm audit, secret scan, license,
// changelog/version and right-git drift (class A); then clippy, check, doc, tsc, eslint and swift
// (class B, only after every class A scan passed) all live in ONE module,
// packages/release/deterministic-scans.mjs (#833, #888, #824). A required tool that is missing FAILS;
// nothing is skipped. Rust scans run in the live checkout through `rightkit cargo`; content checks run
// on the export of HEAD. Tests of the changed crates and packages, and scripts/ci, run here too.
// Every scan runs here, before any push or CI build (Adrian 2026-10-22, #888): class B too, through
// the broker, under the committed local compile grant. A deferred scan is a failure, never a pass.
//
// Broker scans (cargo, pnpm, scripts/ci tests) read the live checkout, so it must equal HEAD there:
// an uncommitted fix could otherwise hide a committed failure. Any change or untracked file under the
// paths they read blocks the gate.
const LIVE_INPUTS = ["crates", "packages", "scripts", "vendor", "ci", "Cargo.toml", "Cargo.lock", ".cargo", "rust-toolchain.toml", "deny.toml",
  "clippy.toml", "rustfmt.toml", "package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", ".npmrc"];
const dirty = run("git", ["status", "--porcelain", "--untracked-files=all", "--", ...LIVE_INPUTS]);
const dirtyFiles = dirty.status === 0
  ? dirty.stdout.split(/\r?\n/).filter(Boolean).filter((l) => !l.includes("node_modules"))
  : [`git status failed: ${dirty.stderr.trim()}`];
if (dirtyFiles.length) {
  console.error("local-gate: BLOCKED: the checkout differs from HEAD where broker scans read it; commit or stash first:");
  for (const l of dirtyFiles.slice(0, 20)) console.error(`  ${l}`);
  cleanup();
  process.exit(1);
}

// Cargo only through the broker: `rightkit cargo`, or a `cargo` on PATH that is the managed shim
// (it runs packages/release/cargo-guard.mjs). Any other cargo would bypass the broker: refused.
const which = (cmd) => {
  const r = run(process.platform === "win32" ? "where" : "which", [cmd]);
  return r.status === 0 ? r.stdout.split(/\r?\n/).map((l) => l.trim()).filter(Boolean) : [];
};
const isManagedShim = (file) => {
  try { return readFileSync(file, "utf8").includes("cargo-guard.mjs"); } catch { return false; }
};
let cargoCommand = null;
if (which("rightkit").length) cargoCommand = ["rightkit", "cargo"];
else {
  const first = which("cargo")[0];
  if (first && isManagedShim(first)) cargoCommand = ["cargo"];
}
if (!cargoCommand) {
  console.error("local-gate: BLOCKED: neither `rightkit` nor the managed cargo shim (cargo-guard.mjs) is first on PATH; plain cargo is never used");
  cleanup();
  process.exit(1);
}
// A lock-only workspace change (Cargo.lock, vendor/) reaches only the crates whose resolved dependency closure
// holds a changed package (the same lock graph CI's selection uses). Any other workspace Rust file, or a base
// without a lock, keeps every crate in scope.
const rustScope = (() => {
  const wide = changed.filter((f) => /^(Cargo\.(toml|lock)|rust-toolchain(\.toml)?|clippy\.toml|rustfmt\.toml|\.cargo\/.+|vendor\/.+)$/.test(f));
  if (!wide.length || wide.some((f) => f !== "Cargo.lock" && !f.startsWith("vendor/"))) return null;
  const before = run("git", ["show", `${base}:Cargo.lock`]);
  if (before.status !== 0) return null;
  const next = parseLock(readFileSync(path.join(repo, "Cargo.lock"), "utf8"));
  const touched = changedLockNames(parseLock(before.stdout), next);
  for (const f of wide) if (f.startsWith("vendor/")) touched.add(f.split("/")[1]);
  const memo = new Map();
  const scope = new Set();
  for (const [name] of next) {
    if (!existsSync(path.join(repo, "crates", name, "Cargo.toml"))) continue;
    if (touched.has(name) || [...touched].some((t) => reach(next, name, memo).has(t))) scope.add(name);
  }
  return scope;
})();
const plan = planScans({
  changed, contentRoot: repo, liveRoot: live, includeTests: true, rustScope,
  localCompile: existsSync(path.join(live, ".rightkit-local-build.json")),
  cargoCommand,
  // The same per-crate feature sets CI uses (crates whose features cannot all be on together).
  crateFeatures: (() => { try { return JSON.parse(readFileSync(path.join(repo, "ci/gate.config.json"), "utf8")).rust ?? {}; } catch { return {}; } })(),
});
const jobs = Math.max(1, Number(process.env.RK_GATE_JOBS ?? 4) || 4);
console.log(`local-gate: ${plan.scans.length} scan(s) for ${changed.length} changed file(s) vs ${base.slice(0, 12)}; compile scans ${jobs} at a time`);
const scanResult = await runScans(plan, {
  classes: ["A", "B"], jobs,
  onResult: (row, p) => console.log(`local-gate: [${p.done}/${p.total}] ${row.status.padEnd(7)} ${row.id} (${p.seconds}s)${row.status === "fail" ? ` :: ${String(row.detail ?? "").slice(0, 300)}` : ""}`),
});
for (const line of scanFailures(scanResult)) failures.push(line);
const deferrals = scanDeferrals(scanResult);
for (const line of deferrals) failures.push(`DEFERRED ${line}`);
for (const note of scanResult.notes) console.log(`local-gate: ${note}`);

cleanup();
if (failures.length) {
  console.error(`local-gate: BLOCKED (${failures.length})`);
  for (const f of failures) console.error(`  - ${f}`);
  console.error("Fix and push again. Never bypass with --no-verify.");
  process.exit(1);
}
console.log(`local-gate: pass (${changed.length} changed file(s) vs ${base}); ${deferrals.length} scan(s) deferred, not passed${deferrals.length ? `: ${scanResult.deferred.join(", ")}` : ""}`);
