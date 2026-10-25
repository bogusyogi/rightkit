#!/usr/bin/env node
// Imports every exported runtime module of each workspace npm package in a fresh Node process, from the source tree, before
// anything is compiled or packed. A module that does anything at import (prints, exits non-zero, reads its command line) fails:
// the publish verify's import check (scripts/ci/consume.mjs) imports every export of the sealed tarball, and 0.6.6's verify
// (run 40168412670) failed there after 27 minutes because cli/right-release.mjs and sign-windows.mjs ran at import (#1189).
// Declared bin targets are skipped (consume.mjs only resolves them; their smoke run executes them), as are entries in
// rightkit.publishCheck.skipImport, and a skipImport entry that imports quietly fails as stale. A wildcard key (`./*`) is expanded
// against the source tree and every match is probed. An export whose target is missing from the source tree is accepted as
// "not checked here" only under ./dist/ (build output the sealed-byte import check of the verify job owns) and is always printed;
// any other missing target, and a wildcard that matches nothing, is a finding: a silent skip was a false green (simulation A5.06/A5.07).
// Each import runs under Node's permission model (`--permission --allow-fs-read=*`): a module that writes a file, starts a child
// process or a worker at import is refused by the runtime and surfaces as a non-zero exit, so an effect without output is a
// finding too (simulation A5.07b). Needs Node 22.13+ (CI pins 26).
//
//   node scripts/ci/exports-import-effects.mjs [--root <repo>] [--report <file>] [package-dir...]
// Needs the workspace's dependencies installed (pnpm install). Exit 1 on any finding, 2 on a usage error. Exports left to the
// sealed-byte check are INCOMPLETE, not green: without --report the run exits 3; with --report it writes them (schema
// rightkit.import-effects/1) and exits 0 only because the verify job's consume step must read that report and refuse any
// listed export the sealed tarball did not import (scripts/ci/consume.mjs, RK_IMPORT_EFFECTS_REPORT).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
/** The probe child runs read-only: file writes, child processes and workers at import are runtime errors, never silent effects. */
export const PROBE_FLAGS = Object.freeze(["--permission", "--allow-fs-read=*"]);

/** Runtime file targets of an exports map (conditions flattened, types skipped), as package-relative paths. */
export function runtimeTargets(exportsField) {
  const out = new Map();
  const map = exportsField && typeof exportsField === "object" && !Array.isArray(exportsField) && Object.keys(exportsField).some((k) => k.startsWith(".")) ? exportsField : { ".": exportsField };
  const walk = (key, node, underTypes) => {
    if (typeof node === "string") { if (!underTypes && node.startsWith("./")) out.set(`${key}\u0000${node}`, { key, target: node }); }
    else if (Array.isArray(node)) node.forEach((n) => walk(key, n, underTypes));
    else if (node && typeof node === "object") for (const [k, v] of Object.entries(node)) walk(key, v, underTypes || k === "types");
  };
  for (const [key, target] of Object.entries(map ?? {})) walk(key, target, false);
  return [...out.values()];
}

/** Expand one wildcard export (`./lib/*` → `./src/*.mjs`) against the package directory. Returns [{ key, target }] of real files. */
export function expandWildcard(dir, key, target) {
  const star = target.indexOf("*");
  if (star < 0) return [{ key, target }];
  const prefix = target.slice(0, star); const suffix = target.slice(star + 1);
  const base = path.join(dir, prefix.slice(0, prefix.lastIndexOf("/") + 1)); const head = prefix.slice(prefix.lastIndexOf("/") + 1);
  const list = (d, rel) => { let names = []; try { names = fs.readdirSync(d, { withFileTypes: true }); } catch { return []; } return names.flatMap((e) => (e.isDirectory() ? list(path.join(d, e.name), `${rel}${e.name}/`) : [`${rel}${e.name}`])); };
  return list(base, "").filter((rel) => rel.startsWith(head) && rel.endsWith(suffix) && rel.length >= head.length + suffix.length).sort()
    .map((rel) => { const inner = rel.slice(head.length, rel.length - suffix.length); return { key: key.replace("*", inner), target: `${prefix.slice(0, prefix.lastIndexOf("/") + 1)}${rel}` }; });
}

