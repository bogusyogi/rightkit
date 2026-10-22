// The one deterministic-scan module (Adrian 2026-10-22, #833 #888 #824; architecture-v3 section 4):
// every scan that applies to a change runs here, before any artifact build, local or CI.
//
//   Class A: compile-free, executes no tests and no repository-supplied plugin or generator.
//   Class B: needs the compiler or runs tests or repository code (clippy, check, doc, tsc, eslint,
//            package tests, the repository's own docs generator).
//
// A required tool that is missing FAILS (status "fail", code TOOL_MISSING); a scan is never skipped.
// The one allowed gap is status "deferred": a class B scan that needs a local compile grant when the
// repository has none. A deferred scan is listed by name (result.deferred, scanDeferrals), is never a
// pass (result.ok and result.complete are false while any scan is deferred) and cannot become a SCAN_B receipt.
// Content scans run on the exported HEAD (contentRoot). Only a scan that inherently needs the live
// checkout uses liveRoot: `rightkit cargo` scans (the broker binds to the registered checkout), the
// tsc, eslint and package tests (need installed node_modules).
// Class B runs only when every selected class A scan passed (SCAN_B is distinct from SCAN_A and
// follows it). Rust commands go through `rightkit cargo`, JavaScript tools through `pnpm exec`.
// The runner takes injectable spawn/exists/readFile/readdir so tests execute nothing real. The
// result is a plain object (per-scan status, tool versions, scanSetDigest) usable as receipt content
// (packages/release/pipeline-receipts.mjs); it carries no authority by itself.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawn as spawnChild, spawnSync } from "./background-process.mjs";
import { checkAdvisoryExceptions } from "./advisory-exceptions.mjs";

export const SCAN_RESULT_SCHEMA = "rightkit.deterministic-scans/1";
export const SCAN_CLASS = Object.freeze({ A: "A", B: "B" });

/** Scan id prefixes (the text before the first ":") that belong to each class. */
export const CLASS_A_SCAN_PREFIXES = Object.freeze([
  "rustfmt", "node-check", "json-parse", "toml-parse", "yaml-parse", "ownership-catalog",
  "actionlint", "pnpm-lockfile", "pnpm-audit", "cargo-metadata", "cargo-deny", "cargo-audit", "cargo-package-list",
  "changelog-version", "license", "secret-scan", "path-policy", "advisory-exceptions",
]);
export const CLASS_B_SCAN_PREFIXES = Object.freeze([
  "clippy", "check", "check-all-features", "check-features", "doc", "tsc", "eslint", "swift-build", "test", "generated-docs",
]);

/**
 * Rollout switch for the CI integration (ci/gate.config.json: { "deterministicScans": { "enabled": true } }).
 * Absent or anything but boolean true means OFF. A disabled run is never scan success or qualification evidence.
 *
 * deferredBeforeEnable (these gate ENABLING the CI integration, not shipping the dormant code):
 *  - deferred doc:* scans have no CI path yet;
 *  - changed-path / base coverage for workflow-only and package-only changes in ci.yml (the hygiene job has no --base);
 *  - pinned provisioning of cargo-deny, cargo-audit and actionlint via right-git;
 *  - class B wiring: CI's compile and test steps do not yet run through the shared runner with class A evidence.
 */
export function deterministicScansEnabled(config) {
  return config?.deterministicScans?.enabled === true;
}

/** "A", "B" or null for an id such as "clippy:rightkit-fs". */
export function scanClassOf(id) {
  const prefix = String(id).split(":")[0];
  if (CLASS_A_SCAN_PREFIXES.includes(prefix)) return SCAN_CLASS.A;
  if (CLASS_B_SCAN_PREFIXES.includes(prefix)) return SCAN_CLASS.B;
  return null;
}

const sha256 = (value) => crypto.createHash("sha256").update(value).digest("hex");

