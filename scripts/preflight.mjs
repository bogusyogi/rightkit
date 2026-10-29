#!/usr/bin/env node
// Preflight: the cheap checks. Reads committed files only: no network, no cargo, no pnpm, no compile, no broker, no test run,
// no candidate generator. Seconds on any host, in any checkout, with or without node_modules. ci.yml runs it as the plan job's
// first step on every push, so a red tree stops within a minute instead of after native shards; running it locally is optional and
// never a push precondition (no git hook runs it: docs/GATE-CONTRACT.md).
//
// Why (scan-gaps report, 2026-10-23): 555 of 1,455 hosted runs failed, and 144 of those failed on a rule that only reads
// files. The rules already existed, but as late refusals (rk-preflight, rk-npm-preflight, rightkit-dispatch, the hosted
// verify) for the one unit being published, minutes in. Preflight runs the same predicates first, for every unit.
//
// The rules that read any repository (eol-policy G04, file-url G12, retired-alias P11 and the app defects G13-G22), the export of
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
//   consumer-locks   G08  every crates/<x>/Cargo.lock (one workspace and lock per crate, rightkit#1285) and ci/consumers/** Cargo.lock list
//                         each path crate at its crates/<name>/Cargo.toml version: a sibling version bump leaves each dependent's lock stale
//                         (cargo --locked refuses a stale one: tauri-shell consumer-0-0, wv2-0_39, 2026-10-23)
//   generated-docs   G10  scripts/generate-index.mjs --check: INDEX.md and docs/CAPABILITY-MAP.md match the committed sources, and the
//                         port matrix's MD and HTML are the rendering of its JSON (offline: reads manifests and the version feed only)
//   eol-pinned       G25  a tracked text file whose .gitattributes entry leaves eol to core.autocrlf: a Windows checkout writes CRLF into it,
//                         so a test or generator that compares or hashes its bytes fails only there (#1175, #1187). The repository pins
//                         `* text=auto eol=lf` once; this refuses a file that line no longer reaches
//   tmpdir-realpath  G26  a test that passes its temp directory to realpath and also compares the unresolved directory: macOS answers
//                         /private/var/... for /var/... (outdated-command.test.mjs before #1187)
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

// The locks a gate builds with --locked: every crate's own lock, the consumers and desktop-app-template (its own workspace, discovered by
// rightkit-gate). The spikes and the crates' example apps are built by no gate (scripts/ci/gate.mjs "Not covered by this gate")
// and keep whatever versions they were last locked at.
const LOCKED_BUILDS = /^(?:crates\/[^/]+\/Cargo\.lock|ci\/consumers\/.+\/Cargo\.lock|desktop-app-template\/Cargo\.lock)$/;

