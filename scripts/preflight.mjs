#!/usr/bin/env node
// Preflight: the cheap checks, before a push leaves the machine. Reads committed files only: no network, no cargo, no pnpm,
// no compile, no broker, no test run, no candidate generator. Seconds on any host, in any checkout, with or without
// node_modules. .githooks/pre-push runs it before the gate; ci.yml runs it as the plan job's first step, so a push that skipped
// the hook still stops within a minute instead of after native shards.
//
// Why (scan-gaps report, 2026-10-23): 555 of 1,455 hosted runs failed, and 144 of those failed on a rule that only reads
// files. The rules already existed, but as late refusals (rk-preflight, rk-npm-preflight, rightkit-dispatch, the hosted
// verify) for the one unit being published, minutes in. Preflight runs the same predicates first, for every unit.
//
// The rules that read any repository (eol-policy G04, file-url G12, retired-alias P11 and the app defects G13-G21), the export of
// the checked commit, the ratchet and the command line live in packages/release/app-check.mjs: @rightkit/release ships them as
// `rightkit-gate check` and `right-release check`, so an app runs the same check without a RightKit checkout. This file adds the
// rules that police RightKit itself, and is a thin caller of the rest (`--list` shows them in order):
//
//   consumer-recipe  G01  scripts/ci/consumer-recipes.mjs cargoRecipe: Cargo PK recipe inventory, exact hashes, exact version
//   npm-recipe       G01  scripts/ci/rk-npm-preflight.mjs npmRecipePolicy: npm PK recipe inventory, hashes, closure exception
//   npm-surface      G02  scripts/ci/npm-recipe-consumer.mjs validateRecipeEntry + validatePackedClosure on the source
//                         package.json: reviewed surface, importClosures and dependencies match the exports
//   version-pins     G03  scripts/ci/release-version-pins.mjs versionPinProblems: every hand-written copy of a unit's version
//   dispatch-pins    G05  the helper digests pinned in scripts/ci/rightkit-dispatch.mjs match the committed helpers
//   consumer-locks   G08  the root and ci/consumers/** Cargo.lock list each path crate at its crates/<name>/Cargo.toml version
//                         (cargo --locked refuses a stale one: tauri-shell consumer-0-0, wv2-0_39, 2026-10-23)
//   generated-docs   G10  scripts/generate-index.mjs --check: INDEX.md and docs/CAPABILITY-MAP.md match the committed sources, and the
//                         port matrix's MD and HTML are the rendering of its JSON (offline: reads manifests and the version feed only)
//
// Ratchet: ci/app-check-baseline.json (this checkout's) lists, per repository key (lowercased --repo basename; "rightkit" for
// this one), the identities `${rule}|${file}|${subject}` (no line numbers) of findings that exist today. A listed finding prints
// as KNOWN and does not fail; an unlisted one fails; a listed identity that is gone fails as STALE (the list only shrinks).
// An app that is checked from here and carries its own .rightkit/app-check-baseline.json uses that file instead (app-check.mjs).
// --print-baseline prints the current identities for a person to paste; preflight never writes the file.
//
// Not here yet: G07 (ignored tests) refuses rightkit-pdf and rightkit-credentials today, and its fix is a reviewed allowIgnored
// exemption, which preflight never authors. G06, G09, G11 and G13 need a parser.
//
// The rules come from this checkout's scripts; the content is the commit being checked. A FAIL line says what to change.
// Step 1 of docs/PIPELINE-ARCHITECTURE.md; invariants P1, P2, P3 and P9 there name the tests that hold it to this.
//
// Usage: node scripts/preflight.mjs [--repo <dir>] [--commit <rev> | --worktree] [--only <rule,...>] [--list] [--print-baseline]
//                                   [--baseline <file>]
//   --repo <dir>    the repository to check (default: this checkout)
//   --commit <rev>  the commit to check (default HEAD), exported with git ls-tree and cat-file: an uncommitted fix is not what is pushed.
//   --worktree      check what a compile would see instead: tracked and untracked, non-ignored files as they are on disk
//                   (written to a throwaway index, never the repository's own)
//   --print-baseline  print the JSON array of this repository's current finding identities and exit 0
//   --baseline <file> the ratchet file (default: the app's .rightkit/app-check-baseline.json, else ci/app-check-baseline.json of this checkout)
// Exit 0: every rule passed. 1: at least one FAIL. 2: preflight itself could not run.
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { RULES as APP_RULES, main as appCheckMain, readJson, readText, runRules as runAppRules } from "../packages/release/app-check.mjs";
import { cargoRecipe } from "./ci/consumer-recipes.mjs";
import { validatePackedClosure, validateRecipeEntry } from "./ci/npm-recipe-consumer.mjs";
import { npmRecipePolicy } from "./ci/rk-npm-preflight.mjs";
import { consumerRecipePolicy } from "./ci/rk-preflight.mjs";
import { versionPinProblems } from "./ci/release-version-pins.mjs";

