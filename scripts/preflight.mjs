#!/usr/bin/env node
// Preflight: the cheap checks, before a push leaves the machine. Reads committed files only: no network, no cargo, no pnpm,
// no compile, no broker, no test run, no candidate generator. Seconds on any host, in any checkout, with or without
// node_modules. .githooks/pre-push runs it before the gate; ci.yml runs it as the plan job's first step, so a push that skipped
// the hook still stops within a minute instead of after native shards.
//
// Why (scan-gaps report, 2026-10-23): 555 of 1,455 hosted runs failed, and 144 of those failed on a rule that only reads
// files. The rules already existed, but as late refusals (rk-preflight, rk-npm-preflight, rightkit-dispatch, the hosted
// verify) for the one unit being published, minutes in. Preflight runs the same predicates first, for every unit:
//
//   eol-policy       G04  a committed blob has CRLF where .gitattributes says eol=lf (every checkout shows it modified;
//                         byte-compared and hashed copies differ on Windows)
//   consumer-recipe  G01  scripts/ci/consumer-recipes.mjs cargoRecipe: Cargo PK recipe inventory, exact hashes, exact version
//   npm-recipe       G01  scripts/ci/rk-npm-preflight.mjs npmRecipePolicy: npm PK recipe inventory, hashes, closure exception
//   npm-surface      G02  scripts/ci/npm-recipe-consumer.mjs validateRecipeEntry + validatePackedClosure on the source
//                         package.json: reviewed surface, importClosures and dependencies match the exports
//   version-pins     G03  scripts/ci/release-version-pins.mjs versionPinProblems: every hand-written copy of a unit's version
//   dispatch-pins    G05  the helper digests pinned in scripts/ci/rightkit-dispatch.mjs match the committed helpers
//   consumer-locks   G08  the root and ci/consumers/** Cargo.lock list each path crate at its crates/<name>/Cargo.toml version
//                         (cargo --locked refuses a stale one: tauri-shell consumer-0-0, wv2-0_39, 2026-10-23)
//   file-url         G12  new URL(..., import.meta.url).pathname used as a path: wrong on Windows; use fileURLToPath
//   generated-docs   G10  scripts/generate-index.mjs --check: INDEX.md and docs/CAPABILITY-MAP.md match the committed sources
//                         (offline: reads manifests and the version feed only)
//   retired-alias    P11  an invocation of `rightkit`, the retired name of the RustRight client (rustright#91), outside
//                         history; scripts/retire-rightkit-alias.mjs rewrites them (any repository: --repo)
//
// Not here yet: G07 (ignored tests) refuses rightkit-pdf and rightkit-credentials today, and its fix is a reviewed allowIgnored
// exemption, which preflight never authors. G06, G09, G11 and G13 need a parser.
//
// The rules come from this checkout's scripts; the content is the commit being checked. A FAIL line says what to change.
// Step 1 of docs/PIPELINE-ARCHITECTURE.md; invariants P1, P2, P3 and P9 there name the tests that hold it to this.
//
// Usage: node scripts/preflight.mjs [--repo <dir>] [--commit <rev> | --worktree] [--only <rule,...>] [--list]
//   --repo <dir>    the repository to check (default: this checkout)
//   --commit <rev>  the commit to check (default HEAD), exported with git archive: an uncommitted fix is not what is pushed.
//   --worktree      check what a compile would see instead: tracked and untracked, non-ignored files as they are on disk
//                   (written to a throwaway index, never the repository's own)
// Exit 0: every rule passed. 1: at least one FAIL. 2: preflight itself could not run.
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { cargoRecipe } from "./ci/consumer-recipes.mjs";
import { validatePackedClosure, validateRecipeEntry } from "./ci/npm-recipe-consumer.mjs";
import { npmRecipePolicy } from "./ci/rk-npm-preflight.mjs";
import { consumerRecipePolicy } from "./ci/rk-preflight.mjs";
import { versionPinProblems } from "./ci/release-version-pins.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const readText = (root, rel) => { try { return fs.readFileSync(path.join(root, rel), "utf8"); } catch { return null; } };
const readJson = (root, rel) => { const text = readText(root, rel); try { return text === null ? null : JSON.parse(text); } catch { return null; } };
const listDirs = (root, rel) => { try { return fs.readdirSync(path.join(root, rel), { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => e.name).sort(); } catch { return []; } };

function git(args, { cwd = REPO, input, encoding = "utf8" } = {}) {
  const r = spawnSync("git", args, { cwd, input, encoding, maxBuffer: 512 * 1024 * 1024 });
  if (r.error || r.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${r.error?.message ?? String(r.stderr).trim()}`);
  return r.stdout;
}

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

function eolPolicy({ root, commit, repo = REPO }) {
  // CR-bearing text blobs of the commit, then the commit's own attributes (the exported tree holds its .gitattributes).
  const grep = spawnSync("git", ["grep", "-z", "-l", "-I", "-e", "\r", commit, "--"], { cwd: repo, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  if (grep.status === 1) return []; // git grep: 1 = no match
  if (grep.error || grep.status !== 0) throw new Error(`git grep failed: ${grep.error?.message ?? grep.stderr.trim()}`);
  const files = grep.stdout.split("\0").filter(Boolean).map((f) => f.slice(commit.length + 1));
  const attrs = git(["--work-tree", root, "check-attr", "-z", "--stdin", "eol"], { cwd: repo, input: files.join("\0") }).split("\0");
  const bad = [];
  for (let i = 0; i + 2 < attrs.length; i += 3) if (attrs[i + 2] === "lf") bad.push(attrs[i]);
  return bad.length ? [{
    problem: `${bad.length} file(s) committed with CRLF although .gitattributes says eol=lf (a fresh checkout shows them modified; hashed and byte-compared copies differ on Windows): ${bad.join(", ")}`,
    fix: `git add --renormalize -- ${bad.join(" ")} && git commit`,
  }] : [];
}

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

function consumerRecipes({ root }) {
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

// The RustRight client's subcommands as the retired `rightkit` alias took them (rustright crates/rustright-cli/src/cli.rs).
const ALIAS_SUBCOMMANDS = "cargo|rustc|rustdoc|run|build|owns|status|attach|cancel|list|quiet|queue|prune|deps|profiles|forecast-report|resume-admission|pause-admission|remote|cutover|storage|verify-receipt";
// Each form is [pattern, replacement] or [pattern, replacement, { unlessRetired, lineMust }]: `unlessRetired` spares a line that
// says "retired", where code names the old client on purpose to refuse it (`if (name === "rightkit") refuse("R1-retired-client")`);
// `lineMust` limits a form to lines that also match it (a ternary needs its `?`, so JSON's `"kind": "rightkit"` is data).
// Forms match across lines (a multi-line argv array), so the rule reads whole files, not lines.
const ALIAS_FORMS = [
  // rightkit cargo ..., `rightkit status <id>` (never rightkit-qa, @rightkit/..., .../rightkit, or "rightkit build-control")
  [new RegExp(`(^|[^-./@A-Za-z0-9_$])rightkit(?= (?:${ALIAS_SUBCOMMANDS})(?![-\\w]))`, "gm"), "$1rustright"],
  // ["rightkit", "cargo", ...], its quoting variants, one element per line, and the [command, [args]] pair ["rightkit", ["cargo", ...]]
  [new RegExp(`([\\[(]\\s*)(["'\`])rightkit\\2(?=\\s*,\\s*(?:\\/\\/[^\\n]*\\n\\s*)*(?:\\[\\s*)?["'\`](?:${ALIAS_SUBCOMMANDS})["'\`])`, "g"), "$1$2rustright$2"],
  // the same inside an escaped string (a TOML fixture in a Rust or JS literal): [\"rightkit\", \"cargo\", ...]
  [new RegExp(`([\\[(]\\s*)\\\\(["'])rightkit\\\\\\2(?=\\s*,\\s*\\\\["'](?:${ALIAS_SUBCOMMANDS})\\\\["'])`, "g"), "$1\\$2rustright\\$2"],
  // vec!["rightkit".into(), "cargo".into(), ...] and .to_string() / .to_owned()
  [new RegExp(`([\\[(]\\s*)(["'])rightkit\\2(\\.(?:into|to_string|to_owned)\\(\\))(?=\\s*,\\s*["'](?:${ALIAS_SUBCOMMANDS})["'])`, "g"), "$1$2rustright$2$3"],
  // run("rightkit", ...), spawn("rightkit", ...), execFileSync("rightkit", ...), which("rightkit"), Command::new("rightkit")
  [/\b(run|spawn|spawnSync|execFile|execFileSync|execa|resolveCommand|which|Command::new)\((["'])rightkit\2/g, "$1($2rustright$2"],
  // the Windows shim by name: "rightkit.cmd", `...\agent-bin\rightkit.cmd cargo ...`
  [/(["'])rightkit\.(cmd|exe)\1/g, "$1rustright.$2$1"],
  [new RegExp(`(^|[^-./@A-Za-z0-9_$])rightkit\\.(cmd|exe)(?= (?:${ALIAS_SUBCOMMANDS})(?![-\\w]))`, "gm"), "$1rustright.$2"],
  // isWindows ? "rustright.cmd" : "rightkit" (the POSIX half of the same shim choice)
  [/(["'])rustright\.cmd\1(\s*:\s*)(["'])rightkit\3/g, "$1rustright.cmd$1$2$3rustright$3"],
  // { cmd: "rightkit", args: [...] } in a release config
  [/\bcmd:(\s*)(["'])rightkit\2/g, "cmd:$1$2rustright$2"],
  // a command compared by name: `command === "rightkit"` (a Windows .cmd mapping), `argv0 == "rightkit"`
  [/\b(command|cmd|program|bin|binary|exe|executable|tool|base|first|client|argv0)(\s*(?:===?|!==?)\s*)(["'])rightkit\3/g, "$1$2$3rustright$3", { unlessRetired: true }],
  // the POSIX half of a platform choice: `win32 ? (env.ComSpec ?? "cmd.exe") : "rightkit"`, `isWin ? "x" : "rightkit"`
  [/(\)|["'])(\s*:\s*)(["'])rightkit\3(?=\s*[;,)\n])/g, "$1$2$3rustright$3", { lineMust: /\?/ }],
  // a named command constant: `const CARGO_COMMAND = "rightkit"`, `const BROKER: &str = "rightkit"` (RETIRED_* names are refusals)
  [/\b((?![A-Z0-9_]*RETIRED)[A-Z][A-Z0-9_]*(?:COMMAND|CMD|BIN|BINARY|PROGRAM|EXE|CLI|BROKER|CLIENT))(\s*(?::\s*&(?:'static\s+)?str\s*)?=\s*)(["'])rightkit\3/g, "$1$2$3rustright$3", { unlessRetired: true }],
  // PowerShell and shell lookups and calls: `Get-Command rightkit`, `& rightkit @args`, `which rightkit`, `command -v rightkit`
  [/\b(Get-Command|which|where(?:\.exe)?|command -v)(\s+)rightkit\b(?![-.\w])(?=\s*(?:$|[)|;&>"'`\]]|-ErrorAction))/gm, "$1$2rustright"],
  [/(^|[\s(;{`])&(\s+)rightkit\b(?![-.\w])/gm, "$1&$2rustright"],
  // a per-tool table keyed by the client: `{ rightkit: ["cargo", "--version"] }`
  [/(^|[{,\s])rightkit(\s*:\s*\[)/gm, "$1rustright$2"],
];
/** History and byte-pinned content the alias may stay in: what was run then, or bytes a hash or review binds. */
export const RETIRED_ALIAS_EXEMPT = /(^|\/)CHANGELOG\.md$|^docs\/(audits|plans|archive)\/|(^|\/)(evidence|release-evidence|receipts?|fixtures?|vendor)\/|adr-\d+-evidence\/|\.evidence\/|\d{4}-\d{2}-\d{2}|(^|\/)(handoffs?|handovers?|reports?|results|experiments|reviews|research|mockups|archive|incidents?|spikes)\/|[^/]*evidence[^/]*\/|\.(jsonl|log|patch|diff)$|\.receipt\.json$|(^|\/)SHA256SUMS$|^ci\/consumers\/|^packages\/ax\/plugin\/schemas\/|^scripts\/embed-export\/|msix-repair-guard\.ps1$|\.lock$|^scripts\/(preflight|preflight\.test|retire-rightkit-alias)\.mjs$/;

/** A file generated from other sources (AGENTS.md from docs/agent-rules/): its sources are what is checked and edited. */
export const isGeneratedFile = (text) => /^\s*(?:<!--|\/\/|#)\s*GENERATED FILE\. Do not hand-edit\./.test(String(text).slice(0, 300));

const lineAt = (text, offset) => text.slice(text.lastIndexOf("\n", offset - 1) + 1, (text.indexOf("\n", offset) + 1 || text.length + 1) - 1);

/** `text` with every invocation of the retired alias renamed to `rustright`, how many there were, and the 1-based line of each. */
export function retireAliasText(text) {
  let count = 0;
  let out = text;
  const lines = new Set();
  for (const [pattern, replacement, { unlessRetired = false, lineMust = null } = {}] of ALIAS_FORMS) {
    const hits = [];
    out = out.replace(pattern, (...m) => {
      const offset = m.at(-2);
      const whole = m.at(-1);
      const line = lineAt(whole, offset);
      if ((unlessRetired && /retired/i.test(line)) || (lineMust && !lineMust.test(line))) return m[0];
      count += 1;
      hits.push(offset + m[0].search(/rightkit/));
      return replacement.replace(/\$(\d)/g, (_, i) => m[Number(i)] ?? "");
    });
    // Offsets are positions in the text before this form's replacements; every replacement keeps its line count.
    for (const at of hits) lines.add(text.slice(0, Math.max(0, at)).split("\n").length);
  }
  return { text: out, count, lines: [...lines].sort((a, b) => a - b) };
}

function retiredAlias({ root, files }) {
  const out = [];
  for (const file of files) {
    if (RETIRED_ALIAS_EXEMPT.test(file)) continue;
    let bytes;
    try { bytes = fs.readFileSync(path.join(root, file)); } catch { continue; }
    if (bytes.includes(0) || !bytes.includes("rightkit") || isGeneratedFile(bytes.toString("utf8"))) continue;
    const hits = retireAliasText(bytes.toString("utf8")).lines;
    if (hits.length) out.push({ problem: `${file}:${hits.slice(0, 8).join(",")}${hits.length > 8 ? ",…" : ""} invokes \`rightkit\`, the retired name of the RustRight client`, fix: "use rustright (node <rightkit>/scripts/retire-rightkit-alias.mjs --repo <this repository> rewrites them all)" });
  }
  return out;
}

const FILE_URL_PATHNAME = /new URL\((?:[^()]|\([^()]*\))*import\.meta\.url\s*\)\s*\.pathname/g;
function fileUrls({ root, files }) {
  const out = [];
  for (const file of files) {
    if (!/\.(?:mjs|cjs|js|ts|mts|cts|tsx)$/.test(file) || file.startsWith("vendor/") || file.includes("/node_modules/") || file === "scripts/preflight.mjs") continue;
    const text = readText(root, file);
    if (!text || !text.includes("import.meta.url")) continue;
    for (const m of text.matchAll(FILE_URL_PATHNAME)) {
      const line = text.slice(0, m.index).split("\n").length;
      out.push({ problem: `${file}:${line} turns a file URL into a path with .pathname (C:\\ becomes /C:/ and spaces become %20 on Windows)`, fix: `use fileURLToPath(new URL(..., import.meta.url)) from node:url` });
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
  return [{ problem: drift.length ? drift.join("; ") : `scripts/generate-index.mjs --check failed: ${(r.stderr || r.stdout).trim()}`, fix: "node scripts/generate-index.mjs, then commit INDEX.md and docs/CAPABILITY-MAP.md" }];
}

export const RULES = Object.freeze([
  { id: "eol-policy", gap: "G04", run: eolPolicy },
  { id: "consumer-recipe", gap: "G01", run: consumerRecipes },
  { id: "npm-recipe", gap: "G01", run: npmRecipes },
  { id: "npm-surface", gap: "G02", run: npmSurface },
  { id: "version-pins", gap: "G03", run: versionPins },
  { id: "dispatch-pins", gap: "G05", run: dispatchPins },
  { id: "consumer-locks", gap: "G08", run: consumerLocks },
  { id: "file-url", gap: "G12", run: fileUrls },
  { id: "generated-docs", gap: "G10", run: generatedDocs },
  { id: "retired-alias", gap: "P11", run: retiredAlias },
]);

/** The tree a compile of `repo` would see: HEAD plus every tracked and untracked, non-ignored file as it is on disk, written
 * through a throwaway index under `scratch` (the repository's own index is never touched). Returns the tree id. */
function worktreeTree(repo, scratch) {
  const env = { ...process.env, GIT_INDEX_FILE: path.join(scratch, ".preflight-index") };
  const run = (args) => {
    const r = spawnSync("git", args, { cwd: repo, env, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
    if (r.error || r.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${r.error?.message ?? String(r.stderr).trim()}`);
    return r.stdout.trim();
  };
  run(["read-tree", "HEAD"]);
  run(["add", "-A"]);
  return run(["write-tree"]);
}

/** Run the selected rules over an exported tree. Returns [{ rule, gap, problem, fix, ms }] failures and per-rule timings. */
export function runRules({ root, commit, files, repo = REPO, only = null }) {
  const failures = [];
  const timings = [];
  for (const rule of RULES) {
    if (only && !only.includes(rule.id)) continue;
    const started = Date.now();
    let found;
    try { found = rule.run({ root, commit, files, repo }); }
    catch (error) { found = [{ problem: `the rule itself failed: ${error.message}` }]; }
    timings.push({ rule: rule.id, ms: Date.now() - started, failures: found.length });
    for (const f of found) failures.push({ rule: rule.id, gap: rule.gap, ...f });
  }
  return { failures, timings };
}

export async function main(argv, { out = (s) => process.stdout.write(`${s}\n`), err = (s) => process.stderr.write(`${s}\n`) } = {}) {
  const arg = (flag) => { const i = argv.indexOf(flag); return i >= 0 ? argv[i + 1] : undefined; };
  if (argv.includes("--list")) { for (const r of RULES) out(`${r.id} (${r.gap})`); return 0; }
  const only = arg("--only")?.split(",").filter(Boolean) ?? null;
  const unknown = only?.filter((id) => !RULES.some((r) => r.id === id)) ?? [];
  if (unknown.length) { err(`preflight: unknown rule(s): ${unknown.join(", ")} (see --list)`); return 2; }
  if (argv.includes("--worktree") && arg("--commit")) { err("preflight: --worktree and --commit are exclusive"); return 2; }
  const repo = arg("--repo") ? path.resolve(arg("--repo")) : REPO;
  const started = Date.now();
  let commit, root;
  try {
    root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-preflight-"));
    commit = argv.includes("--worktree") ? worktreeTree(repo, root) : git(["rev-parse", "--verify", `${arg("--commit") ?? "HEAD"}^{commit}`], { cwd: repo }).trim();
    const archive = spawnSync("git", ["archive", "--format=tar", commit], { cwd: repo, maxBuffer: 2 * 1024 * 1024 * 1024 });
    if (archive.status !== 0) throw new Error(`git archive ${commit} failed: ${String(archive.stderr).trim()}`);
    const untar = spawnSync("tar", ["-x", "-f", "-", "-C", root], { input: archive.stdout, maxBuffer: 64 * 1024 * 1024 });
    if (untar.status !== 0) throw new Error(`tar could not extract ${commit}: ${String(untar.stderr).trim()}`);
  } catch (error) {
    err(`preflight: could not export the commit to check: ${error.message}`);
    if (root) fs.rmSync(root, { recursive: true, force: true });
    return 2;
  }
  try {
    const files = git(["ls-tree", "-r", "-z", "--name-only", commit], { cwd: repo }).split("\0").filter(Boolean);
    const { failures } = runRules({ root, commit, files, repo, only });
    for (const f of failures) {
      out(`FAIL ${f.rule} (${f.gap}): ${f.problem}`);
      if (f.fix) out(`     fix: ${f.fix}`);
    }
    const ruleCount = only ? only.length : RULES.length;
    const seconds = ((Date.now() - started) / 1000).toFixed(1);
    if (failures.length) {
      out(`preflight: ${failures.length} problem(s) in ${commit.slice(0, 12)} (${ruleCount} rules, ${seconds}s). Each would refuse later in CI or publication; fix and commit, then push.`);
      return 1;
    }
    out(`preflight: ${commit.slice(0, 12)} passed ${ruleCount} rules in ${seconds}s`);
    return 0;
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(await main(process.argv.slice(2)));
}