/** Check one package directory. Returns { name, checked, notChecked: [{ key, why }], findings: [{ key, problem, fix }] }. */
export function checkPackage(dir, { spawn = spawnSync, node = process.execPath } = {}) {
  const pkg = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8"));
  const result = { name: pkg.name, checked: 0, notChecked: [], findings: [] };
  if (!pkg.exports) return result;
  const bins = new Set(Object.values(typeof pkg.bin === "string" ? { [pkg.name]: pkg.bin } : (pkg.bin ?? {})).map((b) => `./${String(b).replace(/^\.\//, "")}`));
  const skipImport = pkg.rightkit?.publishCheck?.skipImport ?? [];
  const declared = runtimeTargets(pkg.exports);
  const targets = [];
  for (const { key, target } of declared) {
    if (!key.includes("*")) { targets.push({ key, target }); continue; }
    const matches = expandWildcard(dir, key, target);
    if (matches.length === 0 && !target.startsWith("./dist/")) result.findings.push({ key, problem: `${pkg.name}: wildcard export ${key} → ${target} matches no file in the source tree`, fix: "fix the pattern or drop the export; a wildcard that resolves nothing is a broken surface" });
    if (matches.length === 0 && target.startsWith("./dist/")) result.notChecked.push({ key, target, why: `${target} is build output (not in the source tree); the sealed-byte import check covers it` });
    targets.push(...matches);
  }
  const keys = new Set(targets.map((t) => t.key));
  for (const entry of skipImport) if (!keys.has(entry)) result.findings.push({ key: entry, problem: `${pkg.name}: rightkit.publishCheck.skipImport names ${entry}, which the exports map does not list`, fix: "drop the entry" });
  const env = { ...process.env, NODE_OPTIONS: "", NODE_ENV: "production" };
  for (const { key, target } of targets) {
    if (!/\.(?:[cm]?js)$/.test(target) || bins.has(target)) continue;
    const file = path.join(dir, target);
    if (!fs.existsSync(file)) {
      if (target.startsWith("./dist/")) { result.notChecked.push({ key, target, why: `${target} is build output (not in the source tree); the sealed-byte import check covers it` }); continue; }
      result.findings.push({ key, problem: `${pkg.name}: export ${key} → ${target} does not exist in the source tree and is not build output under ./dist/`, fix: "fix the export target; a missing non-dist target is a broken surface, not something to skip" });
      continue;
    }
    const ran = spawn(node, [...PROBE_FLAGS, "--input-type=module", "-e", `await import(${JSON.stringify(pathToFileURL(file).href)}); process.stdout.write("RK_IMPORTED");`], { cwd: dir, encoding: "utf8", env, timeout: 30_000, input: "", windowsHide: true });
    result.checked++;
    const quiet = ran.status === 0 && ran.stdout === "RK_IMPORTED" && (ran.stderr ?? "") === "";
    const lines = `${ran.stdout ?? ""}${ran.stderr ?? ""}`.replace("RK_IMPORTED", "").split("\n").map((l) => l.trim()).filter(Boolean);
    const shown = (lines.find((l) => /Error|restricted/.test(l)) ?? lines[0] ?? "").slice(0, 200);
    if (skipImport.includes(key)) {
      if (quiet) result.findings.push({ key, problem: `${pkg.name}: ${key} is listed in rightkit.publishCheck.skipImport but imports quietly`, fix: "it is a module, not a program: drop the skipImport entry" });
    } else if (!quiet) {
      result.findings.push({ key, problem: `${pkg.name}: importing ${key} does something (status ${ran.status ?? ran.error?.code}): ${shown || "(no output)"}`, fix: "a program that reads its command line is spawned by path, never imported: guard its main with an invoked-directly check, or list it in rightkit.publishCheck.skipImport; a module that writes files or starts processes at import moves that work into a function" });
    }
  }
  return result;
}

export function packageDirs(root) {
  const base = path.join(root, "packages");
  return fs.readdirSync(base).map((d) => path.join(base, d)).filter((d) => fs.existsSync(path.join(d, "package.json"))).sort();
}

export const REPORT_SCHEMA = "rightkit.import-effects/1";
export const EXIT_INCOMPLETE = 3;