/** Deterministic JSON: object keys sorted, no undefined, no non-finite numbers. */
export function canonicalJson(value) {
  if (value === null || typeof value === "string" || typeof value === "boolean") return JSON.stringify(value);
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new TypeError("canonicalJson: non-finite number");
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (typeof value === "object") {
    const keys = Object.keys(value).sort();
    for (const key of keys) if (value[key] === undefined) throw new TypeError(`canonicalJson: undefined at ${key}`);
    return `{${keys.map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
  }
  throw new TypeError(`canonicalJson: unsupported ${typeof value}`);
}

/** Digest of a scan set: the sorted (id, class, command) of every entry. Plan items and result rows agree. */
export function digestScanSet(entries) {
  const rows = entries.map((e) => [String(e.id), String(e.class), String(e.command)]).sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  return sha256(canonicalJson(rows));
}

// ---- command builders shared with scripts/ci/gate.mjs and packages/release/local-publish.mjs -------------

const packageFlags = (units) => [].concat(units).flatMap((unit) => ["-p", unit]);

/** `cargo clippy -p <units> [--message-format=...] --no-deps --all-targets <features> [--locked] -- -D warnings` (argv after `cargo`). */
export function clippyArgs(units, { features = [], messageFormat, noDeps = true, locked = false } = {}) {
  return ["clippy", ...packageFlags(units), ...(messageFormat ? [`--message-format=${messageFormat}`] : []), ...(noDeps ? ["--no-deps"] : []), "--all-targets", ...features, ...(locked ? ["--locked"] : []), "--", "-D", "warnings"];
}

/** `cargo check -p <unit> [--message-format=...] --all-targets <features> [--locked]`. */
export function checkArgs(unit, { features = [], messageFormat, locked = false } = {}) {
  return ["check", "-p", unit, ...(messageFormat ? [`--message-format=${messageFormat}`] : []), "--all-targets", ...features, ...(locked ? ["--locked"] : [])];
}

/** `cargo doc -p <unit> --no-deps --all-features --locked`; run it with RUSTDOCFLAGS=DOC_RUSTDOCFLAGS (CI). */
export function docArgs(unit) { return ["doc", "-p", unit, "--no-deps", "--all-features", "--locked"]; }
export const DOC_RUSTDOCFLAGS = "-D warnings";
/** The same check as rustdoc arguments (`cargo rustdoc ... -- -D warnings`), for the broker path. */
export const DOC_RUSTDOC_ARGS = Object.freeze(["-D", "warnings"]);

/** `cargo fmt --check` for one crate, or the whole workspace when `unit` is null. */
export function fmtCheckArgs(unit = null) { return unit ? ["fmt", "-p", unit, "--check"] : ["fmt", "--all", "--check"]; }

/** `cargo metadata` that validates manifests and the lock without resolving anything new. */
export function metadataArgs({ noDeps = true } = {}) { return ["metadata", ...(noDeps ? ["--no-deps"] : []), "--format-version", "1", "--locked"]; }

// ---- plan ---------------------------------------------------------------------------------------------

const posix = (p) => p.replaceAll("\\", "/");
// Crate and package directory names reach argv (and, on Windows, possibly a .cmd shim); accept only
// plain names. Any other name is refused: it becomes a failing path-policy scan and is never run.
const SAFE_UNIT_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const ABSOLUTE_PATH = /^(?:[A-Za-z]:|[\\/])/;
/** Normalized posix form of a changed path, or { refused } when it is absolute, escapes the root, or is unusable. */
function containedPath(raw, contentRoot) {
  const text = String(raw);
  if (text === "" || text.includes("\0")) return { refused: "empty or contains a NUL" };
  const unified = posix(text);
  if (ABSOLUTE_PATH.test(unified) || path.isAbsolute(text) || path.win32.isAbsolute(text)) return { refused: "absolute path" };
  const norm = path.posix.normalize(unified);
  if (norm === ".." || norm.startsWith("../")) return { refused: "escapes the content root" };
  const rel = path.relative(path.resolve(contentRoot), path.resolve(contentRoot, norm));
  if (rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)) return { refused: "escapes the content root" };
  return { path: norm };
}
const PNPM_NO_HOOKS_ENV = Object.freeze({ npm_config_ignore_pnpmfile: "true", npm_config_ignore_scripts: "true", pnpm_config_ignore_pnpmfile: "true", pnpm_config_ignore_scripts: "true" });
const CONFIG_CRATE_FILE = /(^|\/)Cargo\.(toml|lock)$/;
const JS_TOOLING_FILE = /(^|\/)(package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml)$/;
// Root files that change how EVERY crate (or package) builds: a change to one puts every unit in scope,
// so a dependency or toolchain update never reaches a push without local qualification.
const WORKSPACE_RUST_FILE = /^(Cargo\.(toml|lock)|rust-toolchain(\.toml)?|clippy\.toml|rustfmt\.toml|\.cargo\/.+|vendor\/.+)$/;
const WORKSPACE_JS_FILE = /^(package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|\.npmrc)$/;
const ESLINT_FILES = ["eslint.config.js", "eslint.config.mjs", ".eslintrc.json"];
const ROOT_LINT_FILES = [...ESLINT_FILES, "biome.json"];
const PROBES = Object.freeze({
  rustfmt: ["--version"], node: ["--version"], rightkit: ["cargo", "--version"], pnpm: ["--version"],
  actionlint: ["-version"], "right-git": ["--version"], swift: ["--version"],
});

function commandText(d) {
  if (d.internal) return `internal:${d.internal}`;
  const env = Object.keys(d.env ?? {}).sort().map((k) => `${k}=${d.env[k]} `).join("");
  return `${env}${d.tool} ${d.args.join(" ")}`.trim();
}

/**
 * The scan plan for a change: derived from the changed files plus the manifests they belong to.
 * `contentRoot` is a clean export of the commit (pure content checks); `liveRoot` is the checkout the
 * managed `rightkit cargo` and `pnpm` run in. A scan whose files did not change is not planned.
 */
export function planScans({
  changed = [], contentRoot, liveRoot = contentRoot, exists = fs.existsSync, readFile = (f) => fs.readFileSync(f, "utf8"),
  platform = process.platform, includeTests = true, localCompile = true,
  // The command prefix every cargo scan runs through: `rightkit cargo` on a managed host, plain `cargo` in CI.
  cargoCommand = ["rightkit", "cargo"],
  // Physical containment: a changed path whose real path leaves the real content root (a symlink) is refused.
  realpath = (p) => fs.realpathSync(p),
  // Bytes of a changed file for the input digest (defaults to readFile; the gates pass the raw reader).
  readBytes = readFile,
  // Directory listing, for the every-unit scope a workspace file change brings in.
  listDir = (d) => { try { return fs.readdirSync(d); } catch { return []; } },
  // Per-crate feature sets, shaped like ci/gate.config.json `rust` ({ [crate]: { featureSets, checkFeatureSets } }).
  crateFeatures = {},
  // Crates a workspace-wide Rust change (Cargo.lock, vendor/) actually reaches, computed by the caller from
  // the lock graph (scripts/ci/select.mjs). null keeps every crate in scope.
  rustScope = null,
} = {}) {
  if (!contentRoot) throw new TypeError("planScans: contentRoot is required");
  if (!Array.isArray(cargoCommand) || cargoCommand.length === 0 || !["rightkit", "cargo"].includes(cargoCommand[0]) || cargoCommand.slice(1).some((a) => a !== "cargo")) {
    throw new TypeError('planScans: cargoCommand must be ["rightkit", "cargo"] or ["cargo"]');
  }
  const refusals = []; const accepted = [];
  for (const raw of new Set(changed)) {
    const c = containedPath(raw, contentRoot);
    if (c.refused) refusals.push({ raw: String(raw), why: c.refused });
    else accepted.push(c.path);
  }
  const files = [];
  let realRoot = null; let realRootError = null;
  try { realRoot = realpath(contentRoot); } catch (e) { realRootError = e; }
  for (const f of new Set(accepted)) {
    const abs = path.join(contentRoot, f);
    if (!exists(abs)) continue;
    let real = null;
    try { if (!realRoot) throw realRootError ?? new Error("no real content root"); real = realpath(abs); } catch (e) { refusals.push({ raw: f, why: `real path cannot be resolved (${e.message})` }); continue; }
    const rel = path.relative(realRoot, real);
    if (rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)) { refusals.push({ raw: f, why: "real path (symlink) leaves the content root" }); continue; }
    files.push(f);
  }
  // Content digest of the input the scans cover: every accepted changed file's bytes plus every refused name.
  const bytesDigest = (f) => { try { return sha256(readBytes(path.join(contentRoot, f))); } catch { return "unreadable"; } };
  const inputDigest = sha256(canonicalJson({ files: [...files].sort().map((f) => [f, bytesDigest(f)]), refused: refusals.map((r) => r.raw).sort() }));
  const under = (prefix) => files.filter((f) => f.startsWith(prefix));
  let scans = []; const notes = [];
  const add = (d) => {
    const entry = { root: "live", env: {}, args: [], ...d };
    entry.command = commandText(entry);
    scans.push(entry);
  };
  const A = SCAN_CLASS.A; const B = SCAN_CLASS.B;
  const refuse = (what, why) => add({ id: `path-policy:${sha256(`${what}\0${why}`).slice(0, 12)}`, class: A, internal: "refuse", detail: `${what}: ${why}; refused, never run` });
  for (const r of refusals) refuse(`changed path ${JSON.stringify(r.raw)}`, r.why);
  const safeUnits = (names, kind) => names.filter((n) => {
    if (SAFE_UNIT_NAME.test(n)) return true;
    refuse(`${kind} directory name ${JSON.stringify(n)}`, "not a plain name");
    return false;
  });
  const cargo = (...args) => ({ tool: cargoCommand[0], args: [...cargoCommand.slice(1), ...args] });

  // ---- class A: content checks on the exported commit
  // A vendor/ file keeps upstream formatting only when vendor/upstream-files.json records it and its
  // bytes still match that upstream digest; any other vendor/ .rs (new, patched) is rustfmt-checked.
  let upstream = {};
  try { upstream = JSON.parse(readFile(path.join(contentRoot, "vendor/upstream-files.json"))).files ?? {}; } catch { /* no manifest: nothing exempt */ }
  const verbatimUpstream = (f) => f.startsWith("vendor/") && typeof upstream[f] === "string" && bytesDigest(f) === upstream[f];
  const rustFiles = files.filter((f) => f.endsWith(".rs") && !verbatimUpstream(f));
  const byEdition = new Map();
  for (const f of rustFiles) {
    let dir = path.dirname(path.join(contentRoot, f));
    let edition = "2021";
    while (dir.startsWith(path.normalize(contentRoot))) {
      const manifest = path.join(dir, "Cargo.toml");
      if (exists(manifest)) {
        const m = readFile(manifest).match(/^edition\s*=\s*"(\d{4})"/m);
        if (m) edition = m[1];
        break;
      }
      const parent = path.dirname(dir);
      if (parent === dir) break;
      dir = parent;
    }
    byEdition.set(edition, [...(byEdition.get(edition) ?? []), f]);
  }
  for (const [edition, group] of [...byEdition].sort((a, b) => (a[0] < b[0] ? -1 : 1))) {
    add({ id: `rustfmt:${edition}`, class: A, tool: "rustfmt", args: ["--check", "--edition", edition, ...group.sort()], root: "content" });
  }
  for (const f of files.filter((f) => /\.(mjs|cjs|js)$/.test(f) && !f.includes("node_modules")).sort()) {
    add({ id: `node-check:${f}`, class: A, tool: "node", args: ["--check", f], root: "content" });
  }
  const jsonFiles = files.filter((f) => f.endsWith(".json") && !f.includes("node_modules")).sort();
  if (jsonFiles.length) add({ id: "json-parse", class: A, internal: "json-parse", files: jsonFiles });
  const tomlFiles = files.filter((f) => f.endsWith(".toml") && !f.includes("node_modules")).sort();
  if (tomlFiles.length) add({ id: "toml-parse", class: A, internal: "toml-parse", files: tomlFiles });
  // The strict workflow YAML parser covers the subset GitHub workflows and actions use; other YAML is not parsed with it.
  const yamlFiles = files.filter((f) => /^\.github\/.+\.ya?ml$/.test(f)).sort();
  if (yamlFiles.length) add({ id: "yaml-parse", class: A, internal: "yaml-parse", files: yamlFiles });

  if (exists(path.join(contentRoot, "packages/release/rightkit-ownership.json")) && exists(path.join(contentRoot, "crates/rightkit-ownership/catalog/ownership-catalog.json"))) {
    add({ id: "ownership-catalog", class: A, internal: "ownership-catalog", files: ["packages/release/rightkit-ownership.json", "crates/rightkit-ownership/catalog/ownership-catalog.json"] });
  }
  const rustWide = files.some((f) => WORKSPACE_RUST_FILE.test(f));
  const jsWide = files.some((f) => WORKSPACE_JS_FILE.test(f));
  const wideCrates = rustWide ? listDir(path.join(contentRoot, "crates")).filter((n) => !rustScope || rustScope.has(n)) : [];
  if (rustWide && rustScope) notes.push(`workspace Rust change reaches ${wideCrates.length} crate(s) through the lock graph`);
  const crateNames = safeUnits([...new Set([...under("crates/").map((f) => f.split("/")[1]), ...wideCrates])].filter((n) => exists(path.join(contentRoot, "crates", n, "Cargo.toml"))).sort(), "crate");
  const rustConfigChanged = files.some((f) => CONFIG_CRATE_FILE.test(f) || f === "deny.toml");
  const rustTouched = crateNames.length > 0 || rustConfigChanged;
  const pkgNames = safeUnits([...new Set([...under("packages/").map((f) => f.split("/")[1]), ...(jsWide ? listDir(path.join(contentRoot, "packages")) : [])])].filter((n) => exists(path.join(contentRoot, "packages", n, "package.json"))).sort(), "package");
  const jsTouched = pkgNames.length > 0 || files.some((f) => JS_TOOLING_FILE.test(f));

  // Explicit files: the exported commit has no .git for actionlint's project discovery.
  const workflowFiles = files.filter((f) => /^\.github\/workflows\/[^/]+\.ya?ml$/.test(f)).sort();
  const actionlintConfig = exists(path.join(contentRoot, ".github/actionlint.yaml")) ? ["-config-file", ".github/actionlint.yaml"] : [];
  if (workflowFiles.length) add({ id: "actionlint", class: A, tool: "actionlint", args: [...actionlintConfig, ...workflowFiles], root: "content" });
  // No right-git drift scan: the one workflow right-git renders here, ci.yml, is hand-owned (its header;
  // docs/CI.md) and held by scripts/ci/ci-scans-contract.mjs instead, so drift is always reported by design.

  // Exceptions expire by date, not by change: checked on every run, whatever changed.
  if (exists(path.join(contentRoot, "ci/advisory-exceptions.json"))) add({ id: "advisory-exceptions", class: A, internal: "advisory-exceptions" });
  if (rustTouched) {
    add({ id: "cargo-metadata", class: A, ...cargo(...metadataArgs()) });
    for (const name of crateNames) add({ id: `cargo-package-list:${name}`, class: A, ...cargo("package", "-p", name, "--list", "--allow-dirty", "--locked") });
  }
  if (rustConfigChanged) {
    add({ id: "cargo-deny", class: A, ...cargo("deny", "check") });
    add({ id: "cargo-audit", class: A, ...cargo("audit") });
  }
  if (jsTouched) {
    // Class A runs no repository hooks: `install` accepts --ignore-pnpmfile and --ignore-scripts; `audit` accepts
    // neither flag, so every class A pnpm scan also sets the same two settings through the environment.
    add({ id: "pnpm-lockfile", class: A, tool: "pnpm", args: ["install", "--frozen-lockfile", "--lockfile-only", "--ignore-pnpmfile", "--ignore-scripts"], env: PNPM_NO_HOOKS_ENV, root: "content" });
    add({ id: "pnpm-audit", class: A, tool: "pnpm", args: ["audit", "--prod", "--audit-level", "high"], env: PNPM_NO_HOOKS_ENV, root: "content" });
  }
  const crateDirs = crateNames.map((n) => `crates/${n}`); const pkgDirs = pkgNames.map((n) => `packages/${n}`);
  const withChangelog = crateNames.filter((n) => exists(path.join(contentRoot, "crates", n, "CHANGELOG.md")) && exists(path.join(contentRoot, "crates", n, "Cargo.toml")));
  for (const name of withChangelog) add({ id: `changelog-version:${name}`, class: A, internal: "changelog-version", files: [`crates/${name}/Cargo.toml`, `crates/${name}/CHANGELOG.md`] });
  if (crateDirs.length || pkgDirs.length) {
    add({ id: "license", class: A, internal: "license", files: [...crateDirs.map((d) => `${d}/Cargo.toml`), ...pkgDirs.map((d) => `${d}/package.json`)] });
    add({ id: "secret-scan", class: A, internal: "secret-scan", dirs: [...crateDirs, ...pkgDirs] });
  }

  // ---- class B: needs the compiler or runs tests or repository code
  // The docs generator is code. It runs only from the TRUSTED checkout (liveRoot), with the exported commit
  // handed to it as data (`--root`); the content root's own copy is never executed. When no separate
  // trusted checkout exists (liveRoot is contentRoot) or it lacks the generator, the scan is marked as
  // requiring a trusted-root run: deferred (never a pass), not run from submitted code.
  if (exists(path.join(contentRoot, "scripts/generate-index.mjs"))) {
    const trusted = path.resolve(liveRoot) !== path.resolve(contentRoot) && exists(path.join(liveRoot, "scripts/generate-index.mjs"));
    add({
      id: "generated-docs", class: B, tool: "node", args: ["scripts/generate-index.mjs", "--check", "--root", "{contentRoot}"],
      ...(trusted ? {} : { deferred: "requires a trusted-root run: the generator is repository code and no separate trusted checkout provides it; the content root's copy is never executed" }),
    });
  }
  for (const name of crateNames) {
    // Feature sets exactly as CI's gate runs them (scripts/ci/gate.mjs, ci/gate.config.json rust.<crate>):
    // clippy per `featureSets` (default features when absent); check per `checkFeatureSets` (default
    // --all-features), none when a lint set already uses --all-features. Some crates' features cannot
    // all be on together (rightkit-db sqlcipher vs sqlcipher-system; qa-driver wv2 generations).
    const own = crateFeatures?.[name] ?? {};
    const lintSets = Array.isArray(own.featureSets) && own.featureSets.length ? own.featureSets : [[]];
    // `cargo clippy --all-targets` type-checks exactly what `cargo check --all-targets` does: the default
    // check runs only when no clippy set covers default features.
    if (!lintSets.some((set) => set.length === 0)) add({ id: `check:${name}`, class: B, ...cargo(...checkArgs(name, { locked: true })) });
    const checkSets = lintSets.some((set) => set.includes("--all-features")) ? [] : (Array.isArray(own.checkFeatureSets) ? own.checkFeatureSets : [["--all-features"]]);
    checkSets.filter((set) => set.length).forEach((set, i) => add({ id: `check-features:${name}:${i}`, class: B, ...cargo(...checkArgs(name, { features: set, locked: true })) }));
    lintSets.forEach((set, i) => add({ id: lintSets.length === 1 ? `clippy:${name}` : `clippy:${name}:${i}`, class: B, ...cargo(...clippyArgs(name, { features: set, locked: true })) }));
    // rustdoc flags go after `--` for this crate's library only: the broker owns RUSTDOCFLAGS and refuses
    // a request that sets it. A crate with no library has no API docs to check.
    if (exists(path.join(contentRoot, "crates", name, "src", "lib.rs"))) {
      add({ id: `doc:${name}`, class: B, ...cargo("rustdoc", "-p", name, "--lib", ...lintSets[0], "--locked", "--", ...DOC_RUSTDOC_ARGS) });
    }
  }
  const rootLint = ROOT_LINT_FILES.some((f) => exists(path.join(liveRoot, f)));
  for (const name of pkgNames) {
    const dir = `packages/${name}`;
    if (exists(path.join(liveRoot, dir, "tsconfig.json"))) add({ id: `tsc:${name}`, class: B, tool: "pnpm", args: ["exec", "tsc", "--noEmit", "-p", "tsconfig.json"], cwd: dir });
    if (rootLint || ESLINT_FILES.some((f) => exists(path.join(liveRoot, dir, f)))) add({ id: `eslint:${name}`, class: B, tool: "pnpm", args: ["exec", "eslint", "."], cwd: dir });
  }
  if (files.some((f) => f.startsWith("swift/") || f === "Package.swift")) {
    if (platform === "darwin") add({ id: "swift-build", class: B, tool: "swift", args: ["build"] });
    else notes.push("swift files changed; `swift build` is required on the Mac host before its push");
  }
  if (includeTests) {
    // Tests per declared feature set, serially too where the crate asks for it, as CI's gate runs them.
    for (const name of crateNames) {
      const own = crateFeatures?.[name] ?? {};
      const sets = Array.isArray(own.featureSets) && own.featureSets.length ? own.featureSets : [[]];
      sets.forEach((set, i) => {
        const suffix = sets.length === 1 ? "" : `:${i}`;
        add({ id: `test:rust:${name}${suffix}`, class: B, ...cargo("test", "-p", name, ...set, "--locked", "--no-fail-fast") });
        if (own.alsoSerial) add({ id: `test:rust-serial:${name}${suffix}`, class: B, ...cargo("test", "-p", name, ...set, "--locked", "--no-fail-fast", "--", "--test-threads=1") });
      });
    }
    for (const name of pkgNames) {
      let manifest = null;
      try { manifest = JSON.parse(readFile(path.join(liveRoot, "packages", name, "package.json"))); } catch { /* unreadable manifest: no test row */ }
      if (manifest?.scripts?.test) add({ id: `test:npm:${name}`, class: B, tool: "pnpm", args: ["--dir", `packages/${name}`, "run", "test"] });
    }
    if (files.some((f) => f.startsWith("scripts/")) || exists(path.join(liveRoot, "scripts/ci"))) {
      add({ id: "test:scripts-ci", class: B, tool: "node", args: ["--test", "scripts/ci/*.test.mjs"] });
    }
  }

  // Without a local compile grant the managed broker path is closed: every broker scan (class A
  // deny/audit/package list included) stays in the plan marked `deferred`, never dropped, never a pass
  // and never run outside the broker. A local gate treats any deferral as a failure (#888).
  const deferred = [];
  if (!localCompile) {
    scans = scans.map((x) => {
      if (x.tool !== "rightkit" && !(x.tool === "cargo" && cargoCommand.length === 1)) return x;
      deferred.push(x.id);
      return { ...x, deferred: "no local compile grant; the broker refuses this scan" };
    });
    if (deferred.length) notes.push(`deferred, not passed (no local compile grant): ${deferred.join(", ")}`);
  }
  const ids = scans.map((s) => s.id);
  const dup = ids.find((id, i) => ids.indexOf(id) !== i);
  if (dup) throw new Error(`planScans: duplicate scan id ${dup}`);
  const classDigest = (cls) => digestScanSet(scans.filter((s) => s.class === cls));
  return {
    schema: SCAN_RESULT_SCHEMA, roots: { contentRoot, liveRoot }, changed: files, inputDigest, scans, notes, deferred,
    scanSetDigest: digestScanSet(scans), classDigests: { A: classDigest(A), B: classDigest(B) },
  };
}

// ---- runner -------------------------------------------------------------------------------------------

// An argument that may reach a .cmd/.bat shim (which cmd.exe parses) must be plain; anything else is rejected, never escaped.
const SAFE_SHIM_ARG = /^[A-Za-z0-9_.,:=@+\/\\-]+$/;
const SAFE_SHIM_PATH = /^[A-Za-z0-9_.,:=@+ \/\\()-]+$/;

/**
 * How to start `command` without a shell. POSIX: as given. Windows: resolve it on PATH here (never the
 * current directory), prefer .exe/.com, run an npm-style pnpm.cmd through node and its pnpm.cjs, and
 * run any other .cmd/.bat shim only when its path and every argument pass a strict allowlist.
 * Returns { file, args, verbatim } or { error }.
 */
export function resolveInvocation(command, args, { platform = process.platform, env = process.env, exists = fs.existsSync, execPath = process.execPath } = {}) {
  if (platform !== "win32") return { file: command, args, verbatim: false };
  const hasExt = /\.[A-Za-z0-9]{1,4}$/.test(command);
  const pathKey = Object.keys(env).find((k) => k.toLowerCase() === "path");
  const dirs = /[\\/]/.test(command) ? [""] : String(env[pathKey] ?? "").split(";").filter(Boolean);
  const exts = String(env.PATHEXT || ".COM;.EXE;.BAT;.CMD").split(";").filter(Boolean).map((e) => e.toLowerCase());
  let found = null;
  for (const dir of dirs) {
    const base = dir ? path.win32.join(dir, command) : command;
    for (const ext of hasExt ? [""] : exts) if (!found && exists(base + ext)) found = base + ext;
  }
  if (!found) return { error: Object.assign(new Error(`spawn ${command} ENOENT`), { code: "ENOENT" }) };
  if (!/\.(cmd|bat)$/i.test(found)) return { file: found, args, verbatim: false };
  const cjs = path.win32.join(path.win32.dirname(found), "node_modules", "pnpm", "bin", "pnpm.cjs");
  if (/^pnpm\.cmd$/i.test(path.win32.basename(found)) && exists(cjs)) return { file: execPath, args: [cjs, ...args], verbatim: false };
  const bad = args.find((a) => !SAFE_SHIM_ARG.test(a));
  if (bad !== undefined) return { error: new Error(`refusing to pass ${JSON.stringify(bad)} to the shell shim ${found}`) };
  if (!SAFE_SHIM_PATH.test(found)) return { error: new Error(`refusing to run shell shim ${found}: unsafe path`) };
  // Outer quotes around the whole line: with /s cmd strips exactly one leading and one trailing quote.
  return { file: env.ComSpec || "cmd.exe", args: ["/d", "/s", "/c", `""${found}"${args.map((a) => ` ${a}`).join("")}"`], verbatim: true };
}

/** Default spawn: synchronous, no console window, never `shell: true`. */
export function defaultSpawn(command, args, options = {}) {
  const inv = resolveInvocation(command, args, { env: options.env ?? process.env });
  if (inv.error) return { status: null, stdout: "", stderr: "", error: inv.error };
  return spawnSync(inv.file, inv.args, { encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024, windowsVerbatimArguments: inv.verbatim, ...options, shell: false });
}

const OUTPUT_KEEP = 4 * 1024 * 1024;
/** The same invocation as defaultSpawn without blocking, so scans can run side by side. Keeps the last 4 MiB of each stream. */
export function defaultSpawnAsync(command, args, options = {}) {
  const inv = resolveInvocation(command, args, { env: options.env ?? process.env });
  if (inv.error) return Promise.resolve({ status: null, stdout: "", stderr: "", error: inv.error });
  return new Promise((resolve) => {
    let stdout = ""; let stderr = ""; let error = null;
    const keep = (buf, chunk) => { const next = buf + chunk; return next.length > OUTPUT_KEEP ? next.slice(-OUTPUT_KEEP) : next; };
    let child;
    try {
      child = spawnChild(inv.file, inv.args, { windowsHide: true, windowsVerbatimArguments: inv.verbatim, ...options, shell: false, stdio: ["ignore", "pipe", "pipe"] });
    } catch (e) { resolve({ status: null, stdout: "", stderr: "", error: e }); return; }
    child.stdout.setEncoding("utf8"); child.stderr.setEncoding("utf8");
    child.stdout.on("data", (c) => { stdout = keep(stdout, c); });
    child.stderr.on("data", (c) => { stderr = keep(stderr, c); });
    child.on("error", (e) => { error = e; });
    child.on("close", (status) => resolve({ status, stdout, stderr, ...(error ? { error } : {}) }));
  });
}

// Lanes for concurrent class B runs. The broker admits check and clippy side by side; rustdoc and
// package are serialized by the broker anyway; tests and repository code share files, ports and
// devices, so they run one at a time, as on a CI runner.
function laneOf(scan) {
  if (/^(clippy|check|check-features):/.test(scan.id)) return "compile";
  if (/^doc:/.test(scan.id)) return "doc";
  return "serial";
}

const tail = (r) => `${r.stdout ?? ""}${r.stderr ?? ""}`.split(/\r?\n/).filter((l) => l.trim()).slice(-8).join(" | ");

/** rustfmt: $RUSTFMT, else the newest installed rustup toolchain, else PATH (null when none). */
export function findRustfmt({ env = process.env, exists = fs.existsSync, readdir = fs.readdirSync, platform = process.platform, home = os.homedir(), spawn = defaultSpawn } = {}) {
  if (env.RUSTFMT && exists(env.RUSTFMT)) return env.RUSTFMT;
  const exe = platform === "win32" ? "rustfmt.exe" : "rustfmt";
  const toolchains = path.join(env.RUSTUP_HOME || path.join(home, ".rustup"), "toolchains");
  if (exists(toolchains)) {
    const found = readdir(toolchains)
      .map((d) => ({ d, v: (d.match(/^(\d+)\.(\d+)(?:\.(\d+))?-/) || []).slice(1).map((n) => Number(n || 0)) }))
      .filter((t) => t.v.length && exists(path.join(toolchains, t.d, "bin", exe)))
      .sort((a, b) => b.v[0] - a.v[0] || b.v[1] - a.v[1] || b.v[2] - a.v[2]);
    if (found.length) return path.join(toolchains, found[0].d, "bin", exe);
  }
  const which = spawn(platform === "win32" ? "where" : "which", [exe]);
  return which && which.status === 0 ? String(which.stdout).split(/\r?\n/)[0].trim() : null;
}

async function defaultLoadToml() {
  const mod = await import("smol-toml");
  return mod.parse;
}
// Parsers come from this package's own trusted location (the runner's copy), never from the content root,
// which is submitted code. `smol-toml` resolves through this package's own dependencies.
const TRUSTED_YAML_PARSER = fileURLToPath(new URL("../../scripts/ci/workflow-yaml.mjs", import.meta.url));
async function defaultLoadYaml() {
  if (!fs.existsSync(TRUSTED_YAML_PARSER)) throw new Error(`strict YAML parser not found at ${TRUSTED_YAML_PARSER}`);
  return (await import(pathToFileURL(TRUSTED_YAML_PARSER).href)).parseYaml;
}
async function defaultScanSecrets() {
  return (await import("./publish-cargo.mjs")).scanCrateForSecrets;
}

const INTERNAL = {
  "json-parse": (s, ctx) => {
    const bad = [];
    for (const f of s.files) {
      try { JSON.parse(ctx.read(f).replace(/^﻿/, "")); } catch (e) { bad.push(`${f}: ${e.message}`); }
    }
    return bad;
  },
  "toml-parse": async (s, ctx) => {
    let parse;
    try { parse = await ctx.deps.loadToml(); } catch (e) { return { missing: `TOML parser unavailable: ${e.message}` }; }
    const bad = [];
    for (const f of s.files) { try { parse(ctx.read(f)); } catch (e) { bad.push(`${f}: ${String(e.message).split("\n")[0]}`); } }
    return bad;
  },
  "yaml-parse": async (s, ctx) => {
    let parse;
    try { parse = await ctx.deps.loadYaml(); } catch (e) { return { missing: `YAML parser unavailable: ${e.message}` }; }
    const bad = [];
    for (const f of s.files) { try { parse(ctx.read(f)); } catch (e) { bad.push(`${f}: ${e.message}`); } }
    return bad;
  },
  refuse: (s) => [s.detail],
  // Every RustSec exception is dated, scoped to the locked versions and identical in deny.toml and
  // .cargo/audit.toml (ci/advisory-exceptions.json; Astra 2026-10-22). Expiry fails every run.
  "advisory-exceptions": (s, ctx) => checkAdvisoryExceptions({ root: ctx.contentRoot, readFile: ctx.read }),
  "ownership-catalog": (s, ctx) => (ctx.read(s.files[0]) === ctx.read(s.files[1]) ? [] : ["copy packages/release/rightkit-ownership.json to crates/rightkit-ownership/catalog/ownership-catalog.json"]),
  "changelog-version": (s, ctx) => {
    const version = (ctx.read(s.files[0]).match(/^version\s*=\s*"([^"]+)"/m) || [])[1];
    if (!version) return [];
    const heading = new RegExp(`^##\\s+\\[?${version.replace(/\./g, "\\.")}\\]?(\\s|$)`, "m");
    return heading.test(ctx.read(s.files[1])) ? [] : [`${s.files[1]} has no "## ${version}" heading`];
  },
  license: (s, ctx) => {
    const bad = [];
    for (const f of s.files) {
      const text = ctx.read(f);
      if (f.endsWith("Cargo.toml")) {
        if (!/^\s*(license|license-file)\s*(=|\.workspace)/m.test(text)) bad.push(`${f}: no license or license-file`);
      } else {
        try { const m = JSON.parse(text); if (m.private !== true && !(typeof m.license === "string" && m.license.trim()) && !m.licenses) bad.push(`${f}: no license field`); } catch (e) { bad.push(`${f}: ${e.message}`); }
      }
    }
    return bad;
  },
  "secret-scan": async (s, ctx) => {
    const scan = await ctx.deps.scanSecrets();
    const bad = [];
    for (const dir of s.dirs) {
      const abs = path.join(ctx.contentRoot, dir);
      if (!ctx.deps.exists(abs)) { bad.push(`${dir}: directory not found`); continue; }
      for (const v of scan(abs)) bad.push(`${dir}: ${v}`);
    }
    return bad;
  },
};