// What scripts that call preflight, and its tests, take from it (scripts/retire-rightkit-alias.mjs).
export { RETIRED_ALIAS_EXEMPT, applyBaseline, baselineKnown, cargoTestTargets, isGeneratedFile, retireAliasText } from "../packages/release/app-check.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const listDirs = (root, rel) => { try { return fs.readdirSync(path.join(root, rel), { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => e.name).sort(); } catch { return []; } };

/** `[package]` name, version and publish flag of a Cargo manifest (literal values only). */
export function cargoPackage(text) {
  const section = /^\[package\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m.exec(text ?? "")?.[1] ?? "";
  const field = (name) => new RegExp(`^\\s*${name}\\s*=\\s*"([^"]*)"`, "m").exec(section)?.[1];
  return { name: field("name"), version: field("version"), publish: !/^\s*publish\s*=\s*false\b/m.test(section) };
}

/** crates/<dir>/Cargo.toml packages by crate name. */
function workspaceCrates(root) {
  const crates = new Map();
  for (const dir of listDirs(root, "crates")) {
    const pkg = cargoPackage(readText(root, `crates/${dir}/Cargo.toml`));
    if (pkg.name) crates.set(pkg.name, { ...pkg, dir: `crates/${dir}` });
  }
  return crates;
}

/** packages/<dir>/package.json manifests by package name. */
function workspacePackages(root) {
  const packages = new Map();
  for (const dir of listDirs(root, "packages")) {
    const manifest = readJson(root, `packages/${dir}/package.json`);
    if (manifest?.name) packages.set(manifest.name, { manifest, dir: `packages/${dir}`, file: `packages/${dir}/package.json` });
  }
  return packages;
}

/** Path packages (no `source`) of a Cargo.lock: [{ name, version }]. */
export function lockPathPackages(text) {
  return String(text ?? "").split(/^\[\[package\]\]\s*$/m).slice(1).map((block) => ({
    name: /^name\s*=\s*"([^"]+)"/m.exec(block)?.[1],
    version: /^version\s*=\s*"([^"]+)"/m.exec(block)?.[1],
    source: /^source\s*=/m.test(block),
  })).filter((p) => p.name && p.version && !p.source);
}

/** Files of a cargo PK recipe directory with their sha256, the way cargoRecipe hashes them. */
function recipeFileHashes(root, unit) {
  const directory = path.join(root, "ci", "consumers", unit);
  const files = {};
  const visit = (dir) => {
    for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, ent.name);
      if (ent.isDirectory()) visit(full);
      else if (ent.isFile()) files[path.relative(directory, full).replaceAll("\\", "/")] = sha256(fs.readFileSync(full));
    }
  };
  try { visit(directory); } catch { return null; }
  return files;
}

/** What changed between the reviewed inventory and the committed recipe, file by file, with the hash to review. */
function inventoryDiff(reviewed = {}, actual = {}, prefix = "") {
  const lines = [];
  for (const file of [...new Set([...Object.keys(reviewed), ...Object.keys(actual)])].sort()) {
    if (reviewed[file] === actual[file]) continue;
    if (actual[file] === undefined) lines.push(`${prefix}${file}: listed in the inventory but not committed`);
    else if (reviewed[file] === undefined) lines.push(`${prefix}${file}: committed but not in the inventory (sha256 ${actual[file]})`);
    else lines.push(`${prefix}${file}: inventory ${reviewed[file].slice(0, 12)}…, committed ${actual[file]}`);
  }
  return lines;
}
// ---- rules: each returns [{ problem, fix? }] -----------------------------------------------------------------------------

/** Publishable units whose committed version is not the recorded published one (packages/release/rightkit-versions.json
 * `registry`): the next publication takes them, and it refuses a unit without a reviewed consumer recipe. Found from the
 * manifests, not from ci/consumer-recipes.json, so deleting a unit's inventory entry (and its recipe directory) cannot
 * take it out of the check. */
