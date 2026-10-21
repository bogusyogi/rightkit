#!/usr/bin/env node
// Mandatory local gate (Adrian, 2026-10-22: "local deterministic checks ... mandatory for all
// builds"). Runs from the pre-push hook (.githooks/pre-push) on every push, so it never depends on
// an agent remembering. Deterministic and compile-free: rustfmt --check, JS syntax, JSON parse,
// generated-docs drift, the embedded ownership catalog, and the scripts/ci test suite. CI still
// compiles, lints and tests everything; this stops pushes that would fail CI's hygiene.
//
// Content checks run on an export of HEAD (exactly what is pushed), never on uncommitted work in
// the shared checkout, so another lane's edits can neither hide nor cause a failure.
//
// Usage: node scripts/local-gate.mjs [--base <ref>]   (default base: origin/main)
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const live = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const base = args.includes("--base") ? args[args.indexOf("--base") + 1] : "origin/main";

const run = (cmd, argv, opts = {}) =>
  spawnSync(cmd, argv, { cwd: live, encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024, ...opts });

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

// 1. rustfmt --check on changed Rust files, per crate edition.
function findRustfmt() {
  if (process.env.RUSTFMT && existsSync(process.env.RUSTFMT)) return process.env.RUSTFMT;
  const exe = process.platform === "win32" ? "rustfmt.exe" : "rustfmt";
  const toolchains = path.join(process.env.RUSTUP_HOME || path.join(os.homedir(), ".rustup"), "toolchains");
  if (existsSync(toolchains)) {
    const found = readdirSync(toolchains)
      .map((d) => ({ d, v: (d.match(/^(\d+)\.(\d+)(?:\.(\d+))?-/) || []).slice(1).map((n) => Number(n || 0)) }))
      .filter((t) => t.v.length && existsSync(path.join(toolchains, t.d, "bin", exe)))
      .sort((a, b) => b.v[0] - a.v[0] || b.v[1] - a.v[1] || b.v[2] - a.v[2]);
    if (found.length) return path.join(toolchains, found[0].d, "bin", exe);
  }
  const which = run(process.platform === "win32" ? "where" : "which", [exe]);
  return which.status === 0 ? which.stdout.split(/\r?\n/)[0].trim() : null;
}
const rustFiles = changed.filter((f) => f.endsWith(".rs"));
if (rustFiles.length) {
  const rustfmt = findRustfmt();
  if (!rustfmt) fail("rustfmt", "no rustfmt found (set RUSTFMT); refusing to push unformatted Rust");
  else {
    const byEdition = new Map();
    for (const f of rustFiles) {
      let dir = path.dirname(path.join(repo, f));
      let edition = "2021";
      while (dir.startsWith(repo)) {
        const manifest = path.join(dir, "Cargo.toml");
        if (existsSync(manifest)) {
          const m = readFileSync(manifest, "utf8").match(/^edition\s*=\s*"(\d{4})"/m);
          if (m) edition = m[1];
          break;
        }
        dir = path.dirname(dir);
      }
      byEdition.set(edition, [...(byEdition.get(edition) || []), f]);
    }
    for (const [edition, files] of byEdition) {
      const r = run(rustfmt, ["--check", "--edition", edition, ...files], { cwd: repo });
      if (r.status !== 0) {
        const bad = [...new Set((r.stdout.match(/Diff in .+?:\d+:/g) || []).map((l) => l.replace(/^Diff in /, "").replace(/:\d+:$/, "")))];
        fail("rustfmt", `format with "${rustfmt}" --edition ${edition}: ${bad.join(", ") || files.join(", ")}`);
      }
    }
  }
}

// 2. JS syntax.
for (const f of changed.filter((f) => /\.(mjs|cjs|js)$/.test(f) && !f.includes("node_modules"))) {
  const r = run(process.execPath, ["--check", f], { cwd: repo });
  if (r.status !== 0) fail("node --check", `${f}: ${r.stderr.split(/\r?\n/).find((l) => l.trim()) || "syntax error"}`);
}

// 3. JSON parse.
for (const f of changed.filter((f) => f.endsWith(".json") && !f.includes("node_modules"))) {
  try {
    JSON.parse(readFileSync(path.join(repo, f), "utf8").replace(/^﻿/, ""));
  } catch (e) {
    fail("json", `${f}: ${e.message}`);
  }
}

// 4. Embedded ownership catalog must equal its source.
const catalogSrc = path.join(repo, "packages/release/rightkit-ownership.json");
const catalogCopy = path.join(repo, "crates/rightkit-ownership/catalog/ownership-catalog.json");
if (existsSync(catalogSrc) && existsSync(catalogCopy) && !readFileSync(catalogSrc).equals(readFileSync(catalogCopy))) {
  fail("ownership catalog", "copy packages/release/rightkit-ownership.json to crates/rightkit-ownership/catalog/ownership-catalog.json");
}

// 5. Generated docs drift (INDEX.md, docs/CAPABILITY-MAP.md).
const gen = run(process.execPath, [path.join("rightkit", "scripts", "generate-index.mjs"), "--check"], { cwd: exportRoot });
if (gen.status !== 0) fail("generated docs", `run node rightkit/scripts/generate-index.mjs from ${path.dirname(live)} and commit the result`);

// 6. CI contract and gate tests (pure Node, deterministic). They need the installed workspace
// packages, so they run in the live checkout.
const ci = run(process.execPath, ["--test", "scripts/ci/*.test.mjs"]);
if (ci.status !== 0) {
  const failed = (ci.stdout.match(/^not ok \d+ - .+$/gm) || []).slice(0, 10);
  fail("scripts/ci tests", failed.join("; ") || "node --test scripts/ci/*.test.mjs failed");
}

cleanup();
if (failures.length) {
  console.error(`local-gate: BLOCKED (${failures.length})`);
  for (const f of failures) console.error(`  - ${f}`);
  console.error("Fix and push again. Never bypass with --no-verify.");
  process.exit(1);
}
console.log(`local-gate: pass (${changed.length} changed file(s) vs ${base})`);
