#!/usr/bin/env node
// Imports every exported runtime module of each workspace npm package in a fresh Node process, from the source tree, before
// anything is compiled or packed. A module that does anything at import (prints, exits non-zero, reads its command line) fails:
// the publish verify's import check (scripts/ci/consume.mjs) imports every export of the sealed tarball, and 0.6.6's verify
// (run 40168412670) failed there after 27 minutes because cli/right-release.mjs and sign-windows.mjs ran at import (#1189).
// Declared bin targets are skipped (consume.mjs only resolves them; their smoke run executes them), as are entries in
// rightkit.publishCheck.skipImport, and a skipImport entry that imports quietly fails as stale. Exports that point at build
// output that does not exist in the source tree (dist/) are listed as not checked here; the sealed-byte import check owns them.
//
//   node scripts/ci/exports-import-effects.mjs [--root <repo>] [package-dir...]
// Needs the workspace's dependencies installed (pnpm install). Exit 0 clean, 1 on any finding, 2 on a usage error.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

/** Runtime file targets of an exports map (conditions flattened, types skipped), as package-relative paths. */
export function runtimeTargets(exportsField) {
  const out = new Map();
  const map = exportsField && typeof exportsField === "object" && !Array.isArray(exportsField) && Object.keys(exportsField).some((k) => k.startsWith(".")) ? exportsField : { ".": exportsField };
  const walk = (key, node, underTypes) => {
    if (typeof node === "string") { if (!underTypes && node.startsWith("./")) out.set(`${key}\u0000${node}`, { key, target: node }); }
    else if (Array.isArray(node)) node.forEach((n) => walk(key, n, underTypes));
    else if (node && typeof node === "object") for (const [k, v] of Object.entries(node)) walk(key, v, underTypes || k === "types");
  };
  for (const [key, target] of Object.entries(map ?? {})) if (!key.includes("*")) walk(key, target, false);
  return [...out.values()];
}

/** Check one package directory. Returns { name, checked, notChecked: [{ key, why }], findings: [{ key, problem, fix }] }. */
export function checkPackage(dir, { spawn = spawnSync, node = process.execPath } = {}) {
  const pkg = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8"));
  const result = { name: pkg.name, checked: 0, notChecked: [], findings: [] };
  if (!pkg.exports) return result;
  const bins = new Set(Object.values(typeof pkg.bin === "string" ? { [pkg.name]: pkg.bin } : (pkg.bin ?? {})).map((b) => `./${String(b).replace(/^\.\//, "")}`));
  const skipImport = pkg.rightkit?.publishCheck?.skipImport ?? [];
  const keys = new Set(runtimeTargets(pkg.exports).map((t) => t.key));
  for (const entry of skipImport) if (!keys.has(entry)) result.findings.push({ key: entry, problem: `${pkg.name}: rightkit.publishCheck.skipImport names ${entry}, which the exports map does not list`, fix: "drop the entry" });
  const env = { ...process.env, NODE_OPTIONS: "", NODE_ENV: "production" };
  for (const { key, target } of runtimeTargets(pkg.exports)) {
    if (!/\.(?:[cm]?js)$/.test(target) || bins.has(target)) continue;
    const file = path.join(dir, target);
    if (!fs.existsSync(file)) { result.notChecked.push({ key, why: `${target} is build output (not in the source tree); the sealed-byte import check covers it` }); continue; }
    const ran = spawn(node, ["--input-type=module", "-e", `await import(${JSON.stringify(pathToFileURL(file).href)}); process.stdout.write("RK_IMPORTED");`], { cwd: dir, encoding: "utf8", env, timeout: 30_000, input: "", windowsHide: true });
    result.checked++;
    const quiet = ran.status === 0 && ran.stdout === "RK_IMPORTED" && (ran.stderr ?? "") === "";
    const shown = `${ran.stdout ?? ""}${ran.stderr ?? ""}`.replace("RK_IMPORTED", "").trim().split("\n")[0].slice(0, 200);
    if (skipImport.includes(key)) {
      if (quiet) result.findings.push({ key, problem: `${pkg.name}: ${key} is listed in rightkit.publishCheck.skipImport but imports quietly`, fix: "it is a module, not a program: drop the skipImport entry" });
    } else if (!quiet) {
      result.findings.push({ key, problem: `${pkg.name}: importing ${key} does something (status ${ran.status ?? ran.error?.code}): ${shown || "(no output)"}`, fix: "a program that reads its command line is spawned by path, never imported: guard its main with an invoked-directly check, or list it in rightkit.publishCheck.skipImport" });
    }
  }
  return result;
}

export function packageDirs(root) {
  const base = path.join(root, "packages");
  return fs.readdirSync(base).map((d) => path.join(base, d)).filter((d) => fs.existsSync(path.join(d, "package.json"))).sort();
}

export function main(argv, io = { log: console.log, error: console.error }) {
  let root = REPO; const dirs = [];
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--root") root = path.resolve(argv[++i] ?? "");
    else if (argv[i].startsWith("-")) { io.error(`exports-import-effects: unknown option ${argv[i]}`); return 2; }
    else dirs.push(path.resolve(root, argv[i]));
  }
  let findings = 0; let checked = 0;
  for (const dir of dirs.length ? dirs : packageDirs(root)) {
    const r = checkPackage(dir);
    checked += r.checked;
    for (const f of r.findings) { findings++; io.error(`FAIL exports-import-effects: ${f.problem}\n  fix: ${f.fix}`); }
  }
  io.log(`exports-import-effects: ${checked} exported module(s) imported in fresh processes; ${findings} finding(s)`);
  return findings ? 1 : 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = main(process.argv.slice(2));