export function pendingPublication(root) {
  const registry = readJson(root, "packages/release/rightkit-versions.json")?.registry ?? {};
  const cargo = [...workspaceCrates(root)].filter(([name, c]) => c.publish && c.version && registry.cargo?.[name] !== c.version).map(([name]) => name);
  const npm = [...workspacePackages(root)].filter(([name, p]) => !p.manifest.private && p.manifest.version && registry.npm?.[name] !== p.manifest.version).map(([name]) => name);
  return { cargo: cargo.sort(), npm: npm.sort() };
}

/** Recipe coverage, explicit: every publishable unit has a reviewed recipe or is listed in ci/recipe-coverage.json
 * `uncovered` (with why). A new unit with neither, a listed unit that now has a recipe, or a listed name that is no
 * publishable unit each fails, so the list is exactly the units still without one and can only shrink by review. */
function coverageProblems(root, kind, publishable, covered) {
  const file = "ci/recipe-coverage.json";
  const listed = readJson(root, file)?.uncovered?.[kind] ?? {};
  const out = [];
  for (const unit of [...publishable].sort()) {
    if (!covered.has(unit) && !Object.hasOwn(listed, unit)) out.push({ problem: `${unit}: publishable but has no reviewed consumer recipe (ci/consumer-recipes.json ${kind}) and is not listed in ${file} uncovered.${kind}`, fix: `write and review its recipe, or list it in ${file} uncovered.${kind} with the reason` });
  }
  for (const unit of Object.keys(listed).sort()) {
    if (covered.has(unit)) out.push({ problem: `${unit}: listed in ${file} uncovered.${kind} but has a reviewed recipe`, fix: `remove it from ${file} uncovered.${kind}` });
    else if (!publishable.has(unit)) out.push({ problem: `${unit}: listed in ${file} uncovered.${kind} but no publishable ${kind === "cargo" ? "crate" : "package"} has that name`, fix: `remove it from ${file} uncovered.${kind}` });
  }
  return out;
}

// The recipe rules (G01) police RightKit's own publication; an app has no recipe inventory or version feed to check.
const hasRecipeInputs = (root) => fs.existsSync(path.join(root, "ci/consumer-recipes.json")) || fs.existsSync(path.join(root, "packages/release/rightkit-versions.json"));

function consumerRecipes({ root }) {
  if (!hasRecipeInputs(root)) return [];
  const inventory = readJson(root, "ci/consumer-recipes.json");
  const units = new Set([...Object.keys(inventory?.cargo ?? {}), ...listDirs(root, "ci/consumers").filter((d) => d.startsWith("rightkit-")), ...pendingPublication(root).cargo]);
  const out = [];
  for (const unit of [...units].sort()) {
    const result = consumerRecipePolicy(root, unit);
    if (result.ok) continue;
    const diff = inventoryDiff(inventory?.cargo?.[unit]?.files, recipeFileHashes(root, unit) ?? {}, "  ");
    out.push({ problem: `${unit}: ${result.reason}${diff.length ? `\n${diff.join("\n")}` : ""}`, fix: diff.length ? `review the recipe change, then set ci/consumer-recipes.json cargo.${unit}.files to the committed hashes above` : undefined });
  }
  const publishable = new Set([...workspaceCrates(root)].filter(([, c]) => c.publish).map(([name]) => name));
  const covered = new Set([...Object.keys(inventory?.cargo ?? {}), ...listDirs(root, "ci/consumers")]);
  return [...out, ...coverageProblems(root, "cargo", publishable, covered)];
}

function npmRecipes({ root }) {
  if (!hasRecipeInputs(root)) return [];
  const inventory = readJson(root, "ci/consumer-recipes.json");
  const out = [];
  const units = new Map(Object.entries(inventory?.npm ?? {}));
  for (const unit of pendingPublication(root).npm) if (!units.has(unit)) units.set(unit, {});
  for (const [unit, entry] of [...units].sort(([a], [b]) => a.localeCompare(b))) {
    const result = npmRecipePolicy(root, unit);
    if (result.ok) continue;
    const actual = Object.fromEntries(Object.keys(entry.files ?? {}).map((f) => { try { return [f, sha256(fs.readFileSync(path.join(root, f)))]; } catch { return [f, undefined]; } }).filter(([, h]) => h));
    const diff = inventoryDiff(entry.files, actual, "  ");
    out.push({ problem: `${unit}: ${result.reason}${diff.length ? `\n${diff.join("\n")}` : ""}`, fix: diff.length ? `review the recipe change, then set ci/consumer-recipes.json npm["${unit}"].files to the committed hashes above` : undefined });
  }
  const publishable = new Set([...workspacePackages(root)].filter(([, p]) => !p.manifest.private).map(([name]) => name));
  return [...out, ...coverageProblems(root, "npm", publishable, new Set(Object.keys(inventory?.npm ?? {})))];
}

