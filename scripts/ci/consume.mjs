// Consumer verification of the SHIPPED bytes, run by `publish.mjs verify` on each verify host. The gate
// tests workspace source; this proves the sealed artifacts themselves:
//   npm     install the sealed tarballs of the request TOGETHER (plus their already-published
//           dependencies, from the registry) into a scratch project with `npm install --ignore-scripts`,
//           then prove from the INSTALLED copy that every declared main/module/types/bin/exports target
//           exists, runtime exports resolve & import under their declared conditions, test-only exports
//           reject production resolution, and every
//           declared bin runs its declared safe command (rightkit.publishCheck.binSmoke) with the
//           expected exit code.
//   crates  run each committed PK example against exact sealed bytes through an offline directory
//           registry, then require locked build & runtime success. Missing recipes fail closed.
// Nothing here publishes or holds a token; the caller has already refused any.
import { closeIncomplete, expectedIncomplete } from "./exports-import-effects.mjs";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { runRegistryConsumer } from "./registry-consumer.mjs";
import { fileURLToPath } from "node:url";
import { runNpmConsumer } from "./npm-consumer.mjs";
import { runNpmRecipeConsumer, hasReviewedNpmEntry, runtimeEnvNames, validateRecipeEntry, validatePackedClosure, validateImportClosures } from "./npm-recipe-consumer.mjs";
import { childEnv, tarEntry } from "./upload.mjs";
const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const tail = (text, n = 600) => String(text ?? "").trim().slice(-n);

/** How to run npm without a shell: node + npm-cli.js beside the running node, else the npm shim. */
export function npmCliCommand({ execPath = process.execPath, platform = process.platform, exists = fs.existsSync } = {}) {
  const candidates = [
    path.join(path.dirname(execPath), "node_modules", "npm", "bin", "npm-cli.js"),
    path.join(path.dirname(execPath), "..", "lib", "node_modules", "npm", "bin", "npm-cli.js"),
  ];
  const cli = candidates.find((c) => exists(c));
  if (cli) return { command: execPath, prefix: [cli], shell: false };
  return { command: platform === "win32" ? "npm.cmd" : "npm", prefix: [], shell: platform === "win32" };
}

// ---- npm --------------------------------------------------------------------------------------

/**
 * What a package.json promises. Returns { subpaths: [{ subpath, spec, runtime: [files], types: [files], testConditions }],
 * patterns: [subpath], files: [every non-pattern file the manifest points at], bins, binMap: { binName: file }, hasEntry }.
 */