function consumerLocks({ root, files }) {
  const crates = workspaceCrates(root);
  const locks = files.filter((f) => LOCKED_BUILDS.test(f));
  const out = [];
  for (const lock of locks) {
    for (const pkg of lockPathPackages(readText(root, lock))) {
      const crate = crates.get(pkg.name);
      if (crate?.version && crate.version !== pkg.version) {
        out.push({ problem: `${lock} lists ${pkg.name} ${pkg.version} but ${crate.dir}/Cargo.toml is ${crate.version} (cargo --locked refuses to update it)`, fix: `set version = "${crate.version}" in the ${pkg.name} [[package]] entry of ${lock} (a path package: no registry change), or regenerate that lock in ${path.posix.dirname(lock)}; every crate that path-depends on ${pkg.name} owns its own lock and needs the same bump` });
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

// ---- G25 and G26: bugs that only a Windows or macOS runner showed, found from files alone ----------------------------------------
// Both police RightKit's own tree (they switch on scripts/preflight.mjs being committed), so an app checked from here stays quiet.

// git's repository-location variables are dropped from the git call (packages/release/app-check.mjs does the same): a hook runs
// with some of them set, and `--repo` must mean the repository it names.
const GIT_LOCATION = /^GIT_(?:DIR|WORK_TREE|INDEX_FILE|COMMON_DIR|PREFIX|OBJECT_DIRECTORY|ALTERNATE_OBJECT_DIRECTORIES|NAMESPACE)$/i;
const gitEnv = () => Object.fromEntries(Object.entries(process.env).filter(([key]) => !GIT_LOCATION.test(key)));
const RIGHTKIT_TREE = "scripts/preflight.mjs";
const VENDORED_OR_INSTALLED = /(^|\/)(node_modules|vendor|target)\//;

/** git's own test for "this is not text" (convert.c convert_is_binary): a NUL, a lone CR, or mostly non-printable bytes. */
export function looksBinary(bytes) {
  let printable = 0, nonprintable = 0, nul = 0, loneCr = 0;
  for (let i = 0; i < bytes.length; i++) {
    const c = bytes[i];
    if (c === 13) { if (bytes[i + 1] === 10) i++; else loneCr++; continue; }
    if (c === 10) continue;
    if (c === 127) nonprintable++;
    else if (c < 32) {
      if (c === 8 || c === 9 || c === 12 || c === 27) printable++;
      else { if (c === 0) nul++; nonprintable++; }
    } else printable++;
  }
  if (bytes.length > 0 && bytes[bytes.length - 1] === 26) nonprintable--; // a trailing ^Z is not counted
  return loneCr > 0 || nul > 0 || (printable >> 7) < nonprintable;
}

/** G25: a Windows checkout (core.autocrlf=true, the default of Git for Windows and of the hosted runners) writes CRLF into every
 * text file whose .gitattributes entry leaves `eol` unset. A test that compares such a file byte for byte, or a generator that
 * hashes it, passes on Linux and macOS and fails only there (docs/RELEASE-EXPORTS.md in release-exports.test.mjs, #1187; the Swift
 * Right Suite API projection of #1175, which scripts/gen-suite-api.mjs compares strictly and .gitattributes did not list). The
 * repository pins `* text=auto eol=lf` once; this refuses a text file that line no longer reaches. `-text` (bytes kept as
 * committed) and `eol=crlf` (a .bat or .cmd) are pinned too: git converts nothing on any host, or the same thing on every host. */
function eolPinned({ root, files, repo = REPO }) {
  if (!files.includes(RIGHTKIT_TREE)) return [];
  const paths = files.filter((f) => !/(^|\/)node_modules\//.test(f)); // vendored trees stay in: their bytes are the ones that get hashed
  const r = spawnSync("git", ["--work-tree", root, "check-attr", "-z", "--stdin", "text", "eol"], { cwd: repo, env: gitEnv(), input: paths.join("\0"), encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  if (r.error || r.status !== 0) throw new Error(`git check-attr failed: ${r.error?.message ?? r.stderr.trim()}`);
  const fields = r.stdout.split("\0");
  const seen = new Map();
  for (let i = 0; i + 2 < fields.length; i += 3) seen.set(fields[i], { ...seen.get(fields[i]), [fields[i + 1]]: fields[i + 2] });
  const loose = [];
  for (const [file, a] of seen) {
    if (a.text === "unset" || a.eol === "lf" || a.eol === "crlf") continue;
    let bytes;
    try { bytes = fs.readFileSync(path.join(root, file)); } catch { continue; } // a submodule directory
    // Nothing to convert in a file without a line break (git's i/none), or in a binary one.
    if (bytes.includes(10) && !looksBinary(bytes)) loose.push(file);
  }
  if (!loose.length) return [];
  loose.sort();
  return [{
    problem: `${loose.length} tracked text file(s) have no pinned line ending: .gitattributes leaves eol to core.autocrlf, so a Windows checkout writes CRLF and a byte-for-byte comparison or hash of them fails there (${loose.slice(0, 5).join(", ")}${loose.length > 5 ? `, and ${loose.length - 5} more` : ""})`,
    fix: "make `* text=auto eol=lf` the first line of .gitattributes again, or give the file its own entry: `text eol=lf`, `-text` when its bytes must stay as committed, `eol=crlf` for a .bat or .cmd",
  }];
}

// G26 reads test files with a small scanner: strings, template literals, comments and regex literals are skipped, and each
// identifier is reported with the callee of every call that encloses it.
function skipQuoted(text, at) {
  const quote = text[at];
  let i = at + 1;
  while (i < text.length && text[i] !== quote) {
    if (text[i] === "\\") i++;
    else if (quote === "`" && text[i] === "$" && text[i + 1] === "{") {
      let depth = 1;
      for (i += 2; i < text.length && depth; i++) {
        if (text[i] === "{") depth++;
        else if (text[i] === "}") depth--;
        else if (text[i] === "`" || text[i] === '"' || text[i] === "'") i = skipQuoted(text, i) - 1;
      }
      i--;
    } else if (quote !== "`" && text[i] === "\n") return i;
    i++;
  }
  return i + 1;
}
function skipRegex(text, at) {
  let i = at + 1, inClass = false;
  for (; i < text.length && text[i] !== "\n"; i++) {
    if (text[i] === "\\") i++;
    else if (text[i] === "[") inClass = true;
    else if (text[i] === "]") inClass = false;
    else if (text[i] === "/" && !inClass) return i + 1;
  }
  return at + 1;
}
function* identifiersWithCallers(text) {
  const callers = [];
  let last = "";
  for (let i = 0; i < text.length;) {
    const c = text[i];
    if (c === "/" && text[i + 1] === "/") { while (i < text.length && text[i] !== "\n") i++; continue; }
    if (c === "/" && text[i + 1] === "*") { const end = text.indexOf("*/", i + 2); i = end < 0 ? text.length : end + 2; continue; }
    if (c === '"' || c === "'" || c === "`") { i = skipQuoted(text, i); last = '"'; continue; }
    if (c === "/" && (last === "" || "(,=:[!&|?{};".includes(last))) { i = skipRegex(text, i); last = "/"; continue; }
    if (/[A-Za-z_$]/.test(c)) {
      let j = i + 1;
      while (j < text.length && /[\w$]/.test(text[j])) j++;
      yield { name: text.slice(i, j), at: i, property: text[i - 1] === "." || /^\s*:/.test(text.slice(j, j + 8)), callers: callers.slice() };
      last = "a";
      i = j;
      continue;
    }
    if (c === "(") callers.push(/([\w$.]+)\s*$/.exec(text.slice(Math.max(0, i - 80), i))?.[1] ?? "");
    else if (c === ")") callers.pop();
    if (!/\s/.test(c)) last = c;
    i++;
  }
}

const PATH_BUILDERS = new Set(["path.join", "path.resolve", "path.posix.join", "path.win32.join", "join", "resolve"]);
const ASSERT_EQUALITY = /^assert(?:\.strict)?\.(?:equal|strictEqual|deepEqual|deepStrictEqual|notEqual|notStrictEqual|notDeepEqual|notDeepStrictEqual)$/;
const TEST_FILE = /\.test\.(?:mjs|cjs|js|mts|ts)$/;
const TEST_START = /^[ \t]*(?:test|it)(?:\.\w+)?\(/gm;
const escapeRegExp = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** G26: macOS puts os.tmpdir() behind the /var -> /private/var symlink, so code that answers real paths (realpathSync, a child's
 * cwd) answers /private/var/... for a directory the test built as /var/.... A test that already resolves its temp directory in one
 * place (`realpathSync(app)`, to compare a child's cwd) knows this; if the same test also puts that unresolved directory into an
 * expected path (`assert.equal(found, path.join(app, ...))`), the comparison fails on the Mac only (outdated-command.test.mjs
 * before #1187). Decided per test() block from the file alone: an unresolved temp directory in a test that never resolves it, or
 * one compared only with what the code echoes back, is correct and is not reported. */
function tmpdirRealpath({ root, files }) {
  if (!files.includes(RIGHTKIT_TREE)) return [];
  const out = [];
  for (const file of files) {
    if (!TEST_FILE.test(file) || VENDORED_OR_INSTALLED.test(file)) continue;
    const text = readText(root, file);
    if (!text || !text.includes("realpath")) continue;
    const starts = [...text.matchAll(TEST_START)].map((m) => m.index);
    for (let k = 0; k < starts.length; k++) {
      const block = text.slice(starts[k], starts[k + 1] ?? text.length);
      const resolved = new Set([...block.matchAll(/\brealpath(?:Sync)?(?:\.native)?\(\s*([A-Za-z_$][\w$]*)\s*\)/g)].map((m) => m[1]));
      for (const name of resolved) {
        // Bound in this test straight from a temp-directory creator (mkdtemp*, tempDir, tmp, scratch...), not already resolved: a path
        // derived from another call is not a temp root, and whether that call resolves it is not decidable from here.
        if (!new RegExp(`(?:const|let)\\s+${escapeRegExp(name)}\\s*=\\s*(?=\\S)(?!(?:await\\s+)?(?:\\w+\\.)*realpath)(?:await\\s+)?(?:\\w+\\.)*(?:mkdtemp|temp|tmp|scratch|withTemp)\\w*\\s*\\(`).test(block)) continue;
        const use = [...identifiersWithCallers(block)].find((u) => {
          if (u.name !== name || u.property) return false;
          let depth = u.callers.length - 1;
          while (depth >= 0 && PATH_BUILDERS.has(u.callers[depth])) depth--;
          return depth >= 0 && ASSERT_EQUALITY.test(u.callers[depth]);
        });
        if (!use) continue;
        const line = text.slice(0, starts[k] + use.at).split("\n").length;
        out.push({ file, subject: name, problem: `${file}:${line} compares ${name}, a temp directory this test keeps unresolved, although the same test passes ${name} to realpath: macOS reports os.tmpdir() as /var/..., and the code under test answers its real path /private/var/..., so the comparison fails there`, fix: `bind ${name} to realpathSync(...) where the test creates it, so every path built from it is already a real path` });
      }
    }
  }
  return out;
}

// G27: a version the registry already serves stays accepted while a newer one is staged. The feed lists what is published
// (registry), what is staged and what older published versions installs may still carry (legacy); dropping a published
// version from legacy while staging its successor fails right-suite-contract inside the full packages/release suite, which
// first runs minutes into a native verify (rightkit#1188 dropped @rightkit/qa-chrome 0.1.11; #1190). Same rule, files only.
const semverParts = (v) => String(v).split(/[.+-]/).slice(0, 3).map((n) => Number.parseInt(n, 10) || 0);
const VERSION = /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/;
/** Why a version feed is not the shape G27 reads: [] when it is. An empty or array-shaped feed is a problem, never "nothing to check". */
export function feedShapeProblems(feed) {
  const out = [];
  const plain = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
  if (!plain(feed)) return ["the feed is not a JSON object"];
  if (!plain(feed.registry)) out.push("registry is missing or not an object");
  else for (const r of ["npm", "cargo"]) {
    if (!plain(feed.registry[r])) { out.push(`registry.${r} is missing or not an object`); continue; }
    for (const [n, v] of Object.entries(feed.registry[r])) if (typeof v !== "string" || !VERSION.test(v)) out.push(`registry.${r}["${n}"] is not a version string`);
  }
  for (const k of ["stagedNpm", "stagedCargo"]) {
    if (!plain(feed[k])) { out.push(`${k} is missing or not an object`); continue; }
    for (const [n, v] of Object.entries(feed[k])) if (typeof v !== "string" || !VERSION.test(v)) out.push(`${k}["${n}"] is not a version string`);
  }
  for (const k of ["legacyNpm", "legacyCargo"]) {
    if (!plain(feed[k])) { out.push(`${k} is missing or not an object`); continue; }
    for (const [n, v] of Object.entries(feed[k])) if (!Array.isArray(v) || v.some((x) => typeof x !== "string" || !VERSION.test(x))) out.push(`${k}["${n}"] is not an array of version strings`);
  }
  return out;
}
const olderThan = (a, b) => { const x = semverParts(a), y = semverParts(b); for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] < y[i]; return false; };
function feedLegacy({ root, files = [] }) {
  const out = [];
  if (!files.includes(RIGHTKIT_TREE)) return out; // an app has no version feed
  const feed = readJson(root, "packages/release/rightkit-versions.json");
  // A feed that cannot be read is not a clean feed: without it no rollout check can run (simulation A4.05 found the silent pass).
  if (!feed || typeof feed !== "object") return [{ subject: "rightkit-versions.json", problem: "packages/release/rightkit-versions.json is missing or not valid JSON, so the published/staged/legacy rollout check cannot run", fix: "restore the feed (valid JSON with registry, stagedNpm/stagedCargo and legacyNpm/legacyCargo)" }];
  const shape = feedShapeProblems(feed);
  if (shape.length) return shape.map((problem) => ({ subject: "rightkit-versions.json", problem: `packages/release/rightkit-versions.json: ${problem}; the rollout check cannot run on this shape`, fix: "restore the feed's shape: registry.npm/registry.cargo (name → version), stagedNpm/stagedCargo (name → version), legacyNpm/legacyCargo (name → [versions])" }));
  for (const [registry, stagedKey, legacyKey] of [["npm", "stagedNpm", "legacyNpm"], ["cargo", "stagedCargo", "legacyCargo"]]) {
    for (const [name, published] of Object.entries(feed.registry?.[registry] ?? {})) {
      const staged = feed[stagedKey]?.[name];
      if (!staged || !olderThan(published, staged)) continue;
      if (!(feed[legacyKey]?.[name] ?? []).includes(published)) {
        out.push({ subject: name, problem: `packages/release/rightkit-versions.json: ${name} ${published} is published (registry.${registry}) and ${staged} is staged (${stagedKey}), but ${legacyKey}["${name}"] does not list ${published}; installs still carrying ${published} would be refused`, fix: `add "${published}" to ${legacyKey}["${name}"] in packages/release/rightkit-versions.json and its byte-identical copy packages/build-control-policy/src/ownership-session-versions.json` });
      }
    }
  }
  return out;
}

const appRule = (id) => APP_RULES.find((rule) => rule.id === id) ?? (() => { throw new Error(`packages/release/app-check.mjs has no rule ${id}`); })();
const APP_RULES_FIRST = ["eol-policy", "file-url", "retired-alias"];

export const RULES = Object.freeze([
  appRule("eol-policy"),
  { id: "eol-pinned", gap: "G25", run: eolPinned },
  { id: "consumer-recipe", gap: "G01", run: consumerRecipes },
  { id: "npm-recipe", gap: "G01", run: npmRecipes },
  { id: "npm-surface", gap: "G02", run: npmSurface },
  { id: "version-pins", gap: "G03", run: versionPins },
  { id: "feed-legacy", gap: "G27", run: feedLegacy },
  { id: "dispatch-pins", gap: "G05", run: dispatchPins },
  { id: "consumer-locks", gap: "G08", run: consumerLocks },
  appRule("file-url"),
  { id: "generated-docs", gap: "G10", run: generatedDocs },
  { id: "tmpdir-realpath", gap: "G26", run: tmpdirRealpath },
  appRule("retired-alias"),
  // G13-G22, the app defects, in app-check's order.
  ...APP_RULES.filter((rule) => !APP_RULES_FIRST.includes(rule.id)),
]);

/** Run the selected rules over an exported tree. Returns { failures: [{ rule, gap, problem, fix, ... }], timings }. */
export function runRules({ repo = REPO, ...options }) {
  return runAppRules({ repo, rules: RULES, ...options });
}

export function main(argv, io = {}) {
  // RightKit checks itself with every rule: the compile/release tiers (app-check.mjs TIERS) exist for the apps, not for the owner.
  return appCheckMain(argv.includes("--rule-tier") ? argv : [...argv, "--rule-tier", "release"], { defaultRepo: REPO, rules: RULES, label: "preflight", central: REPO, ...io });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(await main(process.argv.slice(2)));
}