function npmSurface({ root }) {
  const inventory = readJson(root, "ci/consumer-recipes.json");
  const packages = workspacePackages(root);
  const out = [];
  // @rightkit/logs is the reviewed closure dependency, not a recipe-qualified unit (npm-recipe-consumer hasReviewedNpmEntry).
  for (const [unit, entry] of Object.entries(inventory?.npm ?? {}).sort()) {
    if (unit === "@rightkit/logs") continue;
    const pkg = packages.get(unit);
    if (!pkg) { out.push({ problem: `${unit}: listed in ci/consumer-recipes.json but no packages/*/package.json has that name` }); continue; }
    try {
      validateRecipeEntry(entry, unit);
      validatePackedClosure(pkg.manifest, entry, unit, pkg.manifest.version);
    } catch (error) {
      out.push({ problem: `${unit}: ${error.message}`, fix: `make ci/consumer-recipes.json npm["${unit}"] (surface, importClosures, closure) match ${pkg.file} exports and dependencies` });
    }
  }
  return out;
}

function versionPins({ root }) {
  const out = [];
  const units = [
    ...[...workspacePackages(root)].filter(([, p]) => !p.manifest.private).map(([name]) => name),
    ...[...workspaceCrates(root)].filter(([name, c]) => c.publish && fs.existsSync(path.join(root, "ci", "consumers", name, "Cargo.toml"))).map(([name]) => name),
  ].sort();
  for (const unit of units) for (const problem of versionPinProblems(root, unit)) out.push({ problem: `${unit}: ${problem}` });
  // versionPinProblems checks a version-bound dispatch exception for npm units only; a crate's goes stale the same way
  // (rightkit-automation 0.6.0 -> 0.6.1, 0b1041fc). release-version-pins.mjs is digest-pinned by the dispatcher, so the crate
  // half lives here rather than in that file.
  const crates = workspaceCrates(root);
  for (const [unit, exception] of Object.entries(readJson(root, "ci/rk-pipeline.json")?.dispatch?.exceptions ?? {})) {
    const crate = crates.get(unit);
    if (crate?.version && exception?.version !== crate.version) {
      out.push({ problem: `${unit}: ci/rk-pipeline.json dispatch.exceptions["${unit}"].version is ${JSON.stringify(exception?.version)} but ${crate.dir}/Cargo.toml is ${crate.version}; the exception no longer applies`, fix: `if ${unit} ${crate.version} still needs it (re-read its reason), set its version to "${crate.version}"; otherwise remove the entry` });
    }
  }
  return out;
}

// Which committed file each named digest constant of scripts/ci/rightkit-dispatch.mjs pins. A constant this table does not
// know refuses: a new pin must be added here, never silently unchecked.
const DISPATCH_PIN_FILES = Object.freeze({
  PREFLIGHT_SHA256: "scripts/ci/rk-preflight.mjs",
  NPM_PREFLIGHT_SHA256: "scripts/ci/rk-npm-preflight.mjs",
  NPM_SELECT_SHA256: "scripts/ci/select.mjs",
  NPM_VERSION_PINS_SHA256: "scripts/ci/release-version-pins.mjs",
});

