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
export const INSTALL_COMMAND = "pnpm install --frozen-lockfile --ignore-scripts --ignore-pnpmfile --offline --package-import-method=copy --config.ignore-scripts=true --config.ignore-pnpmfile=true --config.side-effects-cache=false --config.manage-package-manager-versions=false";

/**
 * The immutable closure the probe runs against: `git archive <commit>` (the exact commit captured before anything ran) extracted
 * to a fresh directory, then `pnpm install --frozen-lockfile --ignore-scripts --offline` INSIDE it. Nothing from the live checkout
 * is linked in: workspace packages resolve to the exported (committed) sources, and every third-party byte comes from the
 * content-addressed store as the tree's lockfile names it (pnpm verifies each package against the lockfile's integrity hash).
 * The report binds the dependency closure by bytes: a sha256 over every installed file's path and content under node_modules
 * (`deps.store`), plus the lockfile digest, the installed-state file and the pnpm version. Returns { dir, deps } or { problem }.
 */
export function exportTree(root, { commit, spawn = spawnSync, mkdtemp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-import-effects-")) } = {}) {
  if (!/^[0-9a-f]{40}$/.test(commit ?? "")) return { problem: "exportTree needs the exact commit to archive" };
  const dir = mkdtemp();
  const tar = path.join(dir, "tree.tar");
  const archived = spawn("git", ["-C", root, "archive", "--format=tar", "-o", tar, commit], { encoding: "utf8", windowsHide: true });
  if (archived.error || archived.status !== 0) return { problem: `git archive ${commit} of ${root} failed: ${(archived.stderr || archived.error?.message || "").trim()}` };
  const extracted = spawn("tar", ["-xf", tar, "-C", dir], { encoding: "utf8", windowsHide: true });
  if (extracted.error || extracted.status !== 0) return { problem: `extracting the exported tree failed: ${(extracted.stderr || extracted.error?.message || "").trim()}` };
  fs.rmSync(tar, { force: true });
  const sha = (f) => { try { return crypto.createHash("sha256").update(fs.readFileSync(f)).digest("hex"); } catch { return null; } };
  // `--package-import-method=copy`: every dependency file is a private copy inside the export, never a hardlink or symlink into
  // the content-addressed store or the live checkout, so a later mutation of either cannot reach the probed bytes and the digest
  // below is over the bytes that were actually imported. `--ignore-scripts`: no lifecycle script or build hook runs (nothing is
  // compiled here; compiles are admitted elsewhere, after class A).
  // `--ignore-scripts` suppresses lifecycle scripts; `--ignore-pnpmfile` suppresses the repository's own pnpm hooks (.pnpmfile.cjs
  // readPackage/afterAllResolved: candidate code that would otherwise run during dependency setup, before class A had a say);
  // `--config.*` repeats both so no project .npmrc can switch them back. The environment is rebuilt from an allowlist: inherited
  // `npm_config_*`/`PNPM_*`/NODE_OPTIONS never reach the install, so nothing outside the tree can re-enable scripts or hooks.
  const INSTALL = ["install", "--frozen-lockfile", "--ignore-scripts", "--ignore-pnpmfile", "--offline", "--package-import-method=copy", "--config.ignore-scripts=true", "--config.ignore-pnpmfile=true", "--config.side-effects-cache=false", "--config.manage-package-manager-versions=false"];
  const deps = { lock: sha(path.join(dir, "pnpm-lock.yaml")), pnpm: null, installed: null, store: null, files: 0, hardlinks: 0, externalLinks: 0, install: INSTALL_COMMAND, pnpmIdentity: null, closureStable: null };
  if (deps.lock) {
    const env = installEnvironment(process.env);
    deps.pnpmIdentity = pnpmIdentity({ spawn, env });
    if (deps.pnpmIdentity.problem) return { problem: deps.pnpmIdentity.problem };
    deps.pnpm = deps.pnpmIdentity.version;
    const pinned = packageManagerPin(dir);
    if (pinned && pinned !== deps.pnpm) return { problem: `the exported tree pins pnpm@${pinned} (package.json packageManager) but the pnpm on PATH is ${deps.pnpm} (${deps.pnpmIdentity.path}); the dependency closure would not be the tree's` };
    const installed = spawn(deps.pnpmIdentity.path, INSTALL, { encoding: "utf8", windowsHide: true, cwd: dir, env, maxBuffer: 64 * 1024 * 1024, timeout: 10 * 60_000 });
    if (installed.error || installed.status !== 0) return { problem: `installing the exported tree's dependencies offline from the lockfile failed (the store must already hold them; run pnpm install in the checkout first): ${tail(installed.stderr || installed.stdout || installed.error?.message)}` };
    // Every node_modules the install produced (the workspace root's and each package's), by bytes. pnpm's `.modules.yaml`
    // carries a prune timestamp, so it is digested separately as installed-state metadata and left out of the byte digest.
    const digest = hashInstalled(dir);
    deps.installed = digest.installed; deps.store = digest.sha256; deps.files = digest.files; deps.hardlinks = digest.hardlinks; deps.externalLinks = digest.externalLinks;
  }
  return { dir, deps };
}

/** The environment the dependency install runs with: an allowlist, never the caller's `npm_config_*`, `PNPM_*` or NODE_OPTIONS. */
export function installEnvironment(from = process.env) {
  const keep = ["PATH", "HOME", "USERPROFILE", "TMPDIR", "TEMP", "TMP", "SYSTEMROOT", "COMSPEC", "PATHEXT", "LANG", "LC_ALL", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "PNPM_HOME", "COREPACK_HOME"];
  const env = {};
  for (const k of keep) if (from[k] !== undefined) env[k] = from[k];
  Object.assign(env, { CI: "1", NODE_OPTIONS: "", NODE_ENV: "production", npm_config_ignore_scripts: "true", npm_config_ignore_pnpmfile: "true", npm_config_frozen_lockfile: "true", npm_config_offline: "true", npm_config_package_import_method: "copy", npm_config_side_effects_cache: "false", npm_config_manage_package_manager_versions: "false" });
  return env;
}

/** The pnpm that will run: its resolved executable (real path), the sha256 of that file and its version — an identity, not a string. */
export function pnpmIdentity({ spawn = spawnSync, env = installEnvironment() } = {}) {
  const found = spawn(process.platform === "win32" ? "where" : "which", ["pnpm"], { encoding: "utf8", windowsHide: true, env });
  const onPath = String(found.stdout ?? "").split(/\r?\n/).find((l) => l.trim()) ?? "";
  if (found.status !== 0 || !onPath) return { problem: "pnpm is not on PATH" };
  let real; try { real = fs.realpathSync(onPath.trim()); } catch { return { problem: `pnpm at ${onPath} cannot be resolved` }; }
  const sha256 = crypto.createHash("sha256").update(fs.readFileSync(real)).digest("hex");
  // pnpm's `manage-package-manager-versions` silently execs ANOTHER pnpm (the tree's `packageManager` pin, fetched into
  // `<pnpm home>/.tools`), so the file on PATH would not be the pnpm that installs. The install environment disables that, and the
  // version the runtime answers is checked against the package.json beside the resolved file: a mismatch means a different
  // executable answered, and the identity is refused rather than recorded against the wrong bytes.
  // `pnpm --version` loads the configuration of the directory it runs in, and loading configuration EXECUTES that tree's
  // `.pnpmfile.cjs` (its `updateConfig` hook) even under `--ignore-pnpmfile` (pnpm 10.28 and 11.25, verified): asked inside the
  // export it would run candidate code before class A. It is therefore asked in a fresh, empty directory holding no tree at all.
  let empty; try { empty = fs.mkdtempSync(path.join(os.tmpdir(), "rk-pnpm-id-")); } catch { return { problem: "cannot create an empty directory to query pnpm in" }; }
  let version; try { version = spawn(onPath.trim(), ["--version"], { encoding: "utf8", windowsHide: true, cwd: empty, env }); } finally { fs.rmSync(empty, { recursive: true, force: true }); }
  if (version.status !== 0) return { problem: `pnpm at ${real} does not report a version` };
  const answered = String(version.stdout).trim();
  const declared = pnpmPackageVersion(real);
  if (!declared) return { problem: `pnpm at ${real} has no package.json naming its version; its identity cannot be bound` };
  if (declared !== answered) return { problem: `pnpm at ${real} is version ${declared} but "pnpm --version" answered ${answered}: another pnpm executable would run (package manager version management); refusing to bind the identity` };
  return { path: onPath.trim(), realPath: real, sha256, version: answered, node: process.version };
}

/** The version named by the package.json of the pnpm package that owns `file` (walking up at most four directories), or null. */
export function pnpmPackageVersion(file) {
  let dir = path.dirname(file);
  for (let i = 0; i < 4; i++) {
    try { const j = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8")); if (j.name === "pnpm" && typeof j.version === "string") return j.version; } catch {}
    const up = path.dirname(dir); if (up === dir) break; dir = up;
  }
  return null;
}

/** The pnpm version a tree pins through package.json `packageManager` (`pnpm@11.25.0` → `11.25.0`), or null. */
export function packageManagerPin(dir) {
  try { const m = /^pnpm@(\d+\.\d+\.\d+(?:[-+][\w.-]+)?)/.exec(JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8")).packageManager ?? ""); return m ? m[1] : null; } catch { return null; }
}

/** pnpm state metadata with its timestamp fields and the export's own path removed; every other field stays. */
export function normalizeInstalledMetadata(name, text, exportDir) {
  const real = (() => { try { return fs.realpathSync(exportDir); } catch { return exportDir; } })();
  const unpath = (t) => t.split(real).join("<export>").split(exportDir).join("<export>");
  if (name === ".modules.yaml") {
    // pnpm ≤10 writes YAML; pnpm 11 writes JSON under the same name. Either way only the TOP-LEVEL prunedAt goes; a nested key of
    // the same name is semantic and stays.
    if (/^\s*\{/.test(text)) { try { const j = JSON.parse(text); delete j.prunedAt; return unpath(JSON.stringify(j, null, 1)); } catch { return unpath(text); } }
    return unpath(text.split(/\r?\n/).filter((l) => !/^prunedAt:/.test(l)).join("\n"));
  }
  if (name === ".pnpm-workspace-state-v1.json") {
    try { const j = JSON.parse(text); delete j.lastValidatedTimestamp; return unpath(JSON.stringify(j, null, 1)); } catch { return unpath(text); }
  }
  return unpath(text);
}

/** Digest of every node_modules directory under an exported tree (root and per package). */
export function hashInstalled(dir) {
  const roots = [];
  const find = (d, depth) => { let entries = []; try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { return; } for (const e of entries) { if (!e.isDirectory() || e.isSymbolicLink()) continue; if (e.name === "node_modules") roots.push(path.join(d, e.name)); else if (depth < 4 && e.name !== ".git") find(path.join(d, e.name), depth + 1); } };
  find(dir, 0);
  const h = crypto.createHash("sha256"); const meta = crypto.createHash("sha256"); let files = 0; let hardlinks = 0; let externalLinks = 0; let metaFiles = 0;
  for (const nm of roots.sort()) {
    const rel = path.relative(dir, nm);
    // pnpm's state files are part of the closure (they decide layout and what counts as installed), with only their demonstrated
    // non-semantic fields normalized: `prunedAt` and `lastValidatedTimestamp` (timestamps) and the export's own temp path (keys of
    // the workspace state). Everything else in them, and every other byte, is digested as is.
    const META = [".modules.yaml", ".pnpm-workspace-state-v1.json"];
    const d = hashTree(nm, { root: dir, skip: (r) => META.includes(r) });
    h.update(`T ${rel}\0${d.sha256}\0`); files += d.files; hardlinks += d.hardlinks; externalLinks += d.externalLinks;
    for (const m of META) {
      let text; try { text = fs.readFileSync(path.join(nm, m), "utf8"); } catch { continue; }
      const normalized = normalizeInstalledMetadata(m, text, dir);
      h.update(`M ${rel}/${m}\0${normalized}\0`); meta.update(`M ${rel}/${m}\0${normalized}\0`); files++; metaFiles++;
    }
  }
  return { sha256: h.digest("hex"), files, hardlinks, externalLinks, installed: metaFiles ? meta.digest("hex") : null, roots: roots.length };
}

/**
 * sha256 over every regular file under `dir` (sorted relative path + bytes; symlinks by their target text), counting files whose
 * bytes are shared with somewhere else (hardlinks, nlink > 1) and symlinks that leave `root` (mutable external links).
 */
export function hashTree(dir, { root = dir, skip = () => false } = {}) {
  const h = crypto.createHash("sha256"); let files = 0; let hardlinks = 0; let externalLinks = 0;
  const realRoot = fs.realpathSync(root);
  const resolved = new Set(); // link targets already digested (cycle guard; a target reached twice is digested once)
  const walk = (d, rel) => {
    let entries = []; try { entries = fs.readdirSync(d, { withFileTypes: true }); } catch { return; }
    for (const e of entries.sort((a, b) => (a.name < b.name ? -1 : 1))) {
      const p = path.join(d, e.name); const r = `${rel}${e.name}`;
      if (skip(r)) continue;
      if (e.isSymbolicLink()) {
        const target = fs.readlinkSync(p); h.update(`L ${r}\0${target}\0`); files++;
        let real = null; try { real = fs.realpathSync(p); } catch { real = null; }
        if (real === null || !(real === realRoot || real.startsWith(`${realRoot}${path.sep}`))) { externalLinks++; continue; }
        // The resolved closure: a link inside the export (a workspace package) is bound by its target's bytes too, not only by its text.
        if (resolved.has(real)) { h.update(`R ${r}\0${path.relative(realRoot, real)}\0seen\0`); continue; }
        resolved.add(real);
        h.update(`R ${r}\0${path.relative(realRoot, real)}\0`);
        const st = fs.statSync(real);
        if (st.isDirectory()) walk(real, `${r}/`); else { h.update(fs.readFileSync(real)); h.update("\0"); files++; }
      }
      else if (e.isDirectory()) walk(p, `${r}/`);
      else if (e.isFile()) { h.update(`F ${r}\0`); h.update(fs.readFileSync(p)); h.update("\0"); files++; if (fs.statSync(p).nlink > 1) hardlinks++; }
    }
  };
  walk(dir, "");
  return { sha256: h.digest("hex"), files, hardlinks, externalLinks };
}
const tail = (t, n = 400) => String(t ?? "").trim().split("\n").slice(-6).join(" | ").slice(-n);

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
    const exported = exporter(root, { commit: before.commit, spawn });
    if (exported.problem) { io.error(`exports-import-effects: ${exported.problem}`); return 2; }
    probeRoot = exported.dir; closure = { kind: "exported-tree", exported: exported.dir, deps: exported.deps };
    // Nothing writes the export after its digest: best-effort read-only bits now, and a second digest after the probe below.
    try { spawn("chmod", ["-R", "a-w", exported.dir], { encoding: "utf8", windowsHide: true }); } catch { /* platform without chmod: the re-digest below still binds it */ }
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
  } finally {
    if (closure.kind === "exported-tree") {
      // The installed bytes must be exactly what was digested before the imports ran; a mutation in between is a different closure.
      try { const again = hashInstalled(closure.exported); closure.deps.closureStable = again.sha256 === closure.deps.store && again.hardlinks === 0 && again.externalLinks === 0; } catch { closure.deps.closureStable = false; }
      try { spawn("chmod", ["-R", "u+w", closure.exported], { encoding: "utf8", windowsHide: true }); } catch { /* best effort */ }
      fs.rmSync(closure.exported, { recursive: true, force: true });
    }
  }
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
  const d = report.closure.deps ?? {};
  const hex64 = (v) => /^[0-9a-f]{64}$/.test(v ?? "");
  if (!hex64(d.lock) || !hex64(d.store) || typeof d.pnpm !== "string" || !Number.isInteger(d.files) || d.files < 0 || !(d.installed === null || hex64(d.installed))) return bad("the dependency closure (lockfile digest, pnpm version, installed-state and installed-bytes digests) is not recorded");
  if (d.hardlinks !== 0 || d.externalLinks !== 0) return bad(`the installed dependencies are not private copies (${d.hardlinks ?? "?"} hardlinked file(s), ${d.externalLinks ?? "?"} link(s) leaving the export); mutable external bytes are refused`);
  if (d.install !== INSTALL_COMMAND) return bad("the dependencies were not installed frozen, offline, without scripts or pnpm hooks, as private copies");
  if (d.files > 0 && !(typeof d.pnpmIdentity?.realPath === "string" && hex64(d.pnpmIdentity?.sha256) && typeof d.pnpmIdentity?.version === "string" && d.pnpmIdentity.version === d.pnpm)) return bad("the pnpm that installed the dependencies is not identified (real path, sha256, version)");
  if (d.files > 0 && d.closureStable !== true) return bad("the installed bytes changed between their digest and the end of the probe (or were not re-digested); the closure is not immutable");
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