export function declaredEntries(pkg) {
  const rel = (v) => (typeof v === "string" && v.startsWith("./") ? v.slice(2) : typeof v === "string" && v && !v.startsWith("/") ? v : "");
  const files = new Set(); const bins = []; const binMap = {};
  const addFile = (v) => { const r = rel(v); if (r && !r.includes("*")) files.add(r); };
  for (const key of ["main", "module", "types", "typings"]) addFile(pkg[key]);
  if (typeof pkg.bin === "string") { addFile(pkg.bin); bins.push(rel(pkg.bin)); binMap[String(pkg.name ?? "").replace(/^@[^/]+\//, "")] = rel(pkg.bin); }
  else if (pkg.bin && typeof pkg.bin === "object") for (const [name, v] of Object.entries(pkg.bin)) { addFile(v); bins.push(rel(v)); binMap[name] = rel(v); }

  const collect = (node, underTypes, out, conditions = []) => {
    if (typeof node === "string") {
      const r = rel(node);
      if (r) {
        (underTypes ? out.types : out.runtime).push(r);
        if (!underTypes) out.runtimeConditions.push(conditions);
      }
    }
    else if (Array.isArray(node)) node.forEach((n) => collect(n, underTypes, out, conditions));
    else if (node && typeof node === "object") for (const [k, v] of Object.entries(node)) collect(v, underTypes || k === "types", out, [...conditions, k]);
  };
  const subpaths = []; const patterns = [];
  const exp = pkg.exports;
  if (exp !== undefined && exp !== null) {
    const map = typeof exp === "object" && !Array.isArray(exp) && Object.keys(exp).some((k) => k.startsWith(".")) ? exp : { ".": exp };
    for (const [subpath, target] of Object.entries(map)) {
      if (subpath.includes("*")) { patterns.push(subpath); continue; }
      const out = { runtime: [], types: [], runtimeConditions: [] };
      collect(target, false, out);
      for (const f of [...out.runtime, ...out.types]) if (!f.includes("*")) files.add(f);
      const testConditions = [...new Set(out.runtimeConditions.flat().filter((c) => c === "development" || c === "test"))];
      // Test-only subpaths retain their production-denial requirement even if a default branch leaks.
      const testOnly = /^\.\/testing(?:\/|$)/.test(subpath) || (out.runtimeConditions.length > 0 && out.runtimeConditions.every((cs) => cs.some((c) => c === "development" || c === "test")));
      subpaths.push({ subpath, spec: subpath === "." ? pkg.name : `${pkg.name}/${subpath.slice(2)}`, runtime: out.runtime, types: out.types, testConditions: testOnly ? testConditions : null });
    }
  } else if (pkg.main) subpaths.push({ subpath: ".", spec: pkg.name, runtime: [rel(pkg.main)], types: [] });
  return { subpaths, patterns, files: [...files], bins, binMap, hasEntry: subpaths.length > 0 || files.size > 0 };
}

/** `rightkit.publishCheck.skipImport`: subpaths a package declares cannot be imported in plain Node (DOM or Tauri host only). */
export function skipImportList(pkg, declared) {
  const raw = pkg?.rightkit?.publishCheck?.skipImport;
  if (raw === undefined) return { list: [], problems: [] };
  if (!Array.isArray(raw) || raw.some((v) => typeof v !== "string")) return { list: [], problems: [`${pkg.name}: rightkit.publishCheck.skipImport must be an array of exports subpaths`] };
  const known = new Set(declared.subpaths.map((s) => s.subpath));
  const stray = raw.filter((v) => !known.has(v));
  return { list: raw, problems: stray.length ? [`${pkg.name}: rightkit.publishCheck.skipImport names ${stray.join(", ")}, which package.json does not export`] : [] };
}

/**
 * `rightkit.publishCheck.binSmoke`: one safe, read-only command per declared bin, run from the scratch install.
 * Each value is an argument array (exit 0 expected) or { args, exitCode?, output? } for a CLI whose only safe
 * invocation exits non-zero (a usage message) and must print `output`. Every bin must be declared.
 */
export function binSmokePlan(pkg, declared) {
  const names = Object.keys(declared.binMap ?? {});
  if (names.length === 0) return { checks: [], problems: [] };
  const raw = pkg?.rightkit?.publishCheck?.binSmoke;
  if (raw === undefined || raw === null || typeof raw !== "object" || Array.isArray(raw)) return { checks: [], problems: [`${pkg.name}: declares bin ${names.join(", ")} but no rightkit.publishCheck.binSmoke object naming a safe command for each`] };
  const problems = []; const checks = [];
  for (const name of names) {
    const spec = raw[name];
    const args = Array.isArray(spec) ? spec : spec?.args;
    if (!Array.isArray(args) || args.some((a) => typeof a !== "string")) { problems.push(`${pkg.name}: rightkit.publishCheck.binSmoke has no argument array for bin ${name}`); continue; }
    const exitCode = Array.isArray(spec) ? 0 : (spec.exitCode ?? 0);
    const output = Array.isArray(spec) ? undefined : spec.output;
    if (!Number.isInteger(exitCode) || exitCode < 0 || exitCode > 255) { problems.push(`${pkg.name}: binSmoke ${name} exitCode must be an integer 0-255`); continue; }
    if (output !== undefined && (typeof output !== "string" || !output)) { problems.push(`${pkg.name}: binSmoke ${name} output must be a non-empty string`); continue; }
    checks.push({ bin: name, file: declared.binMap[name], args, exitCode, output });
  }
  const stray = Object.keys(raw).filter((k) => !names.includes(k));
  if (stray.length) problems.push(`${pkg.name}: rightkit.publishCheck.binSmoke names ${stray.join(", ")}, which package.json does not declare as a bin`);
  return { checks, problems };
}

/** Run each planned bin from the installed package with node (no shell, no stdin). Returns { report, problems }. */
function runBinSmoke({ unit, dir, checks, spawn, node, cwd, env }) {
  const report = []; const problems = [];
  for (const check of checks) {
    if (!/\.(?:[cm]?js)$/.test(check.file)) { problems.push(`${unit.name}: bin ${check.bin} (${check.file}) is not a Node script; the smoke check cannot run it`); continue; }
    const ran = spawn(node, [path.join(dir, check.file), ...check.args], { cwd, encoding: "utf8", env, input: "", maxBuffer: 8 * 1024 * 1024, timeout: 60_000 });
    const text = `${ran.stdout ?? ""}${ran.stderr ?? ""}`;
    report.push({ bin: check.bin, args: check.args, status: ran.status ?? null });
    if (ran.error || ran.status !== check.exitCode) problems.push(`${unit.name}: installed bin ${check.bin} ${check.args.join(" ")} exited ${ran.status ?? "without a status"} (expected ${check.exitCode}): ${tail(text || ran.error?.message, 300)}`);
    else if (check.output !== undefined && !text.includes(check.output)) problems.push(`${unit.name}: installed bin ${check.bin} ${check.args.join(" ")} did not print ${JSON.stringify(check.output)}: ${tail(text, 300)}`);
  }
  return { report, problems };
}

const CHECK_SCRIPT = `import fs from "node:fs";
const plan = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const out = [];
for (const item of plan.items) {
  const row = { spec: item.spec, ok: false, imported: false };
  if (item.expectBlocked) {
    try {
      row.resolved = import.meta.resolve(item.spec);
      await import(item.spec);
      row.error = "test-only export imported under production conditions";
    }
    catch (error) {
      if (!row.resolved && error.code === "ERR_PACKAGE_PATH_NOT_EXPORTED") { row.ok = true; row.rejected = true; }
      else row.error = row.resolved ? "test-only export resolves under production conditions" : String(error && error.stack ? error.stack : error).slice(0, 500);
    }
    out.push(row);
    continue;
  }
  try {
    const url = import.meta.resolve(item.spec);
    row.resolved = url;
    if (item.import && /\\.(?:[cm]?js)$/.test(new URL(url).pathname)) { await import(item.spec); row.imported = true; }
    row.ok = true;
  } catch (error) { row.error = String(error && error.stack ? error.stack : error).split("\\n").slice(0, 4).join(" | ").slice(0, 500); }
  out.push(row);
}
process.stdout.write("RK_CONSUME " + JSON.stringify(out) + "\\n");
`;

/** Read installed peers, rather than treating omitted CLI pins as proof of absence. */
export function installedImportClosure({ unit, pkg, dir, scratch, installPins }) {
  const problems = []; const packages = []; const absentPeers = [];
  for (const [name, version] of Object.entries(installPins)) {
    try {
      const installed = JSON.parse(fs.readFileSync(path.join(scratch, "node_modules", ...name.split("/"), "package.json"), "utf8"));
      if (installed.name !== name || installed.version !== version) throw new Error(`installed ${installed.name}@${installed.version}`);
      packages.push({ name, version: installed.version });
    } catch (error) { problems.push(`${unit.name}: import closure pin ${name}@${version} differs from installed copy: ${error.message}`); }
  }
  for (const name of Object.keys(pkg.peerDependencies ?? {}).filter(name => !Object.hasOwn(installPins, name))) {
    let present = false;
    // Include every ancestor Node could search, so an ambient parent install cannot mask absence.
    for (let current = dir; ; current = path.dirname(current)) {
      if (fs.existsSync(path.join(current, "node_modules", ...name.split("/")))) { present = true; break; }
      if (path.dirname(current) === current) break;
    }
    if (present) problems.push(`${unit.name}: unselected peer ${name} is installed in isolated import closure`);
    else absentPeers.push(name);
  }
  return { problems, evidence: { archive: { name: unit.name, version: unit.version, file: unit.file, sha256: unit.sha256 }, packages, absentPeers } };
}

/**
 * Install the sealed tarballs together into a scratch project and check each from its installed copy.
 * Returns Map(unit name -> { problems: [], report }). Never throws for a package defect.
 */
function consumeNpmInstallation({ units, packDir, scratchRoot = os.tmpdir(), spawn = spawnSync, env = process.env, npm = npmCliCommand(), node = process.execPath, installPins, onlySubpaths, requireAllImports = false, incomplete = null }) {
  const results = new Map(units.map((u) => [u.name, { problems: [], report: {} }]));
  const scratch = fs.mkdtempSync(path.join(scratchRoot, "rk-consume-npm-"));
  try {
    fs.writeFileSync(path.join(scratch, "package.json"), `${JSON.stringify({ name: "rk-consumer", version: "0.0.0", private: true, type: "module" })}\n`);
    const userconfig = path.join(scratch, "npmrc"); fs.writeFileSync(userconfig, "");
    const childEnv = { ...env, npm_config_cache: path.join(scratch, "cache"), npm_config_userconfig: userconfig, npm_config_update_notifier: "false", npm_config_fund: "false", npm_config_audit: "false" };
    const install = spawn(npm.command, [...npm.prefix, "install", "--ignore-scripts", "--no-audit", "--no-fund", "--no-package-lock", ...(installPins ? ["--legacy-peer-deps"] : []), ...units.map((u) => path.resolve(packDir, u.file)), ...Object.entries(installPins ?? {}).map(([name, version]) => `${name}@${version}`)], { cwd: scratch, encoding: "utf8", env: childEnv, shell: npm.shell, maxBuffer: 64 * 1024 * 1024, timeout: 20 * 60_000 });
    if (install.error || install.status !== 0) {
      const why = `npm install --ignore-scripts of the sealed tarballs failed: ${tail(install.stderr || install.error?.message)}`;
      for (const r of results.values()) r.problems.push(why);
      return results;
    }
    for (const unit of units) {
      const out = results.get(unit.name);
      const dir = path.join(scratch, "node_modules", ...unit.name.split("/"));
      let pkg;
      try { pkg = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8")); } catch { out.problems.push(`${unit.name}: not installed from the sealed tarball (no node_modules/${unit.name}/package.json)`); continue; }
      if (pkg.name !== unit.name || pkg.version !== unit.version) { out.problems.push(`${unit.name}: installed ${pkg.name}@${pkg.version}, sealed ${unit.version}`); continue; }
      const closure = installPins ? installedImportClosure({ unit, pkg, dir, scratch, installPins }) : null;
      if (closure) out.problems.push(...closure.problems);
      const declared = declaredEntries(pkg);
      const skip = skipImportList(pkg, declared);
      out.problems.push(...skip.problems);
      const skippedImport = requireAllImports ? [] : skip.list;
      for (const file of declared.files) if (!fs.existsSync(path.join(dir, file))) out.problems.push(`${unit.name}: installed package.json points at ${file}, which the installed package does not contain`);
      const smokePlan = binSmokePlan(pkg, declared);
      out.problems.push(...smokePlan.problems);
      const smoke = runBinSmoke({ unit, dir, checks: smokePlan.checks, spawn, node, cwd: scratch, env: childEnv });
      out.problems.push(...smoke.problems);
      // A bin target is a program: it reads its command line at import, so the smoke check above runs it and the import
      // check only resolves it. (@rightkit/release 0.6.6 exported cli/right-release.mjs; importing it ran the CLI with the
      // plan file as its command.) This holds under reviewed closures too, which otherwise import every runtime export.
      const binTargets = new Set(declared.bins);
      const isBinTarget = (s) => s.runtime.length > 0 && s.runtime.every((file) => binTargets.has(file));
      const binTargetSubpaths = declared.subpaths.filter(isBinTarget).map((s) => s.subpath);
      const items = declared.subpaths
        .filter((s) => s.runtime.length > 0 && (!onlySubpaths || onlySubpaths.includes(s.subpath)))
        .map((s) => ({ spec: s.spec, subpath: s.subpath, import: !skippedImport.includes(s.subpath) && !isBinTarget(s), ...(s.testConditions !== null && s.testConditions !== undefined ? { expectBlocked: true, testConditions: s.testConditions } : {}) }));
      const report = { entries: items.length, imported: [], resolvedOnly: [], productionRejected: [], conditionalImports: [], skippedImport, binTargets: binTargetSubpaths, patterns: declared.patterns, bins: declared.bins, binSmoke: smoke.report, filesChecked: declared.files.length, noJsEntry: items.length === 0 };
      out.report = report;
      if (closure) report.installClosure = closure.evidence;
      if (items.length === 0) continue;
      const scriptFile = path.join(scratch, `check-${unit.name.replace(/[^A-Za-z0-9]+/g, "_")}.mjs`);
      const planFile = `${scriptFile}.json`;
      fs.writeFileSync(scriptFile, CHECK_SCRIPT);
      const checks = [{ conditions: [], items }];
      for (const item of items.filter((i) => i.expectBlocked)) {
        if (item.testConditions.length === 0) out.problems.push(`${unit.name}: ${item.spec} test-only export has no development or test runtime condition`);
        for (const condition of item.testConditions) {
          let check = checks.find((c) => c.conditions[0] === condition);
          if (!check) { check = { conditions: [condition], items: [] }; checks.push(check); }
          check.items.push({ spec: item.spec, subpath: item.subpath, import: true });
        }
      }
      for (const check of checks) {
        fs.writeFileSync(planFile, JSON.stringify({ items: check.items }));
        // Never let inherited --conditions/preloads turn the production-denial probe into a dev probe.
        const checkEnv = { ...childEnv, NODE_OPTIONS: "", NODE_ENV: check.conditions.length ? "test" : "production" };
        const ran = spawn(node, [...check.conditions.map((c) => `--conditions=${c}`), scriptFile, planFile], { cwd: scratch, encoding: "utf8", env: checkEnv, windowsHide: true, maxBuffer: 64 * 1024 * 1024, timeout: 5 * 60_000 });
        const line = String(ran.stdout ?? "").split(/\r?\n/).find((l) => l.startsWith("RK_CONSUME "));
        if (ran.error || ran.status !== 0) { out.problems.push(`${unit.name}: import check failed (status ${ran.status}): ${tail(ran.stderr || ran.error?.message)}`); continue; }
        if (!line) { out.problems.push(`${unit.name}: the import check produced no result (status ${ran.status}): ${tail(ran.stderr || ran.error?.message)}`); continue; }
        let rows;
        try { rows = JSON.parse(line.slice("RK_CONSUME ".length)); } catch { out.problems.push(`${unit.name}: unreadable import check result`); continue; }
        for (const row of rows) {
          if (!row.ok) out.problems.push(`${unit.name}: ${row.spec} ${row.imported ? "imported" : "did not resolve or import"}: ${row.error ?? "unknown error"}`);
          else if (check.conditions.length && !row.imported) out.problems.push(`${unit.name}: ${row.spec} did not import under ${check.conditions.join(",")} conditions`);
          else if (row.rejected) report.productionRejected.push(row.spec);
          else if (row.imported && check.conditions.length) report.conditionalImports.push({ spec: row.spec, conditions: check.conditions });
          else if (row.imported) report.imported.push(row.spec);
          else report.resolvedOnly.push(row.spec);
        }
        if (rows.length !== check.items.length) out.problems.push(`${unit.name}: import check covered ${rows.length} of ${check.items.length} entries`);
      }
      // The source probe (scripts/ci/exports-import-effects.mjs) could not import build output; it handed those exports here as
      // INCOMPLETE. Each one must have been imported from the sealed tarball above, or this unit is not verified.
      if (incomplete) {
        // A reviewed closure group imports only its subpaths; it closes only the INCOMPLETE rows it covers.
        const scoped = { ...incomplete, incomplete: incomplete.incomplete.filter((r) => r.package === unit.name && (!onlySubpaths || onlySubpaths.includes(r.key))) };
        const imported = [...report.imported, ...report.conditionalImports.map((c) => c.spec)];
        const closure = closeIncomplete({ report: scoped, name: unit.name, importedSpecs: imported, expected: onlySubpaths ? expectedIncomplete(pkg).filter((e) => onlySubpaths.includes(e.key)) : expectedIncomplete(pkg) });
        out.problems.push(...closure);
        report.incompleteClosed = scoped.incomplete.length - closure.length;
      }
    }
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
  return results;
}

/** Partition runtime exports by identical reviewed maps; every export has one owner. */
export function importClosureGroups(entry, name, declared) {
  const runtime = declared.subpaths.filter(item => item.runtime.length > 0).map(item => item.subpath);
  const maps = validateImportClosures(entry, name, runtime);
  const groups = new Map();
  for (const subpath of runtime) {
    const install = maps[subpath];
    const key = JSON.stringify(install);
    if (!groups.has(key)) groups.set(key, { install, subpaths: [] });
    groups.get(key).subpaths.push(subpath);
  }
  return groups.size ? [...groups.values()] : [{ install: {}, subpaths: [] }];
}

/** Existing shared install remains unchanged for units without reviewed per-entry closures. */
export function consumeNpmPortability(options) {
  const { units, packDir, root = REPO_ROOT, spawn = spawnSync } = options;
  const results = new Map();
  let inventory;
  try { inventory = JSON.parse(fs.readFileSync(path.join(root, "ci/consumer-recipes.json"), "utf8")); }
  catch (error) {
    for (const unit of units) results.set(unit.name, { problems: [`${unit.name}: unreadable reviewed import inventory: ${error.message}`], report: {} });
    return results;
  }
  const shared = units.filter(unit => inventory.npm?.[unit.name]?.importClosures === undefined);
  if (shared.length) for (const [name, result] of consumeNpmInstallation({ ...options, units: shared })) results.set(name, result);
  for (const unit of units.filter(unit => !shared.includes(unit))) {
    const out = { problems: [], report: {} };
    results.set(unit.name, out);
    try {
      const entry = validateRecipeEntry(inventory.npm[unit.name], unit.name);
      const packed = JSON.parse(tarEntry(path.resolve(packDir, unit.file), "package/package.json", spawn).toString("utf8"));
      validatePackedClosure(packed, entry, unit.name, unit.version);
      const declared = declaredEntries(packed);
      const groups = importClosureGroups(entry, unit.name, declared);
      const report = { entries: 0, imported: [], resolvedOnly: [], productionRejected: [], conditionalImports: [], importClosures: [] };
      out.report = report;
      for (const group of groups) {
        // Only this sealed unit & this exact map enter a fresh install. npm must not auto-install
        // other optional/required peers, which would mask a minimal export's dependency contract.
        // Reviewed closures prove actual imports even when sealed legacy metadata requests a skip.
        const result = consumeNpmInstallation({ ...options, units: [unit], installPins: group.install, onlySubpaths: group.subpaths, requireAllImports: true }).get(unit.name);
        out.problems.push(...result.problems);
        const row = result.report;
        report.entries += row.entries ?? 0;
        for (const field of ["imported", "resolvedOnly", "productionRejected", "conditionalImports"]) report[field].push(...(row[field] ?? []));
        for (const field of ["skippedImport", "binTargets", "patterns", "bins", "binSmoke", "filesChecked", "noJsEntry"]) if (row[field] !== undefined) report[field] = row[field];
        report.importClosures.push({ install: group.install, subpaths: group.subpaths, entries: row.entries ?? 0, installed: row.installClosure ?? null });
      }
      const expected = declared.subpaths.filter(item => item.runtime.length > 0).length;
      if (report.entries !== expected) out.problems.push(`${unit.name}: isolated import checks covered ${report.entries} of ${expected} runtime exports`);
    } catch (error) { out.problems.push(`${unit.name}: reviewed import closure failed: ${error.message}`); }
  }
  return results;
}

/**
 * Any other npm unit needs a reviewed `npm.<name>` entry in ci/consumer-recipes.json (same full-file-hash
 * inventory model as cargoRecipe). No entry is a refusal, never a silent skip; generic portability is not PK.
 */
function consumeReviewedNpm({ unit, out, options }) {
  const root = options.root ?? REPO_ROOT;
  let reviewed = false;
  try { reviewed = hasReviewedNpmEntry(JSON.parse(fs.readFileSync(path.join(root, "ci/consumer-recipes.json"), "utf8")), unit.name); } catch { reviewed = false; }
  if (!reviewed) { out.problems.push(`${unit.name}: missing reviewed PK example (ci/consumer-recipes.json npm.${unit.name}); generic portability is not PK qualification`); return; }
  const portability = out.report;
  const scratchRoot = options.scratchRoot ?? os.tmpdir();
  const reportDirectory = fs.mkdtempSync(path.join(scratchRoot, "rk-npm-recipe-report-"));
  try {
    // Only the reviewed runtime variables for this unit survive the allow-list; nothing else is forwarded.
    const source = options.env ?? process.env;
    const extra = Object.fromEntries(runtimeEnvNames(unit.name).filter((key) => source[key] !== undefined).map((key) => [key, source[key]]));
    const report = (options.npmRecipeConsumer ?? runNpmRecipeConsumer)({ packDir: options.packDir, name: unit.name, reportFile: path.join(reportDirectory, "report.json"), scratchRoot, spawn: options.spawn ?? spawnSync, env: childEnv(source, extra), ...(options.root ? { root: options.root } : {}) });
    out.report = { ...report, portability };
    if (out.problems.length || report?.verified !== true || report.qualification !== "R2a" || report.name !== unit.name || report.version !== unit.version || report.archiveSha256 !== unit.sha256 || report.archiveSize !== unit.size) {
      out.report.verified = false;
      out.problems.push(`${unit.name}: reviewed npm PK consumer or existing portability gates failed`);
    }
  } catch (error) { out.report = { ...(error.report ?? {}), qualification: "R2a", verified: false, portability }; out.problems.push(`${unit.name}: reviewed npm PK consumer failed: ${error.message}`); }
  finally { fs.rmSync(reportDirectory, { recursive: true, force: true }); }
}

/** Preserve every existing portability gate; named PK11 and reviewed npm entries receive R2a evidence. */
export function consumeNpm(options) {
  const results = consumeNpmPortability(options);
  for (const unit of options.units) {
    const out = results.get(unit.name);
    if (unit.name !== "@rightkit/logs") { consumeReviewedNpm({ unit, out, options }); continue; }
    const portability = out.report;
    const scratchRoot = options.scratchRoot ?? os.tmpdir();
    // A unique external report path is required by named runner & removed after readback.
    const reportDirectory = fs.mkdtempSync(path.join(scratchRoot, "rk-pk11-report-"));
    try {
      const report = (options.npmConsumer ?? runNpmConsumer)({ packDir: options.packDir, name: unit.name, reportFile: path.join(reportDirectory, "report.json"), scratchRoot, spawn: options.spawn ?? spawnSync, env: childEnv(options.env ?? process.env), ...(options.root ? { root: options.root } : {}) });
      out.report = { ...report, portability };
      if (out.problems.length || report?.verified !== true || report.qualification !== "R2a" || report.name !== unit.name || report.version !== unit.version || report.archiveSha256 !== unit.sha256 || report.archiveSize !== unit.size) {
        out.report.verified = false;
        out.problems.push(`${unit.name}: named PK11 consumer or existing portability gates failed`);
      }
    } catch (error) { out.report = { ...(error.report ?? {}), qualification: "R2a", verified: false, portability }; out.problems.push(`${unit.name}: named PK11 npm consumer failed: ${error.message}`); }
    finally { fs.rmSync(reportDirectory, { recursive: true, force: true }); }
  }
  return results;
}

// ---- crates -----------------------------------------------------------------------------------

/**
 * Qualify committed PK examples; generic source checks never establish consumer acceptance.
 * Existing explicit host exclusions retain their caller-controlled semantics.
 */
export function consumeCrates({ units, packDir, scratchRoot = os.tmpdir(), spawn = spawnSync, env = process.env, excluded = [], uploadMetadata, root, registryConsumer = runRegistryConsumer }) {
  const results = new Map(units.map((u) => [u.name, { problems: [], report: {} }]));
  for (const unit of units) {
    const out = results.get(unit.name);
    if (excluded.includes(unit.name)) { out.report = { checked: false, reason: "allowed host exclusion (allow_host_exclusion)" }; continue; }
    try {
      out.report = registryConsumer({ packDir, name: unit.name, scratchRoot, spawn, env, uploadMetadata, ...(root ? { root } : {}) });
      if (out.report?.verified !== true || out.report.qualification !== "R2a" || out.report.archiveSha256 !== unit.sha256 || out.report.name !== unit.name || out.report.version !== unit.version) throw new Error("exact-archive R2a PK example did not return verified identity");
    } catch (error) { out.report = error.report ?? { qualification: "R2a", verified: false }; out.problems.push(`${unit.name}: committed registry PK example failed: ${error.message}`); }
  }
  return results;
}