function dispatchPins({ root }) {
  const source = readText(root, "scripts/ci/rightkit-dispatch.mjs");
  if (source === null) return [];
  const pins = [];
  for (const m of source.matchAll(/^const ([A-Z0-9_]+_SHA256) = "([0-9a-f]{64})";/gm)) {
    if (!DISPATCH_PIN_FILES[m[1]]) return [{ problem: `scripts/ci/rightkit-dispatch.mjs pins ${m[1]}, which preflight cannot map to a file`, fix: "add the constant and its file to DISPATCH_PIN_FILES in scripts/preflight.mjs" }];
    pins.push({ name: m[1], file: DISPATCH_PIN_FILES[m[1]], expected: m[2] });
  }
  const table = /const ADMISSION_DEPENDENCIES = \{([\s\S]*?)\};/.exec(source)?.[1];
  if (table === undefined) return [{ problem: "scripts/ci/rightkit-dispatch.mjs has no ADMISSION_DEPENDENCIES table preflight can read" }];
  for (const m of table.matchAll(/"([^"]+)":\s*"([0-9a-f]{64})"/g)) pins.push({ name: `ADMISSION_DEPENDENCIES["${m[1]}"]`, file: m[1], expected: m[2] });
  const out = [];
  for (const pin of pins) {
    let actual;
    try { actual = sha256(fs.readFileSync(path.join(root, pin.file))); } catch { out.push({ problem: `${pin.name} pins ${pin.file}, which is not committed` }); continue; }
    if (actual !== pin.expected) out.push({ problem: `${pin.file} changed but scripts/ci/rightkit-dispatch.mjs ${pin.name} still pins ${pin.expected.slice(0, 12)}… (the local dispatch refuses: "implementation digest changed")`, fix: `review the helper change, then set ${pin.name} to ${actual}` });
  }
  return out;
}

// The locks a gate builds with --locked. desktop-app-template, the spikes and the crates' example apps are built by no gate
// (scripts/ci/gate.mjs "Not covered by this gate") and keep whatever versions they were last locked at.
const LOCKED_BUILDS = /^(?:Cargo\.lock|ci\/consumers\/.+\/Cargo\.lock)$/;

function consumerLocks({ root, files }) {
  const crates = workspaceCrates(root);
  const locks = files.filter((f) => LOCKED_BUILDS.test(f));
  const out = [];
  for (const lock of locks) {
    for (const pkg of lockPathPackages(readText(root, lock))) {
      const crate = crates.get(pkg.name);
      if (crate?.version && crate.version !== pkg.version) {
        out.push({ problem: `${lock} lists ${pkg.name} ${pkg.version} but ${crate.dir}/Cargo.toml is ${crate.version} (cargo --locked refuses to update it)`, fix: `set version = "${crate.version}" in the ${pkg.name} [[package]] entry of ${lock} (a path package: no registry change), or regenerate that lock` });
      }
    }
  }
  return out;
}

function generatedDocs({ root, files }) {
  // Only a repository that carries the generator (RightKit) has generated docs to compare.
  if (!files.includes("scripts/generate-index.mjs")) return [];
  const r = spawnSync(process.execPath, [path.join(REPO, "scripts", "generate-index.mjs"), "--check", "--root", root], { encoding: "utf8" });
  if (r.status === 0) return [];
  const drift = `${r.stdout}${r.stderr}`.split("\n").filter((l) => l.startsWith("DRIFT")).map((l) => l.replace(/^DRIFT /, ""));
  return [{ problem: drift.length ? drift.join("; ") : `scripts/generate-index.mjs --check failed: ${(r.stderr || r.stdout).trim()}`, fix: "node scripts/generate-index.mjs, then commit INDEX.md, docs/CAPABILITY-MAP.md and, for a port matrix drift, docs/PORT-MATRIX.md, docs/port-matrix.html and docs/port-matrix.json" }];
}

const appRule = (id) => APP_RULES.find((rule) => rule.id === id) ?? (() => { throw new Error(`packages/release/app-check.mjs has no rule ${id}`); })();
const APP_RULES_FIRST = ["eol-policy", "file-url", "retired-alias"];

export const RULES = Object.freeze([
  appRule("eol-policy"),
  { id: "consumer-recipe", gap: "G01", run: consumerRecipes },
  { id: "npm-recipe", gap: "G01", run: npmRecipes },
  { id: "npm-surface", gap: "G02", run: npmSurface },
  { id: "version-pins", gap: "G03", run: versionPins },
  { id: "dispatch-pins", gap: "G05", run: dispatchPins },
  { id: "consumer-locks", gap: "G08", run: consumerLocks },
  appRule("file-url"),
  { id: "generated-docs", gap: "G10", run: generatedDocs },
  appRule("retired-alias"),
  // G13-G21, the app defects, in app-check's order.
  ...APP_RULES.filter((rule) => !APP_RULES_FIRST.includes(rule.id)),
]);

/** Run the selected rules over an exported tree. Returns { failures: [{ rule, gap, problem, fix, ... }], timings }. */
export function runRules({ repo = REPO, ...options }) {
  return runAppRules({ repo, rules: RULES, ...options });
}

export function main(argv, io = {}) {
  return appCheckMain(argv, { defaultRepo: REPO, rules: RULES, label: "preflight", central: REPO, ...io });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(await main(process.argv.slice(2)));
}