export function main(argv, io = { log: console.log, error: console.error }) {
  let root = REPO; const dirs = []; let reportFile = null;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--root") root = path.resolve(argv[++i] ?? "");
    else if (argv[i] === "--report") reportFile = path.resolve(argv[++i] ?? "");
    else if (argv[i].startsWith("-")) { io.error(`exports-import-effects: unknown option ${argv[i]}`); return 2; }
    else dirs.push(path.resolve(root, argv[i]));
  }
  let findings = 0; let checked = 0; const incomplete = [];
  for (const dir of dirs.length ? dirs : packageDirs(root)) {
    const r = checkPackage(dir);
    checked += r.checked;
    for (const n of r.notChecked) { incomplete.push({ package: r.name, key: n.key, target: n.target, why: n.why }); io.log(`INCOMPLETE: ${r.name} ${n.key} (${n.target}): ${n.why}`); }
    for (const f of r.findings) { findings++; io.error(`FAIL exports-import-effects: ${f.problem}\n  fix: ${f.fix}`); }
  }
  if (reportFile) {
    fs.mkdirSync(path.dirname(reportFile), { recursive: true });
    fs.writeFileSync(reportFile, `${JSON.stringify({ schema: REPORT_SCHEMA, commit: process.env.GITHUB_SHA ?? null, checked, findings, incomplete }, null, 2)}\n`);
  }
  const state = findings ? "RED" : incomplete.length ? (reportFile ? "INCOMPLETE (handed to the sealed-byte check)" : "INCOMPLETE (no --report: nothing downstream can close it)") : "green";
  io.log(`exports-import-effects: ${state}; ${checked} exported module(s) imported in fresh processes; ${incomplete.length} export(s) incomplete; ${findings} finding(s)`);
  if (findings) return 1;
  if (incomplete.length && !reportFile) return EXIT_INCOMPLETE;
  return 0;
}

/** Read a --report file. Returns { report } or { problem }. With `commit`, the report must have been written for that commit. */
export function readIncompleteReport(file, { commit = null } = {}) {
  let report;
  try { report = JSON.parse(fs.readFileSync(file, "utf8")); } catch (e) { return { problem: `import-effects report ${file} cannot be read: ${e.message}` }; }
  if (report?.schema !== REPORT_SCHEMA || !Array.isArray(report.incomplete) || report.incomplete.some((r) => typeof r?.package !== "string" || typeof r?.key !== "string")) return { problem: `import-effects report ${file} is not ${REPORT_SCHEMA}` };
  if (commit !== null && report.commit !== commit) return { problem: `import-effects report ${file} was written for commit ${report.commit ?? "(none)"}, not the dispatched ${commit}` };
  return { report };
}

/** The dist/ runtime exports a sealed package.json declares: every one must be in the report's INCOMPLETE rows for that package, or the report is not the probe's (forged COMPLETE). */
export function expectedIncomplete(pkg) {
  const out = [];
  for (const { key, target } of runtimeTargets(pkg.exports ?? {})) if (!key.includes("*") && /\.(?:[cm]?js)$/.test(target) && target.startsWith("./dist/")) out.push({ key, target });
  return out;
}

/** The sealed-byte stage's closure: every INCOMPLETE export of `name` must appear among the specs the tarball import actually imported. */
export function closeIncomplete({ report, name, importedSpecs, expected = null }) {
  const problems = [];
  const keys = new Set(report.incomplete.filter((r) => r.package === name).map((r) => r.key));
  for (const e of expected ?? []) if (!keys.has(e.key)) problems.push(`${name}: the import-effects report does not list ${e.key} (${e.target}) as INCOMPLETE although the sealed package exports it from build output; the report is not this candidate's`);
  for (const row of report.incomplete.filter((r) => r.package === name)) {
    const spec = row.key === "." ? name : `${name}/${row.key.replace(/^\.\//, "")}`;
    if (!importedSpecs.includes(spec)) problems.push(`${name}: INCOMPLETE not closed: ${spec} (${row.target ?? row.key}) was left to the sealed-byte check by the source probe and the sealed tarball import check did not import it`);
  }
  return problems;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = main(process.argv.slice(2));