/**
 * Run a plan. Options: classes (default ["A","B"]), exclude(scan) => boolean (an explicit, recorded
 * omission such as a caller that runs its own tests), plus the injectable spawn/exists/readFile/readdir/env.
 * Class B runs only when every selected class A scan passed; otherwise each B scan is "not-run".
 */
export async function runScans(plan, options = {}) {
  const deps = {
    // Concurrent runs need a spawn that does not block the event loop.
    spawn: Number(options.jobs ?? 1) > 1 ? defaultSpawnAsync : defaultSpawn, exists: fs.existsSync, readFile: (f) => fs.readFileSync(f, "utf8"), readdir: fs.readdirSync,
    env: process.env, platform: process.platform, execPath: process.execPath,
    loadToml: defaultLoadToml, loadYaml: defaultLoadYaml, scanSecrets: defaultScanSecrets, findRustfmt,
    ...options,
  };
  const classes = options.classes ?? [SCAN_CLASS.A, SCAN_CLASS.B];
  const exclude = options.exclude ?? (() => false);
  const { contentRoot, liveRoot } = plan.roots;
  const rootDir = (r) => (r === "content" ? contentRoot : r === "parent" ? path.dirname(contentRoot) : liveRoot);
  const selected = plan.scans.filter((s) => classes.includes(s.class) && !exclude(s));
  const ordered = [...selected.filter((s) => s.class === SCAN_CLASS.A), ...selected.filter((s) => s.class === SCAN_CLASS.B)];
  const ctx = { deps, contentRoot, read: (f) => deps.readFile(path.join(contentRoot, f)) };
  const toolVersions = {}; const probed = new Map();
  const results = [];
  let classAFailed = false;
  // Class B needs class A first: in this call (classes includes A) or valid prior class A evidence for the
  // same class A scan set (options.priorClassA: an earlier runScans result whose class A rows all passed).
  const priorA = options.priorClassA;
  const priorRows = Array.isArray(priorA?.scans) ? priorA.scans.filter((r) => r.class === SCAN_CLASS.A) : [];
  // Prior evidence binds to the input content (inputDigest of the changed files), the class A scan set, and a
  // class A run that actually included class A.
  const priorClassAValid = Boolean(priorA) && priorA.schema === SCAN_RESULT_SCHEMA && Boolean(plan.classDigests?.A) && Boolean(plan.inputDigest)
    && priorA.inputDigest === plan.inputDigest && Array.isArray(priorA.classes) && priorA.classes.includes(SCAN_CLASS.A)
    && priorA.classDigests?.A === plan.classDigests.A && digestScanSet(priorRows) === plan.classDigests.A && priorRows.every((r) => r.status === "pass");
  // Class A ran "in this call" only when it was selected, the plan has class A scans, and none was excluded:
  // excluding every class A scan (or a plan with none) never unlocks class B; explicit prior evidence is needed.
  const planA = plan.scans.filter((x) => x.class === SCAN_CLASS.A);
  const inCallA = classes.includes(SCAN_CLASS.A) && planA.length > 0 && selected.filter((x) => x.class === SCAN_CLASS.A).length === planA.length;
  const classBBlocked = !inCallA && !priorClassAValid;

  const toolCommand = (tool) => (tool === "node" ? deps.execPath : tool === "rustfmt" ? deps.findRustfmt({ env: deps.env, exists: deps.exists, readdir: deps.readdir, platform: deps.platform, spawn: deps.spawn }) : tool);
  async function probe(tool) {
    if (probed.has(tool)) return probed.get(tool);
    let outcome;
    const command = toolCommand(tool);
    if (!command) outcome = { ok: false, why: "not found" };
    else {
      const r = await deps.spawn(command, PROBES[tool] ?? ["--version"], { cwd: liveRoot, env: deps.env });
      if (!r || r.error || r.status !== 0) outcome = { ok: false, why: r?.error?.message ?? `exit ${r?.status}` };
      else outcome = { ok: true, command, version: String(r.stdout || r.stderr || "").split(/\r?\n/).find((l) => l.trim())?.trim() ?? "unknown" };
    }
    toolVersions[tool] = outcome.ok ? outcome.version : null;
    probed.set(tool, outcome);
    return outcome;
  }

  const jobs = Math.max(1, Number(options.jobs ?? 1) || 1);
  const onResult = typeof options.onResult === "function" ? options.onResult : () => {};
  const total = ordered.length; let done = 0;
  async function runOne(s) {
    const started = Date.now();
    const row = await scanRow(s);
    done += 1;
    try { onResult(row, { done, total, seconds: Math.round((Date.now() - started) / 1000) }); } catch { /* reporting never fails a scan */ }
    return row;
  }
  // Class A in order (a failure stops class B); then class B, concurrently by lane when jobs > 1.
  const aScans = ordered.filter((s) => s.class === SCAN_CLASS.A);
  const bScans = ordered.filter((s) => s.class === SCAN_CLASS.B);
  for (const s of aScans) results.push(await runOne(s));
  if (jobs === 1) {
    for (const s of bScans) results.push(await runOne(s));
  } else {
    const slots = new Array(bScans.length);
    const lanes = { compile: [], doc: [], serial: [] };
    bScans.forEach((s, i) => lanes[laneOf(s)].push(i));
    const drain = async (queue) => { while (queue.length) { const i = queue.shift(); slots[i] = await runOne(bScans[i]); } };
    await Promise.all([
      ...Array.from({ length: jobs }, () => drain(lanes.compile)),
      drain(lanes.doc),
      drain(lanes.serial),
    ]);
    results.push(...slots);
  }

  async function scanRow(s) {
    const row = { id: s.id, class: s.class, command: s.command, tool: s.tool ?? null, toolVersion: null, status: "fail" };
    if (s.class === SCAN_CLASS.B && classBBlocked) {
      return { ...row, status: "not-run", code: "CLASS_A_REQUIRED", detail: "class B refused: class A did not run in this call and no valid prior class A evidence for the same scan set was supplied" };
    }
    if (s.class === SCAN_CLASS.B && classAFailed) {
      return { ...row, status: "not-run", detail: "a class A scan failed; class B starts only after class A passes" };
    }
    let status = "pass"; let detail = ""; let code;
    if (s.deferred) {
      // A class A scan that did not run is not a class A pass: class B waits.
      if (s.class === SCAN_CLASS.A) classAFailed = true;
      return { ...row, status: "deferred", detail: s.deferred };
    }
    try {
      if (s.internal) {
        const out = await INTERNAL[s.internal](s, ctx);
        if (out && out.missing) { status = "fail"; code = "TOOL_MISSING"; detail = out.missing; }
        else if (out.length) { status = "fail"; detail = out.join("; "); }
      } else {
        const p = await probe(s.tool);
        row.toolVersion = p.ok ? p.version : null;
        if (!p.ok) { status = "fail"; code = "TOOL_MISSING"; detail = `required tool ${s.tool} is missing or not runnable (${p.why}); a required scan is never skipped`; }
        else {
          const r = await deps.spawn(p.command, s.args.map((a) => (a === "{contentRoot}" ? contentRoot : a)), { cwd: path.join(rootDir(s.root), s.cwd ?? ""), env: { ...deps.env, ...s.env } });
          if (!r || r.error) { status = "fail"; code = "SPAWN_ERROR"; detail = r?.error?.message ?? "no result"; }
          else if (r.status !== 0) { status = "fail"; code = "EXIT"; detail = tail(r) || `exit ${r.status}`; }
        }
      }
    } catch (e) { status = "fail"; code = "SCAN_ERROR"; detail = e.message; }
    if (status === "fail" && s.class === SCAN_CLASS.A) classAFailed = true;
    return { ...row, status, ...(code ? { code } : {}), ...(detail ? { detail } : {}) };
  }

  return {
    schema: SCAN_RESULT_SCHEMA, ok: results.every((r) => r.status === "pass"), complete: results.every((r) => r.status === "pass"), classes,
    inputDigest: plan.inputDigest ?? null,
    deferred: results.filter((r) => r.status === "deferred").map((r) => r.id),
    scans: results, toolVersions, notes: plan.notes ?? [],
    scanSetDigest: digestScanSet(results),
    classDigests: { A: digestScanSet(results.filter((r) => r.class === "A")), B: digestScanSet(results.filter((r) => r.class === "B")) },
  };
}

/** One line per scan that failed or did not run, for a gate's failure list. A deferred scan is not a failure; see scanDeferrals. */
export function scanFailures(result) {
  return result.scans.filter((r) => r.status !== "pass" && r.status !== "deferred").map((r) => `${r.id}${r.status === "not-run" ? " (not run)" : ""}: ${r.detail ?? r.status}`);
}

/** One line per deferred scan, by name. A gate prints these; they are never reported as passed. */
export function scanDeferrals(result) {
  return result.scans.filter((r) => r.status === "deferred").map((r) => `${r.id}: deferred, not passed (${r.detail})`);
}
