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
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import crypto from "node:crypto";
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
  const result = { name: pkg.name, checked: 0, notChecked: [], covered: [], findings: [] };
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
    result.covered.push({ key, target });
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

/**
 * The immutable source closure the probe runs against: `git archive HEAD` of `root`, extracted to a fresh directory (no untracked,
 * ignored or locally modified file can reach it), with the live checkout's installed dependencies linked in (root and per-package
 * node_modules) and bound by the lockfile in the tree plus pnpm's installed-state digest. Returns { dir, deps } or { problem }.
 */
export function exportTree(root, { spawn = spawnSync, mkdtemp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-import-effects-")) } = {}) {
  const dir = mkdtemp();
  const tar = path.join(dir, "tree.tar");
  const archived = spawn("git", ["-C", root, "archive", "--format=tar", "-o", tar, "HEAD"], { encoding: "utf8", windowsHide: true });
  if (archived.error || archived.status !== 0) return { problem: `git archive HEAD of ${root} failed: ${(archived.stderr || archived.error?.message || "").trim()}` };
  const extracted = spawn("tar", ["-xf", tar, "-C", dir], { encoding: "utf8", windowsHide: true });
  if (extracted.error || extracted.status !== 0) return { problem: `extracting the exported tree failed: ${(extracted.stderr || extracted.error?.message || "").trim()}` };
  fs.rmSync(tar, { force: true });
  // Dependencies: the tree's lockfile (already part of the tree id) and the installed state the live checkout links in.
  const sha = (f) => { try { return crypto.createHash("sha256").update(fs.readFileSync(f)).digest("hex"); } catch { return null; } };
  const deps = { lock: sha(path.join(dir, "pnpm-lock.yaml")), installed: sha(path.join(root, "node_modules", ".modules.yaml")), linked: [] };
  for (const rel of [".", ...fs.readdirSync(path.join(dir, "packages"), { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => path.join("packages", e.name))]) {
    const live = path.join(root, rel, "node_modules");
    if (fs.existsSync(live) && !fs.existsSync(path.join(dir, rel, "node_modules"))) { fs.symlinkSync(live, path.join(dir, rel, "node_modules"), "dir"); deps.linked.push(rel); }
  }
  return { dir, deps };
}

export function main(argv, io = { log: console.log, error: console.error }, deps = {}) {
  const { spawn = spawnSync, identity = checkoutIdentity, exporter = exportTree, probe = checkPackage, between = null } = deps;
  let root = REPO; const dirs = []; let reportFile = null; let noExport = false;
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--root") root = path.resolve(argv[++i] ?? "");
    else if (argv[i] === "--report") reportFile = path.resolve(argv[++i] ?? "");
    else if (argv[i] === "--no-export") noExport = true;
    else if (argv[i].startsWith("-")) { io.error(`exports-import-effects: unknown option ${argv[i]}`); return 2; }
    else dirs.push(argv[i]);
  }
  // Identity of the checkout before anything runs; a report binds to it only when the same identity holds after the probe.
  const before = identity(root, { spawn });
  let probeRoot = root; let closure = { kind: "working-dir", exported: null, deps: null };
  if (!noExport && before.commit) {
    const exported = exporter(root, { spawn });
    if (exported.problem) { io.error(`exports-import-effects: ${exported.problem}`); return 2; }
    probeRoot = exported.dir; closure = { kind: "exported-tree", exported: exported.dir, deps: exported.deps };
  }
  let findings = 0; let checked = 0; const incomplete = []; const covered = [];
  try {
    const targets = dirs.length ? dirs.map((d) => path.resolve(probeRoot, path.isAbsolute(d) ? path.relative(root, d) : d)) : packageDirs(probeRoot);
    for (const dir of targets) {
      const r = probe(dir, { spawn });
      checked += r.checked;
      for (const n of r.notChecked) { incomplete.push({ package: r.name, key: n.key, target: n.target, why: n.why }); io.log(`INCOMPLETE: ${r.name} ${n.key} (${n.target}): ${n.why}`); }
      for (const c of r.covered) covered.push({ package: r.name, key: c.key, target: c.target });
      for (const f of r.findings) { findings++; io.error(`FAIL exports-import-effects: ${f.problem}\n  fix: ${f.fix}`); }
      if (between) between(dir); // tests: an edit scheduled in the middle of the probe
    }
  } finally { if (closure.kind === "exported-tree") fs.rmSync(closure.exported, { recursive: true, force: true }); }
  const after = identity(root, { spawn });
  const stable = before.commit !== null && before.commit === after.commit && before.tree === after.tree;
  if (reportFile) {
    fs.mkdirSync(path.dirname(reportFile), { recursive: true });
    if (!stable) io.log(`exports-import-effects: the checkout's HEAD moved during the probe (${before.commit ?? "none"} → ${after.commit ?? "none"}); the report records that and no verify will accept it`);
    if (closure.kind !== "exported-tree") io.log("exports-import-effects: probed a working directory, not an exported commit; the report records that and no verify will accept it");
    const { exported, ...closureOut } = closure;
    fs.writeFileSync(reportFile, `${JSON.stringify({ schema: REPORT_SCHEMA, ...before, stable, closure: closureOut, checked, findings, incomplete, covered }, null, 2)}\n`);
  }
  const state = findings ? "RED" : incomplete.length ? (reportFile ? "INCOMPLETE (handed to the sealed-byte check)" : "INCOMPLETE (no --report: nothing downstream can close it)") : "green";
  io.log(`exports-import-effects: ${state}; ${checked} exported module(s) imported in fresh processes from ${closure.kind === "exported-tree" ? `the exported tree of ${before.commit.slice(0, 12)}` : "the working directory"}; ${incomplete.length} export(s) incomplete; ${findings} finding(s)`);
  if (findings) return 1;
  if (incomplete.length && !reportFile) return EXIT_INCOMPLETE;
  return 0;
}

/**
 * The source closure the probe ran against: HEAD commit and tree id of the checkout at `root`, whether the working tree is exactly
 * that tree (`clean`: no modified tracked file, no untracked non-ignored file — either could change what imports), the digest of the
 * probe's own code and the Node that ran it. A report from a dirty or non-git root records that and is unusable downstream: the
 * evidence would describe mutable state no one can re-derive.
 */
export function checkoutIdentity(root, { spawn = spawnSync, node = process.version } = {}) {
  const git = (...args) => { const r = spawn("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024 }); return r.status === 0 ? String(r.stdout ?? "") : null; };
  const rev = (what) => { const v = (git("rev-parse", what) ?? "").trim(); return /^[0-9a-f]{40}$/.test(v) ? v : null; };
  const status = git("status", "--porcelain", "--untracked-files=all");
  const dirty = status === null ? null : status.split(/\r?\n/).filter(Boolean);
  const probe = crypto.createHash("sha256").update(fs.readFileSync(fileURLToPath(import.meta.url))).digest("hex");
  return { commit: rev("HEAD"), tree: rev("HEAD^{tree}"), clean: dirty !== null && dirty.length === 0, dirty: dirty === null ? null : dirty.length, probe, node };
}

const ROW_OK = (r) => typeof r?.package === "string" && typeof r?.key === "string" && typeof r?.target === "string" && r.target.startsWith("./");
/**
 * Read a --report file. Returns { report } or { problem }. The report is usable only when it is the probe's schema, carries a 40-hex
 * commit, reports 0 findings (a red probe is never carried forward as "incomplete"), has integer counts and well-formed rows. With
 * `commit`, it must have been written for that commit (the probed checkout's HEAD, compared to the dispatched commit).
 */
export function readIncompleteReport(file, { commit = null, tree = null, probe = null, node = null } = {}) {
  let report;
  try { report = JSON.parse(fs.readFileSync(file, "utf8")); } catch (e) { return { problem: `import-effects report ${file} cannot be read: ${e.message}` }; }
  const bad = (why) => ({ problem: `import-effects report ${file} is not ${REPORT_SCHEMA}: ${why}` });
  if (report?.schema !== REPORT_SCHEMA) return bad("wrong schema");
  if (!Array.isArray(report.incomplete) || !report.incomplete.every(ROW_OK)) return bad("incomplete rows must be { package, key, target }");
  if (!Array.isArray(report.covered) || !report.covered.every(ROW_OK)) return bad("covered rows must be { package, key, target }");
  if (!Number.isInteger(report.checked) || report.checked < 0 || report.checked !== report.covered.length) return bad(`checked (${report.checked}) must equal the number of covered rows (${report.covered?.length})`);
  if (report.findings !== 0) return bad(`findings is ${report.findings}; a red probe is not carried forward`);
  if (!/^[0-9a-f]{40}$/.test(report.commit ?? "") || !/^[0-9a-f]{40}$/.test(report.tree ?? "")) return bad("no checkout commit/tree recorded");
  if (report.closure?.kind !== "exported-tree") return bad("the probe ran on a working directory, not on the exported commit; unbound mutable inputs are refused");
  if (report.stable !== true) return bad("the checkout's HEAD moved while the probe ran; the report is not bound to one commit");
  if (!/^[0-9a-f]{64}$/.test(report.closure.deps?.lock ?? "") || !/^[0-9a-f]{64}$/.test(report.closure.deps?.installed ?? "")) return bad("the dependency closure (lockfile and installed state) is not recorded");
  if (!/^[0-9a-f]{64}$/.test(report.probe ?? "") || typeof report.node !== "string") return bad("no probe code digest / Node version recorded");
  if (probe !== null && report.probe !== probe) return { problem: `import-effects report ${file} was written by probe ${report.probe.slice(0, 12)}…, not the trusted ${String(probe).slice(0, 12)}…` };
  if (node !== null && report.node !== node) return { problem: `import-effects report ${file} was written under Node ${report.node}, not the qualified ${node}` };
  if (commit !== null && report.commit !== commit) return { problem: `import-effects report ${file} was written for commit ${report.commit}, not the dispatched ${commit}` };
  if (tree !== null && report.tree !== tree) return { problem: `import-effects report ${file} was written for tree ${report.tree}, not this checkout's ${tree}` };
  return { report };
}

/**
 * The dist/ runtime exports a sealed package.json declares, each { key, target, pattern }: every one must be accounted for by the
 * report (an INCOMPLETE row closed by the tarball import, or a covered row the probe imported itself), with the same target, or
 * the report is not this candidate's (forged COMPLETE). A wildcard key is returned with pattern: true; it can never be closed,
 * because neither the probe nor the tarball check imports a pattern, so the closure refuses it.
 */
export function expectedIncomplete(pkg) {
  const out = [];
  for (const { key, target } of runtimeTargets(pkg.exports ?? {})) if (/\.(?:[cm]?js)$/.test(target.replace("*", "x")) && target.startsWith("./dist/")) out.push({ key, target, pattern: key.includes("*") || target.includes("*") });
  return out;
}

/** The sealed-byte stage's closure: every INCOMPLETE export of `name` must appear among the specs the tarball import actually imported. */
export function closeIncomplete({ report, name, importedSpecs, expected = null }) {
  const problems = [];
  const mine = [...(report.covered ?? []).map((r) => ({ ...r, list: "covered" })), ...report.incomplete.map((r) => ({ ...r, list: "incomplete" }))].filter((r) => r.package === name);
  // Exact set: one row per key, in one list, and no row for an export the sealed package does not declare.
  const seen = new Map();
  for (const r of mine) {
    const prior = seen.get(r.key);
    if (prior) problems.push(prior.list === r.list && prior.target === r.target ? `${name}: the import-effects report lists ${r.key} twice; a report with duplicate rows is not the probe's` : `${name}: the import-effects report lists ${r.key} as both ${prior.list} (${prior.target}) and ${r.list} (${r.target}); conflicting rows are refused`);
    else seen.set(r.key, r);
  }
  if (expected !== null) {
    const known = new Set(expected.map((e) => e.key));
    for (const r of mine) if (!known.has(r.key) && r.target.startsWith("./dist/")) problems.push(`${name}: the import-effects report names ${r.key} (${r.target}), which the sealed package does not export from build output; the report is not this candidate's`);
  }
  for (const e of expected ?? []) {
    if (e.pattern) { problems.push(`${name}: wildcard export ${e.key} → ${e.target} points at build output; neither the source probe nor the sealed-tarball check imports a pattern, so it cannot be closed: enumerate the exports`); continue; }
    const row = seen.get(e.key);
    if (!row) problems.push(`${name}: the import-effects report accounts for neither an INCOMPLETE nor a covered ${e.key} (${e.target}) although the sealed package exports it from build output; the report is not this candidate's`);
    else if (row.target !== e.target) problems.push(`${name}: the import-effects report binds ${e.key} to ${row.target}, the sealed package to ${e.target}; the report is not this candidate's`);
  }
  for (const row of mine.filter((r) => r.list === "incomplete")) {
    const spec = row.key === "." ? name : `${name}/${row.key.replace(/^\.\//, "")}`;
    if (!importedSpecs.includes(spec)) problems.push(`${name}: INCOMPLETE not closed: ${spec} (${row.target ?? row.key}) was left to the sealed-byte check by the source probe and the sealed tarball import check did not import it`);
  }
  return problems;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = main(process.argv.slice(2));
