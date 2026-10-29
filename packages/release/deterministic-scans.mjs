// The one deterministic-scan module (Adrian 2026-10-22, #833 #888 #824; architecture-v3 section 4):
// every scan that applies to a change runs here, before any artifact build, local or CI.
//
//   Class A: compile-free, executes no tests and no repository-supplied plugin or generator.
//   Class B: needs the compiler or runs tests or repository code (clippy, check, doc, tsc, eslint, biome,
//            package tests, the repository's own docs generator).
//
// A required tool that is missing FAILS (status "fail", code TOOL_MISSING); a scan is never skipped.
// The one allowed gap is status "deferred": a class B scan that needs a local compile grant when the
// repository has none. A deferred scan is listed by name (result.deferred, scanDeferrals), is never a
// pass (result.ok and result.complete are false while any scan is deferred) and cannot become a SCAN_B receipt.
// Content scans run on the exported HEAD (contentRoot). Only a scan that inherently needs the live
// checkout uses liveRoot: `rustright cargo` scans (the broker binds to the registered checkout), the
// tsc, eslint, biome and package tests (need installed node_modules).
// Class B runs only when every selected class A scan passed (SCAN_B is distinct from SCAN_A and
// follows it). Rust commands go through `rustright cargo`, JavaScript tools through `pnpm exec`.
// The runner takes injectable spawn/exists/readFile/readdir so tests execute nothing real. The
// result is a plain object (per-scan status, tool versions, scanSetDigest) usable as receipt content
// (packages/release/pipeline-receipts.mjs); it carries no authority by itself.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawn as spawnChild, spawnSync } from "./background-process.mjs";
import { spawnUnderOwnership } from "./build-ownership.mjs";
import { checkAdvisoryExceptions } from "./advisory-exceptions.mjs";
import { canonicalJson, digestScanSet, requiredScanIds, rustCratePlan, rustScanId } from "./scan-digest.mjs";
import { reach } from "./lock-graph.mjs";
import { cargoManifestState } from "./cargo-manifest-text.mjs";
import { LAUNCH_GUARD_IMPORT } from "./class-a-launch-guard.mjs";
import { advisoryCacheRoot, ensureAdvisoryDb, prepareDenyOverlay, readAdvisoryPin } from "./advisory-db.mjs";
import { observedEnvOf, readRequestRef, verifySnapshotReceipt, writeSnapshotRequest } from "./snapshot-request.mjs";

export const SCAN_RESULT_SCHEMA = "rightkit.deterministic-scans/1";
/**
 * The checker that produced a scan result: `<name>@<version>#<digest>` where the digest covers this module and scan-digest.mjs,
 * so a changed checker at the same version is a different identity. null when it cannot be computed; prior class A evidence
 * then never unlocks class B (an unknown checker is not trusted, it is refused).
 */
export const CHECKER_IDENTITY = (() => {
  try {
    const p = JSON.parse(fs.readFileSync(new URL("./package.json", import.meta.url), "utf8"));
    const h = crypto.createHash("sha256");
    for (const f of ["./deterministic-scans.mjs", "./scan-digest.mjs"]) { h.update(f); h.update("\0"); h.update(fs.readFileSync(new URL(f, import.meta.url))); h.update("\0"); }
    return `${p.name}@${p.version}#${h.digest("hex").slice(0, 16)}`;
  } catch { return null; }
})();
export const CHECKER_VERSION = CHECKER_IDENTITY;
export const SCAN_CLASS = Object.freeze({ A: "A", B: "B" });

/** Scan id prefixes (the text before the first ":") that belong to each class. */
export const CLASS_A_SCAN_PREFIXES = Object.freeze([
  "rustfmt", "node-check", "json-parse", "toml-parse", "yaml-parse", "ownership-catalog",
  "actionlint", "shellcheck", "pnpm-lockfile", "pnpm-audit", "cargo-metadata", "cargo-deny", "cargo-audit", "cargo-package-list",
  "changelog-version", "license", "secret-scan", "path-policy", "advisory-exceptions", "suite-api",
  // Everything mechanical and deterministic that does not compile is class A (Adrian, 2026-10-27): typecheck, lint, generated docs.
  "tsc", "eslint", "biome", "generated-docs",
]);
// Class B includes compiles and tests that execute project code.
export const CLASS_B_SCAN_PREFIXES = Object.freeze([
  "clippy", "check", "check-all-features", "check-features", "check-dependent", "doc", "swift-build",
]);
// Test scripts can invoke a compiler indirectly, so all tests run after Class A.
export const TEST_SCAN_CLASS = Object.freeze({ "test:rust": "B", "test:rust-serial": "B", "test:npm": "B", "test:scripts-ci": "B" });

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

let neutralDir = null;
/** An empty directory outside every repository, for tool version probes. */
function probeDir() {
  if (!neutralDir || !fs.existsSync(neutralDir)) neutralDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-probe-"));
  return neutralDir;
}

/** JSONC to JSON: drops line and block comments outside strings, and trailing commas before } or ]. */
export function stripJsonComments(text) {
  let out = ""; let i = 0; let inString = false;
  while (i < text.length) {
    const c = text[i];
    if (inString) { out += c; if (c === "\\") { out += text[i + 1] ?? ""; i += 2; continue; } if (c === "\"") inString = false; i++; continue; }
    if (c === "\"") { inString = true; out += c; i++; continue; }
    if (c === "/" && text[i + 1] === "/") { while (i < text.length && text[i] !== "\n") i++; continue; }
    if (c === "/" && text[i + 1] === "*") { i += 2; while (i < text.length && !(text[i] === "*" && text[i + 1] === "/")) i++; i += 2; continue; }
    out += c; i++;
  }
  return out.replace(/,(\s*[}\]])/g, "$1");
}

/** "A", "B" or null for an id such as "clippy:rightkit-fs". */
export function scanClassOf(id, scan = null) {
  const parts = String(id).split(":");
  const prefix = parts[0];
  if (prefix === "test") {
    const kind = TEST_SCAN_CLASS[`test:${parts[1]}`] ?? null;
    if (kind === "A" && scan?.brokerCall) return SCAN_CLASS.B;
    return kind === null ? null : SCAN_CLASS[kind];
  }
  if (CLASS_A_SCAN_PREFIXES.includes(prefix)) return SCAN_CLASS.A;
  if (CLASS_B_SCAN_PREFIXES.includes(prefix)) return SCAN_CLASS.B;
  return null;
}

const sha256 = (value) => crypto.createHash("sha256").update(value).digest("hex");

// canonicalJson and digestScanSet live in the dependency-free scan-digest.mjs, so credentialed CI code that verifies
// scan receipts never loads this scanner.
export { canonicalJson, digestScanSet, requiredScanIds };

// ---- gate result cache --------------------------------------------------------------------------------
// A passing class B scan is reused on a later run only when every input is identical: the key is the sha256 of the
// canonical JSON of the scan id, its command text, the probed tool version, the environment the scan sets, a digest of the full environment its child inherits and an input
// digest the caller computes from the gated commit's object ids (bin/rightkit-gate.mjs). Only "pass" rows are stored
// (never fail, deferred or not-run). Class A scans are cached too, except the live checks whose answer depends on the day
// (cargo-deny, cargo-audit, pnpm-audit, pnpm-lockfile: the caller's digest returns null for them). A changed input changes the key, so
// there is no separate invalidation; a corrupt or foreign file is a miss.

// The environment names that can change what a build, lint, doc or test proves: compiler and cargo configuration, the toolchain,
// linkers and C toolchains, sccache, the RightKit/RustRight switches, CI, node and pnpm configuration. Nothing else the child
// inherits is part of the key: a session id, a socket path, a window handle or a temp directory differs from one process to the
// next and proves nothing, and keying on the whole environment made every cached result unreadable after a restart (2026-10-22).
// PATH is deliberately out: which tool runs is bound by the probed tool version in the key. RK_GATE_* (jobs, cache switches) and
// RUSTRIGHT_QUIET_LEASE (a lease id) do not change a scan's verdict.
const BUILD_ENV_NAMES = new Set(["CI", "GITHUB_ACTIONS", "CC", "CXX", "AR", "LD", "RANLIB", "RUSTC", "RUSTDOC", "RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
  "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "CARGO_HOME", "CARGO", "TARGET", "HOST", "NODE_OPTIONS", "NODE_ENV", "SOURCE_DATE_EPOCH", "MACOSX_DEPLOYMENT_TARGET", "SDKROOT", "DEVELOPER_DIR",
  "VCINSTALLDIR", "LIB", "INCLUDE", "LIBPATH", "CFLAGS", "CXXFLAGS", "LDFLAGS", "CPPFLAGS", "PKG_CONFIG_PATH", "PKG_CONFIG_LIBDIR", "OPENSSL_DIR", "OPENSSL_LIB_DIR", "OPENSSL_INCLUDE_DIR",
  "LIBCLANG_PATH", "LLVM_CONFIG_PATH", "ORT_LIB_LOCATION", "ORT_STRATEGY", "PROTOC", "WEBVIEW2_", "TAURI_"]);
const BUILD_ENV_PREFIXES = ["CARGO_", "RUST", "SCCACHE_", "RK_", "RIGHTKIT_", "RUSTRIGHT_", "PNPM_", "NPM_CONFIG_", "CMAKE_", "CFLAGS_", "CXXFLAGS_", "OPENSSL_", "ORT_", "ONNX", "TAURI_", "WEBVIEW2_", "WINDOWS_SDK", "MSVC"];
const BUILD_ENV_EXCLUDED = /^(RK_GATE_|RUSTRIGHT_QUIET_LEASE$)/;
export const buildEnvName = (name) => {
  const upper = String(name).toUpperCase();
  if (BUILD_ENV_EXCLUDED.test(upper)) return false;
  return BUILD_ENV_NAMES.has(upper) || BUILD_ENV_PREFIXES.some((prefix) => upper.startsWith(prefix));
};

/** sha256 of the build-relevant part (buildEnvName) of the environment a scan's child receives. Pass the merged `{ ...deps.env, ...scan.env }`. */
export function inheritedEnvDigest(env = {}) {
  const rows = Object.entries(env).filter(([name, value]) => value !== undefined && value !== null && buildEnvName(name)).map(([name, value]) => [String(name).toUpperCase(), String(value)]);
  rows.sort((x, y) => (x[0] < y[0] ? -1 : x[0] > y[0] ? 1 : 0));
  return sha256(canonicalJson(rows));
}

/**
 * The cache key for one scan; `digest` is the caller's input digest, `env` the scan's own environment and `inheritedEnv`
 * the digest of the whole environment the child receives (inheritedEnvDigest). A key without the inherited digest is refused.
 */
export function scanCacheKey({ id, command, toolVersion, env = {}, inheritedEnv, digest }) {
  if (typeof inheritedEnv !== "string" || !/^[0-9a-f]{64}$/.test(inheritedEnv)) throw new TypeError("scanCacheKey: inheritedEnv must be the inheritedEnvDigest of the child environment");
  return sha256(canonicalJson({ id: String(id), command: String(command), toolVersion: String(toolVersion), env: { ...env }, inheritedEnv, digest: String(digest) }));
}

const CACHE_KEY_SHAPE = /^[0-9a-f]{64}$/;

/** A cache directory (`<git dir>/rightkit-gate-cache`): { read(key) => entry | null, write(key, row) }. Never throws. */
export function fileScanCache(dir, { now = () => new Date(), fsApi = fs } = {}) {
  const file = (key) => (CACHE_KEY_SHAPE.test(key) ? path.join(dir, `${key}.json`) : null);
  return {
    read(key) {
      try { const f = file(key); return f ? JSON.parse(fsApi.readFileSync(f, "utf8")) : null; } catch { return null; }
    },
    write(key, row) {
      try {
        const f = file(key); if (!f) return;
        fsApi.mkdirSync(dir, { recursive: true });
        const tmp = `${f}.${process.pid}.${crypto.randomBytes(4).toString("hex")}.tmp`;
        fsApi.writeFileSync(tmp, JSON.stringify({ key, at: now().toISOString(), row }));
        fsApi.renameSync(tmp, f);
      } catch { /* a cache that cannot be written is only a miss next time */ }
    },
  };
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
// pnpm 11 checks, before every `pnpm run` and `pnpm exec`, that node_modules matches pnpm-lock.yaml (`verify-deps-before-run`, default
// `install`) and answers a stale tree by running `pnpm install` in it. That install wants to purge the modules directory and, with no TTY (a
// gate child has none), aborts: `[ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY] Aborted removal of modules directory due to no TTY` and a
// stack through runDepsStatusCheck, before the tool or the test runs at all (CodeRight, 2026-10-25: tsc, biome and test:npm failed in 2 s on a
// tree installed before the last lockfile change). A gate scan must not install into, or purge, the checkout it reads, and the lockfile is
// verified where it belongs, by `pnpm-lockfile` (`install --frozen-lockfile --lockfile-only`) in the clean export, so the class B pnpm scans
// turn the check off and run against the tree as installed, as pnpm 10 (default `false`) always did; a tree that is really missing something
// fails in the tool or the test with its own error. The `pnpm_config_` spelling is the one pnpm 11.25 honours for this setting
// (`npm_config_verify_deps_before_run=false` still aborted, checked on a stale tree), the lowercase spelling PNPM_NO_HOOKS_ENV uses for its settings.
const PNPM_RUN_ENV = Object.freeze({ pnpm_config_verify_deps_before_run: "false" });

// The placeholder a tsc row passes as its project when its package depends on a workspace sibling (planScans; runScans replaces it).
export const TSC_PROJECT = "{tscProject}";
const DIST_TYPES = /^\.\/dist\/(.+)\.d\.ts$/;
/**
 * Writes the project a tsc row checks through: it extends the package's own tsconfig (its include, exclude and options, resolved from
 * there), emits nothing, widens rootDir to the checked tree, keeps the package's own @types lookup chain (typeRoots), and maps every
 * exported entry point of each workspace sibling whose types
 * are built output (`types: ./dist/<x>.d.ts`) to that sibling's committed source (`src/<x>.ts`, `.tsx`, `/index.ts`, `/index.tsx`).
 * `workspace` is the siblings' tree-relative directories. Returns { dir, file }; the caller removes `dir`. Throws when a sibling's
 * manifest cannot be read or an exported entry point has no committed source: such a sibling could only be checked against a build.
 */
export function writeTscProject({ contentRoot, packageDir, workspace, tmpdir = os.tmpdir() }) {
  const paths = {};
  for (const rel of workspace) {
    const dir = path.join(contentRoot, rel);
    let manifest;
    try { manifest = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8").replace(/^﻿/, "")); }
    catch (e) { throw new Error(`${rel}/package.json: ${e.message}`); }
    if (typeof manifest.name !== "string" || path.resolve(dir) === path.resolve(packageDir)) continue;
    const exported = manifest.exports && typeof manifest.exports === "object" ? Object.entries(manifest.exports) : [];
    for (const [sub, target] of exported) {
      const types = target && typeof target === "object" ? target.types : null;
      const m = typeof types === "string" ? DIST_TYPES.exec(types) : null;
      if (!m) continue;
      const source = [`src/${m[1]}.ts`, `src/${m[1]}.tsx`, `src/${m[1]}/index.ts`, `src/${m[1]}/index.tsx`].map((f) => path.join(dir, f)).find((f) => fs.existsSync(f));
      if (!source) throw new Error(`${manifest.name} exports ${sub} with types ${types}, and ${rel}/src has no committed source for it`);
      paths[sub === "." ? manifest.name : `${manifest.name}${sub.slice(1)}`] = [source];
    }
  }
  // TypeScript looks type libraries up (`types`, and every @types package by default) in node_modules/@types of the CONFIG's directory
  // and its ancestors. The project lives outside the tree, so it names the chain the package's own tsconfig would have used.
  const typeRoots = [];
  for (let d = path.resolve(packageDir); ; d = path.dirname(d)) {
    const at = path.join(d, "node_modules", "@types");
    if (fs.existsSync(at)) typeRoots.push(at);
    if (path.dirname(d) === d) break;
  }
  const out = fs.mkdtempSync(path.join(tmpdir, "rk-tsc-"));
  const file = path.join(out, "tsconfig.json");
  fs.writeFileSync(file, `${JSON.stringify({ extends: path.join(packageDir, "tsconfig.json"), compilerOptions: { noEmit: true, rootDir: path.resolve(contentRoot), typeRoots, paths } }, null, 2)}\n`);
  return { dir: out, file };
}
const CONFIG_CRATE_FILE = /(^|\/)Cargo\.(toml|lock)$/;
const JS_TOOLING_FILE = /(^|\/)(package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml)$/;
// Root files that change how EVERY crate (or package) builds: a change to one puts every unit in scope,
// so a dependency or toolchain update never reaches a push without local qualification.
// `.cargo/audit.toml` is cargo-audit policy (an advisory ignore list), read by no compiler: it is ANCESTOR_DENY_FILE below.
export const WORKSPACE_RUST_FILE = /^(Cargo\.(toml|lock)|rust-toolchain(\.toml)?|\.?clippy\.toml|\.?rustfmt\.toml|\.cargo\/(?!audit\.toml$).+|vendor\/.+)$/;
// What cargo, clippy, rustfmt and cargo-deny read from EVERY ancestor directory of a workspace (cargo merges `.cargo/config*` and
// finds the toolchain file, clippy and rustfmt search upward): a change to one at any ancestor of a workspace, the workspace's own
// root included, puts that workspace's whole crate set in scope. Patterns are relative to the ancestor directory.
export const ANCESTOR_RUST_CONFIG_FILE = /^(rust-toolchain(\.toml)?|\.?clippy\.toml|\.?rustfmt\.toml|\.cargo\/(?!audit\.toml$).+)$/;
// cargo-deny reads deny.toml and cargo-audit reads .cargo/audit.toml from the workspace root and its ancestors, but neither changes
// compilation: a change anywhere above a workspace runs that workspace's metadata, cargo-deny and cargo-audit scans (the deny scan
// covers the workspace's whole crate set), not its compile or test scans.
export const ANCESTOR_DENY_FILE = /^(deny\.toml|\.cargo\/audit\.toml)$/;
/** The directories whose Rust configuration reaches workspace `ws`: "." (the repository root), every directory between, and `ws` itself. */
export const ancestorDirsOf = (ws) => {
  const parts = ws === "." ? [] : String(ws).split("/").filter(Boolean);
  return [".", ...parts.map((_, i) => parts.slice(0, i + 1).join("/"))];
};
/** True when a changed repo-relative file is Rust configuration of some ancestor directory (or the root) of workspace `ws`. */
export const ancestorConfigChanged = (ws, changed, pattern = ANCESTOR_RUST_CONFIG_FILE) => ancestorDirsOf(ws).some((dir) => changed.some((f) => {
  const rel = dir === "." ? f : f.startsWith(`${dir}/`) ? f.slice(dir.length + 1) : null;
  return rel !== null && pattern.test(rel);
}));
const WORKSPACE_JS_FILE = /^(package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|\.npmrc)$/;
// The linter a package has is the one it configures. ESLint reads a flat `eslint.config.*` or a legacy `.eslintrc*` from the package directory or
// any ancestor of it; Biome reads `biome.json` / `biome.jsonc` the same way. A Biome-only app (ViewRight, ScrapeRight: `"lint": "biome lint ."`)
// has no ESLint config and no ESLint dependency, so `eslint .` there fails with "couldn't find an eslint.config" (a gate false positive):
// planScans plans eslint only where an ESLint config exists and biome only where a Biome config exists, both where both do.
export const ESLINT_CONFIG_FILES = Object.freeze([
  "eslint.config.js", "eslint.config.mjs", ".eslintrc.json",
  "eslint.config.cjs", "eslint.config.ts", "eslint.config.mts", "eslint.config.cts",
  ".eslintrc", ".eslintrc.js", ".eslintrc.cjs", ".eslintrc.yaml", ".eslintrc.yml",
]);
export const BIOME_CONFIG_FILES = Object.freeze(["biome.json", "biome.jsonc"]);
// Everything scripts/generate-index.mjs reads or writes (its header and loadInventory): crate and package manifests and
// READMEs, the root Cargo.toml (workspace version), the versions and ownership data, the ownership-availability helper it imports, itself,
// the port matrix generator it imports, and its outputs (the port matrix's MD and HTML are checked for consistency with its JSON).
// The Right Suite API contract and every projection scripts/gen-suite-api.mjs writes from it (the test-vector copy included).
const SUITE_API_INPUT = /^(contracts\/right-suite-api\.v1\.json|scripts\/gen-suite-api\.mjs|crates\/rightkit-license\/src\/suite_api_generated\.rs|packages\/app-update\/src\/suite-api\.generated\.ts|packages\/release\/suite-api\.generated\.mjs|swift\/RightKitSwift\/Sources\/RightKitLicense\/RightSuiteAPI\.generated\.swift|crates\/rightkit-license\/test-vectors\/right-suite-api\.v1\.json)$/;
const GENERATOR_INPUT = /^(crates\/[^/]+\/(Cargo\.toml|README\.md)|packages\/[^/]+\/(package\.json|README\.md)|Cargo\.toml|packages\/release\/(rightkit-versions\.json|rightkit-ownership\.json|ownership-availability\.mjs)|scripts\/generate-(index|port-matrix)\.mjs|INDEX\.md|docs\/(ADOPTION|CAPABILITY-MAP|PORT-MATRIX)\.md|docs\/port-matrix\.(html|json))$/;
const PROBES = Object.freeze({
  rustfmt: ["--version"], node: ["--version"], rustright: ["cargo", "--version"], pnpm: ["--version"],
  actionlint: ["-version"], shellcheck: ["--version"], "right-git": ["--version"], swift: ["--version"],
});
/**
 * The one version of each analyzer whose verdict class A accepts. A class A finding must not depend on which build of the
 * analyzer a host happens to have: ShellCheck 0.10.0 fails SellRight's repaired deploy scripts on SC2015 and a documented
 * `# shellcheck disable=SC2329` on SC2317 (0.11.0 split that code), so the same commit was red on one host and green on another.
 * A scan (or a scan's `requires`) whose tool answers another version fails with TOOL_VERSION; it is never passed or skipped.
 * The installers (.github/workflows/publish.yml, publish-app.yml, ci.yml) and the managed host path in ci/rk-pipeline.json
 * provision exactly this version; scripts/ci/scan-receipt.test.mjs holds them to it.
 */
export const PINNED_SCAN_TOOLS = Object.freeze({ shellcheck: "0.11.0" });
/** Scans whose answer depends on a remote service at run time, not only on their inputs: `pnpm-audit` asks the registry's advisory
 * service. A verdict that includes them is reusable only after they are re-run (`rightkit-gate live-scans`; GATE-CONTRACT §1, E18),
 * and a red caused by one is never deterministic. Ids are `<prefix>` or `<prefix>:<root>`; rustright's REMOTE_SCANS mirrors this list. */
export const LIVE_SCAN_IDS = Object.freeze(["pnpm-audit"]);
export const isLiveScan = (id) => LIVE_SCAN_IDS.some((p) => id === p || String(id).startsWith(`${p}:`));
/** Scans whose FAILURE may come from the registry rather than their inputs: `pnpm-lockfile` (`install --frozen-lockfile --lockfile-only`)
 * re-verifies lockfile entries against the registry when pnpm's cache is cold, so an outage fails it. Their pass is deterministic, but a
 * red they cause is never a reusable deterministic verdict (rightkit#1257; Oracle review of 79c4427b). */
export const REGISTRY_SCAN_IDS = Object.freeze(["pnpm-lockfile"]);
export const isRegistryScan = (id) => REGISTRY_SCAN_IDS.some((p) => id === p || String(id).startsWith(`${p}:`));
/** Files per rustfmt scan: 120 paths of ~100 bytes stay under Windows' 32 KB command line with the tool and flags. */
export const RUSTFMT_BATCH = 120;

/** True when this module is the checked repository's own in-tree copy (packages/release of `contentRoot`), where its scripts/ live. */
export function inTreeGate(contentRoot, exists = fs.existsSync) {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const real = (p) => { try { return fs.realpathSync(p); } catch { return path.resolve(p); } };
  const own = real(path.resolve(here, "..", ".."));
  return own === real(contentRoot) && exists(path.join(own, "scripts", "gen-suite-api.mjs"));
}
// The version a probe's output names, for a tool whose first line is not its version (`shellcheck --version` opens with a banner).
const PROBE_VERSION = Object.freeze({
  shellcheck: (text) => { const m = /^version:\s*(\S+)\s*$/m.exec(text); return m ? `shellcheck ${m[1]}` : null; },
});
/** null when `version` (a probe's answer) is the pinned one or the tool has no pin, else the problem. */
export function pinnedVersionProblem(tool, version) {
  const required = PINNED_SCAN_TOOLS[tool];
  if (!required || version === `${tool} ${required}`) return null;
  return `${tool} found ${version ? `"${version}"` : "with no readable version"}, class A requires ${tool} ${required} (PINNED_SCAN_TOOLS in @rightkit/release deterministic-scans.mjs); another version judges scripts differently, so it is refused, never passed or skipped`;
}

function commandText(d) {
  if (d.internal) return `internal:${d.internal}`;
  const env = Object.keys(d.env ?? {}).sort().map((k) => `${k}=${d.env[k]} `).join("");
  return `${env}${d.tool} ${d.args.join(" ")}`.trim();
}

// ---- package scripts that call the broker -----------------------------------------------------------------------------------
// A package's `test` script may run Rust through the broker (`rustright storage run --app x -- right-release test`, `rustright cargo test`) or
// through plain cargo, which on a host with no broker means the unmanaged toolchain. On such a host the `test:npm:<unit>` scan cannot pass, and
// its failure says nothing about the code: rightkit-gate defers it with the Rust gate. scriptBrokerCall only reads the scripts; it runs nothing.
const SCRIPT_TEXT_LIMIT = 16 * 1024;
const BROKER_TOOLS = new Set(["rustright", "cargo"]);
const ENV_ASSIGNMENT = /^[A-Za-z_][A-Za-z0-9_]*=/;
// Words that run the command after them.
const COMMAND_WRAPPERS = new Set(["cross-env", "env", "time", "exec", "command", "sudo", "nice", "nohup"]);
const PACKAGE_MANAGERS = new Set(["pnpm", "npm", "yarn"]);
const PACKAGE_RUNNERS = new Set(["npx", "bunx"]);
// Package manager options that point the run at another package or at every package: the script that runs is not in this package.json.
const RETARGET_FLAGS = new Set(["-r", "--recursive", "-w", "--workspace-root", "--workspaces", "-ws", "--include-workspace-root"]);
const RETARGET_VALUE_OPTIONS = new Set(["-F", "--filter", "--filter-prod", "-C", "--dir", "--prefix", "--workspace"]);
const SAME_DIR_VALUES = new Set([".", "./"]);
// Options that take the next word as their value (the rest are flags).
const VALUE_OPTIONS = new Set(["--reporter", "--loglevel", "--color", "--workspace-concurrency", "--stream", "--shell-emulator", "--test-pattern"]);

/** The simple commands of a shell line, each as its words: split on `&&`, `||`, `;`, `|`, `&`, `(`, `)` and newlines outside quotes. A backslash outside quotes is a path separator (Windows), not an escape. */
function shellCommands(text) {
  const commands = []; let words = []; let word = ""; let inWord = false; let quote = null;
  const endWord = () => { if (inWord) words.push(word); word = ""; inWord = false; };
  const endCommand = () => { endWord(); if (words.length) commands.push(words); words = []; };
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    if (quote) {
      if (c === quote) quote = null;
      else if (c === "\\" && quote === '"' && i + 1 < text.length) word += text[++i];
      else word += c;
      continue;
    }
    if (c === '"' || c === "'") { quote = c; inWord = true; continue; }
    if (c === "&" || c === "|" || c === ";" || c === "\n" || c === "(" || c === ")") { endCommand(); continue; }
    if (/\s/.test(c)) { endWord(); continue; }
    word += c; inWord = true;
  }
  endCommand();
  return commands;
}

/** What `pnpm|npm|yarn <args>` does in this package: { run: <script name> } or {} (another package, a built-in, an unknown word). */
function inspectPackageManager(args, scripts) {
  let i = 0;
  while (i < args.length && args[i].startsWith("-") && args[i] !== "--") {
    const [name, ...inline] = args[i].split("=");
    const value = inline.length ? inline.join("=") : undefined;
    if (RETARGET_FLAGS.has(name)) return {};
    if (RETARGET_VALUE_OPTIONS.has(name)) {
      if (!SAME_DIR_VALUES.has(value ?? args[i + 1])) return {};
      i += value === undefined ? 2 : 1;
    } else i += VALUE_OPTIONS.has(name) && value === undefined ? 2 : 1;
  }
  const sub = args[i];
  if (sub === undefined || sub === "--") return {};
  const rest = args.slice(i + 1);
  if (sub === "exec" || sub === "dlx") return inspectCommand(rest, scripts);
  if (sub === "run" || sub === "run-script" || sub === "rum" || sub === "urun") {
    let j = 0;
    while (j < rest.length && rest[j].startsWith("-") && rest[j] !== "--") j += VALUE_OPTIONS.has(rest[j]) ? 2 : 1;
    return rest[j] !== undefined && rest[j] !== "--" ? { run: rest[j] } : {};
  }
  if (sub === "test" || sub === "t" || sub === "tst") return { run: "test" };
  return Object.hasOwn(scripts, sub) ? { run: sub } : {};
}

/** What one simple command does: { call: "rustright" | "cargo" } when it runs the tool (through env words and wrappers), { run: <script> } for a package script, else {}. */
function inspectCommand(words, scripts) {
  let i = 0;
  for (let guard = 0; guard < 16; guard += 1) {
    while (i < words.length && ENV_ASSIGNMENT.test(words[i])) i += 1;
    if (i >= words.length) return {};
    const word = path.posix.basename(words[i].replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "");
    if (BROKER_TOOLS.has(word)) return { call: word };
    if (PACKAGE_MANAGERS.has(word)) return inspectPackageManager(words.slice(i + 1), scripts);
    if (COMMAND_WRAPPERS.has(word) || PACKAGE_RUNNERS.has(word)) {
      i += 1;
      while (i < words.length && words[i].startsWith("-")) i += 1;
      continue;
    }
    return {};
  }
  return {};
}

/**
 * Whether the package script `entry` runs `rustright` or `cargo`, directly or through the scripts it starts in the same package.json
 * (`pnpm <script>`, `pnpm run <script>`, `npm run <script>`, `yarn [run] <script>`, `npm test`), their `pre<script>` and `post<script>` hooks, and
 * `exec`/`dlx`/`npx` wrappers. A run pointed at another package (`--filter`, `-r`, `--dir` elsewhere) is not followed. Bounded to `maxDepth` nested
 * scripts and cycle-safe. Returns `{ tool, via, command }` for the first call in execution order (`via` is the scripts walked, `command` the command
 * line that calls the tool), or null. Reads package.json `scripts` data only.
 */
export function scriptBrokerCall(scripts, entry = "test", { maxDepth = 8 } = {}) {
  if (scripts === null || typeof scripts !== "object" || Array.isArray(scripts)) return null;
  const textOf = (name) => (Object.hasOwn(scripts, name) && typeof scripts[name] === "string" ? scripts[name].slice(0, SCRIPT_TEXT_LIMIT) : null);
  const seen = new Set();
  const visit = (name, via) => {
    if (via.length >= maxDepth || seen.has(name)) return null;
    seen.add(name);
    for (const part of [`pre${name}`, name, `post${name}`]) {
      const text = textOf(part);
      if (text === null) continue;
      for (const words of shellCommands(text)) {
        const found = inspectCommand(words, scripts);
        if (found.call) return { tool: found.call, via: [...via, part], command: words.join(" ").slice(0, 200) };
        if (found.run !== undefined) {
          const deeper = visit(found.run, [...via, part]);
          if (deeper) return deeper;
        }
      }
    }
    return null;
  };
  return visit(String(entry), []);
}

/**
 * Whether the package script `entry` builds (emits) code before or while it tests: `tsc` without --noEmit, or a package-manager
 * run of a `build` / `build:*` script in this package or another (`pnpm run build`, `pnpm --filter <pkg> build`), followed through
 * the scripts it starts in the same package.json like scriptBrokerCall. Such a test is not a pre-compile check: it runs after
 * class A, with the compile scans (Codex reviewer 2026-10-28). Returns `{ via, command }` or null.
 */
export function scriptBuildCall(scripts, entry = "test", { maxDepth = 8 } = {}) {
  if (scripts === null || typeof scripts !== "object" || Array.isArray(scripts)) return null;
  const textOf = (name) => (Object.hasOwn(scripts, name) && typeof scripts[name] === "string" ? scripts[name].slice(0, SCRIPT_TEXT_LIMIT) : null);
  const builds = (words) => {
    let i = 0;
    while (i < words.length && (ENV_ASSIGNMENT.test(words[i]) || COMMAND_WRAPPERS.has(path.posix.basename(words[i])) || PACKAGE_RUNNERS.has(path.posix.basename(words[i])))) i += 1;
    const word = path.posix.basename(String(words[i] ?? "").replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "");
    if (word === "tsc") return !words.slice(i + 1).some((w) => /^--?noEmit(=true)?$/i.test(w));
    if (!PACKAGE_MANAGERS.has(word)) return false;
    // pnpm's `-w` is `--workspace-root` (a flag); npm's `-w` is `--workspace <name>` (takes a value).
    const rest = words.slice(i + 1); const values = new Set(word === "pnpm" ? ["--filter", "-F", "--dir", "-C"] : word === "npm" ? ["--workspace", "-w", "--prefix"] : ["--cwd", "--filter"]);
    for (let j = 0; j < rest.length; j += 1) {
      if (values.has(rest[j])) { j += 1; continue; }
      if (rest[j].startsWith("-") || rest[j] === "run" || rest[j] === "run-script") continue;
      return /^build(:|$)/.test(rest[j]);
    }
    return false;
  };
  const seen = new Set();
  const visit = (name, via) => {
    if (via.length >= maxDepth || seen.has(name)) return null;
    seen.add(name);
    for (const part of [`pre${name}`, name, `post${name}`]) {
      const text = textOf(part);
      if (text === null) continue;
      for (const words of shellCommands(text)) {
        if (builds(words)) return { via: [...via, part], command: words.join(" ").slice(0, 200) };
        const found = inspectCommand(words, scripts);
        if (found.run !== undefined) { const deeper = visit(found.run, [...via, part]); if (deeper) return deeper; }
      }
    }
    return null;
  };
  return visit(String(entry), []);
}

// Cargo redirections on a reachable command line or script: an explicit manifest or output directory, or a working-directory change.
const CARGO_REDIRECT = /--manifest-path|--target-dir|(^|[\s;&|(])(CARGO_TARGET_DIR|CARGO_BUILD_TARGET_DIR)=|(^|[\s;&|(])(cd|pushd)\s/;

// The forms a test's path to cargo may take for its target directory to be bound (cargoReachOf). Everything else is unprovable.
// Tools that run no repository code and start no cargo: any arguments.
const REACH_SAFE_TOOLS = new Set(["tsc", "eslint", "prettier", "biome", "oxlint", "echo", "true"]);
// cargo options that move its manifest, workspace or output, or change its configuration (a short-option cluster holding C or Z too).
const CARGO_REDIRECT_ARG = /^(--manifest-path|--target-dir|--config)(=|$)|^-[A-Za-z]*[CZ]/;
// The cargo subcommands a bound test may run; anything else (an alias, cargo make, xtask, install, a plugin) is unprovable.
const CARGO_REACH_SUBCOMMANDS = new Set(["test", "build", "check", "clippy"]);
// Package-manager built-ins: never one of the package's scripts, whatever its package.json names (npm pack runs pack, not a script).
const PM_BUILTINS = new Set(["install", "i", "add", "ci", "pack", "publish", "version", "link", "unlink", "rebuild", "prune", "update", "up", "upgrade", "uninstall", "remove", "rm", "init", "create", "exec", "dlx", "x", "workspace", "workspaces", "config", "set", "import", "patch", "deploy", "audit", "outdated", "why", "store", "cache", "env", "setup", "self-update", "fetch"]);
// Characters a shell gives meaning outside quotes beyond the separators shellCommands splits on: an escape, a glob, a home or
// redirection, a brace or history expansion, a comment, and cmd.exe's ^ and %VAR%. A script holding one unquoted is unprovable.
const UNQUOTED_SHELL_META = new Set(["\\", "*", "?", "[", "]", "~", "<", ">", "{", "}", "!", "#", "^", "%"]);
/** The first character a shell would read specially outside quotes in `text`, or null. */
function unquotedShellMeta(text) {
  let quote = null;
  for (const c of String(text)) {
    if (quote) { if (c === quote) quote = null; continue; }
    if (c === '"' || c === "'") { quote = c; continue; }
    if (UNQUOTED_SHELL_META.has(c)) return c;
  }
  return null;
}
// Package-manager options the walk resolves; any other option is unprovable.
const PM_DIR_OPTIONS = new Set(["--dir", "-C", "--prefix", "--cwd"]);
const PM_PACKAGE_OPTIONS = new Set(["--filter", "-F", "--filter-prod", "--workspace"]);
const PM_PLAIN_FLAGS = new Set(["-s", "--silent", "--if-present", "--color", "--no-color", "--stream"]);
const PM_VALUE_FLAGS = new Set(["--reporter", "--loglevel"]);

/**
 * Where a package's `test` script makes cargo run, for a commit-snapshot compile (joint plan step 4c): `{ none: true }` when it reaches
 * no cargo, `{ dir }` (repository-relative) when every cargo it reaches runs from that one directory, `{ dirs }` when from several (the
 * caller requires them to be one workspace), or `{ unprovable }` with the reason. One CARGO_TARGET_DIR binds the whole test.
 *
 * Fail-closed allowlist (root reviews of 431fbeae, f883d5ca, 15300d82): a test is bound only when every command on every path it
 * reaches (each script with its pre/post hooks) parses completely into one of these forms:
 *   - `cargo <args>` or `rustright cargo <args>` with no option that moves the manifest, workspace or output (--manifest-path,
 *     --target-dir, --config, -C, -Z): cargo runs from the script's package directory;
 *   - `pnpm|npm|yarn` with only resolvable options in any position before the script (--dir/-C/--prefix/--cwd, --filter/-F/
 *     --filter-prod/--workspace naming one package of this repository, pnpm -w, npm -w <workspace>, and output flags) running
 *     `run <script>`, `test`, `<script>` with no further arguments, or `exec <command>` (walked the same way in that package);
 *   - a tool that runs no repository code (tsc, eslint, prettier, biome, oxlint, echo, true).
 * Anything else is unprovable: `cd`, an environment assignment, a shell expansion, node or any test runner, an unknown command or
 * option, a recursive run, an unresolvable package, arguments passed on to a script, a script past the size limit, the depth limit,
 * and a requiresBroker declaration (the declared path is not in the scripts). Over-refusal is the accepted cost.
 */
export function cargoReachOf({ unitDir, scripts, declared = null, readScripts = () => null, packageDirOf = () => null, maxDepth = 8 }) {
  const dirs = new Set();
  // Keyed on package and script alone: the walk refuses everything that could change how a script runs (cd, environment), so a
  // script reached twice from one package runs the same way both times.
  const seen = new Set();
  const unprovable = (why) => { throw Object.assign(new Error(why), { reach: true }); };
  const walkScript = (dirScripts, name, dir, via) => {
    const key = `${dir}\0${name}`;
    if (seen.has(key)) return;
    if (via.length >= maxDepth) unprovable(`the path to ${dir}'s ${name} (${via.join(" -> ")}) is deeper than ${maxDepth} scripts`);
    seen.add(key);
    for (const part of [`pre${name}`, name, `post${name}`]) {
      if (!Object.hasOwn(dirScripts, part)) continue;
      const text = dirScripts[part];
      if (typeof text !== "string") unprovable(`${dir}'s ${part} script is not text`);
      if (text.length > SCRIPT_TEXT_LIMIT) unprovable(`${dir}'s ${part} script is longer than ${SCRIPT_TEXT_LIMIT} characters`);
      const meta = unquotedShellMeta(text);
      if (meta !== null) unprovable(`${dir}'s ${part} script has an unquoted ${JSON.stringify(meta)}, which a shell would expand or redirect`);
      const step = [...via, dir === unitDir ? part : `${dir}:${part}`];
      for (const words of shellCommands(text)) walkCommand(words, dirScripts, dir, step);
    }
  };
  const walkCommand = (words, dirScripts, dir, via) => {
    const where = `${via.join(" -> ")}`;
    const shown = words.join(" ").slice(0, 120);
    if (words.some((w) => /[`$]/.test(w))) unprovable(`${where} runs \`${shown}\`, whose shell expansion this check cannot follow`);
    if (ENV_ASSIGNMENT.test(words[0])) unprovable(`${where} runs \`${shown}\` with its own environment`);
    const tool = path.posix.basename(words[0].replaceAll("\\", "/")).replace(/\.(exe|cmd|bat)$/i, "");
    if (tool === "cargo" || tool === "rustright") {
      const args = tool === "rustright" ? (words[1] === "cargo" ? words.slice(2) : unprovable(`${where} runs \`${shown}\`, a rustright command other than cargo`)) : words.slice(1);
      const end = args.indexOf("--");
      const own = end === -1 ? args : args.slice(0, end);
      if (own.some((w) => CARGO_REDIRECT_ARG.test(w))) unprovable(`the path to cargo (${where}) redirects it`);
      const sub = own.find((w) => !w.startsWith("-") && !w.startsWith("+"));
      if (!CARGO_REACH_SUBCOMMANDS.has(sub)) unprovable(`${where} runs \`${shown}\`, a cargo command (${sub ?? "none"}) other than ${[...CARGO_REACH_SUBCOMMANDS].join(", ")}`);
      dirs.add(dir);
      return;
    }
    if (PACKAGE_MANAGERS.has(tool)) return walkPackageManager(tool, words.slice(1), dirScripts, dir, via, shown);
    if (REACH_SAFE_TOOLS.has(tool)) return;
    unprovable(`${where} runs \`${shown}\` (${tool}), which this check cannot follow to cargo`);
  };
  const walkPackageManager = (tool, args, dirScripts, dir, via, shown) => {
    const where = `${via.join(" -> ")}`;
    let target = dir;
    let i = 0;
    let retargets = 0;
    // Options, in any position before the script or command: each one resolved, or the command is unprovable. At most one of them
    // may name the package or directory (two would leave which one wins to the package manager).
    const options = () => {
      while (i < args.length && args[i].startsWith("-")) {
        const [name, ...inline] = args[i].split("=");
        const value = inline.length ? inline.join("=") : args[i + 1];
        const width = inline.length ? 1 : 2;
        if (PM_DIR_OPTIONS.has(name) || PM_PACKAGE_OPTIONS.has(name) || ((name === "-w" || name === "--workspace-root") && (tool === "pnpm" || tool === "npm"))) {
          retargets += 1;
          if (retargets > 1) unprovable(`${where}: \`${shown}\` names its package or directory more than once`);
        }
        if (PM_DIR_OPTIONS.has(name)) { if (value === undefined) unprovable(`${where}: \`${shown}\` names no directory`); target = path.posix.normalize(path.posix.join(dir, String(value))); i += width; }
        else if (PM_PACKAGE_OPTIONS.has(name)) { const d = packageDirOf(String(value)); if (d === null || d === undefined) unprovable(`${where}: \`${shown}\` runs a package (${value}) this repository cannot resolve`); target = d; i += width; }
        else if ((name === "-w" || name === "--workspace-root") && tool === "pnpm") { target = "."; i += 1; }
        else if ((name === "-w") && tool === "npm") { const d = packageDirOf(String(value)); if (d === null || d === undefined) unprovable(`${where}: \`${shown}\` runs a workspace (${value}) this repository cannot resolve`); target = d; i += width; }
        else if (PM_PLAIN_FLAGS.has(name) && !inline.length) i += 1;
        else if (PM_VALUE_FLAGS.has(name)) i += width;
        else unprovable(`${where}: \`${shown}\` has an option (${name}) this check does not resolve`);
      }
    };
    options();
    const sub = args[i++];
    if (sub === undefined) unprovable(`${where}: \`${shown}\` runs no script`);
    // Options after `run`/`exec` count as well: the target is known only once every option before the script has been read.
    if (sub === "run" || sub === "run-script" || (sub === "exec" && tool === "pnpm")) options();
    if (target.startsWith("..")) unprovable(`${where}: \`${shown}\` runs outside this repository (${target})`);
    const targetScripts = target === dir ? dirScripts : readScripts(target);
    if (targetScripts === null || targetScripts === undefined) unprovable(`${where}: \`${shown}\` runs scripts of ${target}, which has no readable package.json`);
    const hop = target === dir ? via : [...via, `${tool} ${args.join(" ")}`.slice(0, 120)];
    if (sub === "exec" && tool === "pnpm") {
      if (i >= args.length) unprovable(`${where}: \`${shown}\` executes nothing`);
      return walkCommand(args.slice(i), targetScripts, target, hop);
    }
    let script;
    if (sub === "run" || sub === "run-script") { script = args[i++]; if (script === undefined) unprovable(`${where}: \`${shown}\` names no script`); }
    else if (sub === "test" || sub === "t" || sub === "tst") script = "test";
    else if (!PM_BUILTINS.has(sub) && Object.hasOwn(targetScripts, sub)) script = sub;
    else unprovable(`${where}: \`${shown}\` is a ${tool} command (${sub}) this check cannot follow`);
    if (i < args.length) unprovable(`${where}: \`${shown}\` passes arguments to ${target}'s ${script}`);
    walkScript(targetScripts, script, target, hop);
  };
  if (declared?.call) return { unprovable: "it is declared to reach the broker in ways its scripts do not show (requiresBroker)" };
  try {
    walkScript(scripts ?? {}, "test", unitDir, []);
  } catch (e) {
    if (e.reach) return { unprovable: e.message };
    throw e;
  }
  const list = [...dirs];
  return list.length === 0 ? { none: true } : list.length === 1 ? { dir: list[0] } : { dirs: list };
}

// A test script can reach the broker in ways scriptBrokerCall cannot read from package.json alone: `node -e "...spawnSync('pnpm', ['run', n])..."`,
// `node ./scripts/verify-local.mjs` (a script that spawns `rustright cargo test`), `sh -c`, `pnpm --dir <elsewhere> run verify`, `pnpm --filter`. HeardRight's
// root `test` is a `node -e` over `verify`, which is `pnpm --dir tauri-app-next run verify`, which is `node ./scripts/verify-local.mjs`, which runs
// `rustright cargo test`; guessing from the text of such a script would defer tests that merely mention the tool, so the app says it instead.
// `ci/gate.config.json` `npm.<unit>.requiresBroker` is `{ "reason": "<why this unit's test needs the broker>" }`, committed with the app and reviewed in the
// diff that adds it. The reason is required: a declaration without one, or of any other shape, is ignored (the test is planned and runs where this host can
// run it, the way the fail-closed default goes) and planScans says so in a note. `false` and `null` are plain "no declaration". It changes nothing on a host
// with the broker, in CI, on a protected ref or with the opt-out: the scan runs there exactly as before. Only where rightkit-gate defers the Rust gate does it
// take the unit's `test:npm:<unit>` scan out, with the Rust scans, named with its reason and never passed.
const REQUIRES_BROKER_REASON_LIMIT = 400;
export function declaredBrokerCall(unit, value) {
  if (value === undefined || value === null || value === false) return {};
  const reason = typeof value === "object" && !Array.isArray(value) && typeof value.reason === "string" ? value.reason.replace(/\s+/g, " ").trim() : "";
  if (!reason) return { problem: `ci/gate.config.json npm.${unit}.requiresBroker is ignored: it must be { "reason": "<why this unit's test needs the broker>" } with a non-empty reason; test:npm:${unit} is planned as if it were not there` };
  const text = reason.length > REQUIRES_BROKER_REASON_LIMIT ? `${reason.slice(0, REQUIRES_BROKER_REASON_LIMIT)}...` : reason;
  return { call: { tool: "rustright", via: ["test"], command: `ci/gate.config.json npm.${unit}.requiresBroker: ${text}`, declared: true } };
}

/**
 * The scan plan for a change: derived from the changed files plus the manifests they belong to.
 * `contentRoot` is a clean export of the commit (pure content checks); `liveRoot` is the checkout the
 * managed `rustright cargo` and `pnpm` run in. A scan whose files did not change is not planned.
 */
export function planScans({
  changed = [], contentRoot, liveRoot = contentRoot, trustedRoot = null, exists = fs.existsSync, readFile = (f) => fs.readFileSync(f, "utf8"),
  platform = process.platform, includeTests = true, localCompile = true,
  // Where the quiet test scans (ci/gate.config.json `quietTests`) run: "local" plans them for the quiet lane; "ci" leaves them to the
  // CI shards (one crate per runner, where the port, pipe, desktop and wall-clock conflicts that force the serial lane do not exist)
  // and records a note naming them (Adrian 2026-10-22: compile locally on the warm cache, qualify the conflicting tests on VMs).
  quietTests = "local",
  // Which scans this plan owes (Adrian, 2026-10-26; app-check.mjs TIERS): "compile" leaves out the scans added after @rightkit/release
  // 0.6.6 (shellcheck, and actionlint's shellcheck requirement) until every app builds through the door again; "release" plans everything.
  tier = "compile",
  // A broker host (rustright): cargo scans run in this primary checkout instead of the content root, because the RustRight client refuses
  // every cargo command but a locked metadata outside the app's primary checkout (RK_NOT_PRIMARY_CHECKOUT). The gate sets it only after
  // proving the checkout's Cargo inputs are blob-identical to the snapshot's (cargoInputsDiffer); null keeps the content/live roots.
  cargoLiveRoot = null,
  // { [cargoDir]: absolute directory }: the checkout's resolved target directory per Cargo workspace, set as CARGO_TARGET_DIR on
  // that workspace's cargo scans when the scans read a snapshot (the door's commit compile, joint plan step 4c).
  targetDirs = null,
  // True when the gate's runtime cargo wrapper (cargo-target-shim.mjs) is first on PATH for every scan: each cargo invocation binds its
  // own workspace's checkout target directory or is refused at run time, so the static walk (cargoReachOf) is advisory, never a refusal.
  cargoShim = false,
  // The command prefix every cargo scan runs through: `rustright cargo` on a managed host, plain `cargo` in CI.
  cargoCommand = ["rustright", "cargo"],
  // Physical containment: a changed path whose real path leaves the real content root (a symlink) is refused.
  realpath = (p) => fs.realpathSync(p),
  // Bytes of a changed file for the input digest (defaults to readFile; the gates pass the raw reader).
  readBytes = readFile,
  // Directory listing, for the every-unit scope a workspace file change brings in.
  listDir = (d) => { try { return fs.readdirSync(d); } catch { return []; } },
  // Per-crate feature sets, shaped like ci/gate.config.json `rust` ({ [crate]: { featureSets, checkFeatureSets } }).
  crateFeatures = {},
  // ci/gate.config.json `npm`: per-package gate settings (`quietTests`), read by the package's unit name (packages/<dir> → rightkit-<dir>).
  packageFeatures = {},
  // True for full class A on the live checkout, which runs the repository's own code anyway (tests, typecheck, lint).
  liveExecution = false,
  // Crates a workspace-wide Rust change (Cargo.lock, vendor/) actually reaches, computed by the caller from
  // the lock graph (scripts/ci/select.mjs). null keeps every crate in scope.
  rustScope = null,
  // Repository-specific units for any layout: [{ name, dir, publish? }], dir repo-relative ("." for a root unit). When given they
  // replace the crates/<name> and packages/<name> layout (rightkit-gate). `cargoDir` is the Cargo workspace root the cargo scans run in.
  // A rust unit may name its own workspace: { name, dir, cargoDir, crate, publish? } runs its scans with cwd cargoDir and `-p <crate>`;
  // `name` is then the unit's identity in scan ids and feature config (rightkit-gate names it `<cargoDir>:<crate>` below the root), and
  // `crateFeatures` is read by `name` first, then by `crate`. Units of different workspaces never share a lock, a deny/audit run or a
  // `cargo metadata`; a unit that sets no cargoDir belongs to the global `cargoDir` above.
  rustUnits = null, npmUnits = null, cargoDir = ".",
  // pnpm roots: repository-relative directories ("." for the repository root) that hold their own lock or workspace file. null keeps the single
  // root "." (every npm unit belongs to it; one pnpm-lockfile and one pnpm-audit scan run there). With a list, each root gets its own
  // `pnpm-lockfile:<root>` and `pnpm-audit:<root>` scans, run in that directory ("." keeps the unsuffixed ids), planned when one of its units is in
  // scope or one of its own package.json / pnpm-lock.yaml / pnpm-workspace.yaml changed. An npm unit names its root as `root` (default "."); a
  // repository with no pnpm root at all (npmRoots: []) plans no pnpm scan (CodeRight: two independent roots and nothing at the repository root).
  npmRoots = null,
  // Parsed Cargo.lock graph (lock-graph.mjs parseLock). With it, every workspace crate that depends on a crate in scope and is
  // not itself in scope gets a `check-dependent` scan, as CI's rust-check rows do. null plans none.
  rustLock = null,
  // The same per Cargo workspace: { [cargoDir]: parsedLock }. A workspace without an entry here (and not the global `cargoDir`) plans
  // no reverse dependents.
  rustLocks = null,
  // The pinned RustSec advisory database ({ url, commit }, advisory-db.json): cargo-deny and cargo-audit read exactly that commit and never fetch.
  advisoryPin = null,
} = {}) {
  if (!contentRoot) throw new TypeError("planScans: contentRoot is required");
  if (!Array.isArray(cargoCommand) || cargoCommand.length === 0 || !["rustright", "cargo"].includes(cargoCommand[0]) || cargoCommand.slice(1).some((a) => a !== "cargo")) {
    throw new TypeError('planScans: cargoCommand must be ["rustright", "cargo"] or ["cargo"]');
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
  if (!["compile", "release"].includes(tier)) throw new Error(`planScans: tier must be compile or release, not ${JSON.stringify(tier)}`);
  let scans = []; const notes = [];
  if (cargoLiveRoot) notes.push(`broker host: cargo scans run in the primary checkout ${cargoLiveRoot}, whose Cargo inputs match the checked snapshot (rustright refuses cargo outside the primary checkout)`);
  // A snapshot compile's scans carry its target map (item 7): the map is part of every scan's cache key, so no result cached by a
  // live-checkout run (or a compile with another map) is taken for one that writes to these directories.
  const snapshotTargets = targetDirs ? { RK_SNAPSHOT_TARGETS: canonicalJson(targetDirs) } : {};
  const add = (d) => {
    const entry = { root: "live", args: [], ...d, env: { ...snapshotTargets, ...(d.env ?? {}) } };
    entry.command = commandText(entry);
    scans.push(entry);
  };
  const A = SCAN_CLASS.A; const B = SCAN_CLASS.B;
  const refuse = (what, why) => add({ id: `path-policy:${sha256(`${what}\0${why}`).slice(0, 12)}`, class: A, internal: "refuse", detail: `${what}: ${why}; refused, never run` });
  for (const r of refusals) refuse(`changed path ${JSON.stringify(r.raw)}`, r.why);
  // A rust unit's name may be `<cargoDir>:<crate>`; the part that reaches argv (`-p`) is the crate, and the workspace directory is
  // checked as a plain path where the unit is registered.
  const safeUnits = (names, kind) => names.filter((n) => {
    if (SAFE_UNIT_NAME.test(kind === "crate" ? crateOf(n) : n)) return true;
    refuse(`${kind} directory name ${JSON.stringify(n)}`, "not a plain name");
    return false;
  });
  const cleanDir = (d) => { const n = path.posix.normalize(posix(String(d))).replace(/\/+$/, ""); return n === "" ? "." : n; };
  const SAFE_UNIT_DIR = /^(?:\.|[A-Za-z0-9_@+.-]+(?:\/[A-Za-z0-9_@+.-]+)*)$/;
  const plainDir = (d) => !ABSOLUTE_PATH.test(d) && !d.split("/").includes("..") && SAFE_UNIT_DIR.test(d);
  const cargoWs = cleanDir(cargoDir);
  if (!plainDir(cargoWs)) throw new TypeError("planScans: cargoDir must be a plain repository-relative directory");
  const cargoIn = (ws, ...args) => ({ tool: cargoCommand[0], args: [...cargoCommand.slice(1), ...args], ...(ws !== "." ? { cwd: ws } : {}), ...(cargoLiveRoot ? { root: "cargoLive" } : {}), ...(targetDirs?.[ws] ? { env: { CARGO_TARGET_DIR: targetDirs[ws] } } : {}) });
  const unitMap = (units, kind) => {
    if (units === null) return null;
    if (!Array.isArray(units)) throw new TypeError(`planScans: ${kind}Units must be an array of { name, dir }`);
    const map = new Map();
    for (const u of units) {
      if (typeof u?.name !== "string" || typeof u?.dir !== "string") throw new TypeError(`planScans: every ${kind} unit needs a string name and dir`);
      if (map.has(u.name)) throw new TypeError(`planScans: duplicate ${kind} unit ${u.name}`);
      const dir = cleanDir(u.dir);
      if (!plainDir(dir)) { refuse(`${kind} unit ${JSON.stringify(u.name)} directory ${JSON.stringify(u.dir)}`, "not a plain repository-relative path"); continue; }
      const entry = { dir, publish: u.publish !== false };
      if (kind === "npm") {
        entry.root = ".";
        if (u.root !== undefined) {
          if (typeof u.root !== "string") throw new TypeError(`planScans: npm unit ${u.name} needs a string root`);
          const r = cleanDir(u.root);
          if (!plainDir(r) || !(r === "." || dir === r || dir.startsWith(`${r}/`))) { refuse(`npm unit ${JSON.stringify(u.name)} root ${JSON.stringify(u.root)}`, "not a plain repository-relative directory that holds the unit"); continue; }
          entry.root = r;
        }
      }
      if (kind === "rust") {
        if (u.crate !== undefined && typeof u.crate !== "string") throw new TypeError(`planScans: rust unit ${u.name} needs a string crate`);
        entry.crate = u.crate ?? u.name;
        entry.cargoDir = cargoWs;
        if (u.cargoDir !== undefined) {
          if (typeof u.cargoDir !== "string") throw new TypeError(`planScans: rust unit ${u.name} needs a string cargoDir`);
          entry.cargoDir = cleanDir(u.cargoDir);
          if (!plainDir(entry.cargoDir)) { refuse(`rust unit ${JSON.stringify(u.name)} cargoDir ${JSON.stringify(u.cargoDir)}`, "not a plain repository-relative path"); continue; }
        }
      }
      map.set(u.name, entry);
    }
    return map;
  };
  const rustMap = unitMap(rustUnits, "rust"); const npmMap = unitMap(npmUnits, "npm");
  // The pnpm roots (see npmRoots): the listed ones and the roots the units name.
  let pnpmRoots = ["."];
  if (npmRoots !== null) {
    if (!Array.isArray(npmRoots)) throw new TypeError("planScans: npmRoots must be null or an array of repository-relative directories");
    const set = new Set(npmMap ? [...npmMap.values()].map((u) => u.root) : []);
    for (const raw of npmRoots) {
      const r = cleanDir(String(raw));
      if (plainDir(r)) set.add(r); else refuse(`pnpm root ${JSON.stringify(raw)}`, "not a plain repository-relative path");
    }
    pnpmRoots = [...set].sort();
  }
  const rootOfUnit = (n) => (npmRoots !== null && npmMap ? npmMap.get(n).root : ".");
  // The deepest pnpm root that holds a repository-relative file (every file is under "." when "." is a root).
  const rootOfFile = (f) => {
    let best = null;
    for (const r of pnpmRoots) if ((r === "." || f.startsWith(`${r}/`)) && (best === null || r.length > best.length)) best = r;
    return best;
  };
  // Units of an explicit-workspace layout (some unit names its cargoDir) get workspace-suffixed ids for the per-workspace scans.
  const perWorkspaceIds = Array.isArray(rustUnits) && rustUnits.some((u) => u?.cargoDir !== undefined);
  const wsIdSuffix = (ws) => (perWorkspaceIds && ws !== "." ? `:${ws}` : "");
  const wsOf = (n) => (rustMap ? rustMap.get(n).cargoDir : cargoWs);
  const crateOf = (n) => (rustMap?.get(n)?.crate ?? n);
  const featuresOf = (n) => crateFeatures?.[n] ?? crateFeatures?.[crateOf(n)] ?? {};
  const workspaces = rustMap && rustMap.size ? [...new Set([...rustMap.values()].map((u) => u.cargoDir))].sort() : [cargoWs];
  const lockOf = (ws) => (rustLocks?.[ws] instanceof Map ? rustLocks[ws] : ws === cargoWs && rustLock instanceof Map ? rustLock : null);
  const rustDir = (n) => (rustMap ? rustMap.get(n).dir : `crates/${n}`);
  const npmDir = (n) => (npmMap ? npmMap.get(n).dir : `packages/${n}`);
  const inDir = (dir, f) => (dir === "." ? f : `${dir}/${f}`);
  // The unit whose directory holds `f`; the deepest directory wins and a root unit (".") only takes what no other unit does.
  const unitOf = (map, f) => {
    let best = null; let score = -1;
    for (const [name, u] of map) {
      const s = u.dir === "." ? 0 : u.dir.length;
      if ((u.dir === "." || f === u.dir || f.startsWith(`${u.dir}/`)) && s > score) { best = name; score = s; }
    }
    return best;
  };

  // ---- class A: content checks on the exported commit
  // A vendor/ file keeps upstream formatting only when vendor/upstream-files.json records it and its
  // bytes still match that upstream digest; any other vendor/ .rs (new, patched) is rustfmt-checked.
  // A patched vendor/ file (rightkit#915) keeps upstream formatting only while its bytes match the
  // reviewed `patched` record, which names its upstream base digest and the note describing the patch;
  // reformatting it would bury the patch. Any edit changes the digest, so the file is checked until re-recorded.
  let upstream = {}; let patched = {};
  try {
    const manifest = JSON.parse(readFile(path.join(contentRoot, "vendor/upstream-files.json")));
    upstream = manifest.files ?? {}; patched = manifest.patched ?? {};
  } catch { /* no manifest: nothing exempt */ }
  const DIGEST = /^[0-9a-f]{64}$/;
  const verbatimUpstream = (f) => f.startsWith("vendor/") && typeof upstream[f] === "string" && bytesDigest(f) === upstream[f];
  const recordedPatch = (f) => f.startsWith("vendor/") && DIGEST.test(patched[f]?.base ?? "") && typeof patched[f]?.note === "string"
    && patched[f].note.startsWith("vendor/") && DIGEST.test(patched[f]?.sha256 ?? "") && bytesDigest(f) === patched[f].sha256;
  // Generated Rust is not hand-formatted: bindgen output, or rustfmt's own `@generated` marker in the first five lines.
  const generatedRust = (f) => {
    let head = "";
    try { head = readFile(path.join(contentRoot, f)).split("\n", 5).join("\n"); } catch { return false; }
    return /@generated\b|automatically generated by rust-bindgen\b/.test(head);
  };
  const rustFiles = files.filter((f) => f.endsWith(".rs") && !verbatimUpstream(f) && !recordedPatch(f) && !generatedRust(f));
  const byEdition = new Map();
  const editionOf = rustEditionResolver({ contentRoot, exists, readFile });
  const refusedEditions = new Set();
  for (const f of rustFiles) {
    const found = editionOf(f);
    if (found.problem) {
      // Never guess an edition: a file whose edition cannot be resolved is not rustfmt-checked under a default, the gate fails on it.
      if (!refusedEditions.has(found.manifest)) { refusedEditions.add(found.manifest); refuse(`rustfmt edition of ${found.manifest}`, found.problem); }
      continue;
    }
    byEdition.set(found.edition, [...(byEdition.get(found.edition) ?? []), f]);
  }
  // One rustfmt scan per edition, batched: Windows refuses a command line past ~32 KB (spawn ENAMETOOLONG on a large repository,
  // HeardRight on the Dell, 2026-10-27), so a batch holds at most RUSTFMT_BATCH files and the first batch keeps the plain id.
  for (const [edition, group] of [...byEdition].sort((a, b) => (a[0] < b[0] ? -1 : 1))) {
    const sorted = group.sort();
    for (let i = 0; i < sorted.length; i += RUSTFMT_BATCH) {
      const batch = sorted.slice(i, i + RUSTFMT_BATCH);
      add({ id: `rustfmt:${edition}${i ? `:${i / RUSTFMT_BATCH + 1}` : ""}`, class: A, tool: "rustfmt", args: ["--check", "--edition", edition, ...batch], root: "content" });
    }
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
  const wsRel = (ws, f) => (ws === "." ? f : f.startsWith(`${ws}/`) ? f.slice(ws.length + 1) : null);
  // Workspaces whose own root files (lock, manifest, toolchain, .cargo, vendor) changed: every unit of such a workspace is in scope.
  // A workspace is also wide when Rust configuration changed in ANY ancestor directory of it (a repository-level .cargo/config.toml,
  // an intermediate apps/.cargo, a root rust-toolchain.toml, clippy.toml, rustfmt.toml or deny.toml reaches every nested workspace).
  const wideWs = new Set(workspaces.filter((ws) => ancestorConfigChanged(ws, files) || files.some((f) => { const r = wsRel(ws, f); return r !== null && WORKSPACE_RUST_FILE.test(r); })));
  // A pnpm root's own install graph (package.json, lock, workspace file, .npmrc) changed: every unit of that root is in scope.
  const wideJsRoots = new Set(pnpmRoots.filter((r) => files.some((f) => { const rel = wsRel(r, f); return rel !== null && WORKSPACE_JS_FILE.test(rel); })));
  const wideCrates = wideWs.size ? (rustMap ? [...rustMap.keys()].filter((n) => wideWs.has(wsOf(n))) : listDir(path.join(contentRoot, "crates"))).filter((n) => !rustScope || rustScope.has(n)) : [];
  if (wideWs.size && rustScope) notes.push(`workspace Rust change reaches ${wideCrates.length} crate(s) through the lock graph`);
  const crateNames = safeUnits([...new Set([...(rustMap ? files.map((f) => unitOf(rustMap, f)).filter(Boolean) : under("crates/").map((f) => f.split("/")[1])), ...wideCrates])].filter((n) => exists(path.join(contentRoot, rustDir(n), "Cargo.toml"))).sort(), "crate");
  // The workspace a changed file belongs to is the deepest one that holds it (a root workspace holds only what no nested one does).
  const wsOfFile = (f) => { let best = null; for (const ws of workspaces) if ((ws === "." || f.startsWith(`${ws}/`)) && (best === null || ws.length > best.length)) best = ws; return best; };
  const configWs = new Set();
  for (const f of files) {
    const isConfig = CONFIG_CRATE_FILE.test(f); const ws = wsOfFile(f);
    if (ws !== null) { if (isConfig || ANCESTOR_DENY_FILE.test(wsRel(ws, f) ?? "")) configWs.add(ws); }
    else if (isConfig && workspaces.length === 1) configWs.add(workspaces[0]);
  }
  // Ancestor configuration is configuration of the workspace too: its metadata, cargo-deny and cargo-audit scans run as well.
  for (const ws of workspaces) if (ancestorConfigChanged(ws, files) || ancestorConfigChanged(ws, files, ANCESTOR_DENY_FILE)) configWs.add(ws);
  const rustConfigChanged = configWs.size > 0;
  const rustTouched = crateNames.length > 0 || rustConfigChanged;
  const touchedWs = [...new Set([...crateNames.map(wsOf), ...configWs])].sort();
  // A root npm package (dir ".") is every file's last resort, so a file inside a non-root Rust unit does not trigger it.
  const npmOwner = (f) => {
    const n = unitOf(npmMap, f);
    if (n !== null && npmMap.get(n).dir === "." && rustMap) { const r = unitOf(rustMap, f); if (r !== null && rustMap.get(r).dir !== ".") return null; }
    return n;
  };
  const pkgNames = safeUnits([...new Set([...(npmMap ? files.map(npmOwner).filter(Boolean) : under("packages/").map((f) => f.split("/")[1])), ...(wideJsRoots.size ? (npmMap ? [...npmMap.keys()].filter((n) => wideJsRoots.has(rootOfUnit(n))) : wideJsRoots.has(".") ? listDir(path.join(contentRoot, "packages")) : []) : [])])].filter((n) => exists(path.join(contentRoot, npmDir(n), "package.json"))).sort(), "package");
  // The pnpm roots with something to check: a unit in scope, or a package.json / lock / workspace file that belongs to the root (the deepest one).
  const jsRoots = [...new Set([...pkgNames.map(rootOfUnit), ...files.filter((f) => JS_TOOLING_FILE.test(f)).map(rootOfFile).filter((r) => r !== null)])].sort();

  // Explicit files: the exported commit has no .git for actionlint's project discovery.
  const workflowFiles = files.filter((f) => /^\.github\/workflows\/[^/]+\.ya?ml$/.test(f)).sort();
  const actionlintConfig = exists(path.join(contentRoot, ".github/actionlint.yaml")) ? ["-config-file", ".github/actionlint.yaml"] : [];
  // actionlint lints every run: script with shellcheck only when shellcheck is found, and otherwise passes silently; class A
  // requires it (`requires`), so a host without it fails the scan instead of skipping the shell checks.
  if (workflowFiles.length) add({ id: "actionlint", class: A, tool: "actionlint", requires: tier === "release" ? ["shellcheck"] : [], args: [...actionlintConfig, ...workflowFiles], root: "content", runScriptLimit: { files: workflowFiles, bytes: RUN_SCRIPT_LIMIT_BYTES } });
  // Standalone shell scripts (SellRight's deploy scripts failed SC2154, SC2030/2031 and SC2016 in CI): every tracked .sh/.bash file,
  // with ShellCheck's own defaults and the repository's .shellcheckrc (data, not code). actionlint covers only workflow run: blocks.
  // A script whose shebang names another shell (zsh, fish) is not ShellCheck's to judge, so it is left out, never failed.
  const otherShell = (f) => { try { return /^#!.*\b(zsh|fish|pwsh|powershell)\b/.test(String(readFile(path.join(contentRoot, f))).split("\n", 1)[0]); } catch { return false; } };
  const shellFiles = files.filter((f) => /\.(sh|bash)$/.test(f) && !/(^|\/)node_modules\//.test(f) && !otherShell(f)).sort();
  if (shellFiles.length && tier !== "release") notes.push("compile tier: shellcheck (added after 0.6.6) is not planned; it runs in the release tier (--to qualify|release)");
  if (shellFiles.length && tier === "release") add({ id: "shellcheck", class: A, tool: "shellcheck", tier: "release", args: ["--format=gcc", "--external-sources", ...shellFiles], root: "content" });
  // No right-git drift scan: the one workflow right-git renders here, ci.yml, is hand-owned (its header;
  // docs/CI.md) and held by scripts/ci/ci-scans-contract.mjs instead, so drift is always reported by design.

  // Exceptions expire by date, not by change: checked on every run, whatever changed.
  if (exists(path.join(contentRoot, "ci/advisory-exceptions.json"))) add({ id: "advisory-exceptions", class: A, internal: "advisory-exceptions" });
  // An app's advisory exceptions follow the same policy as RightKit's own: a Cargo workspace whose deny.toml or .cargo/audit.toml
  // ignores an advisory needs its ledger, <workspace>/.rightkit/advisory-exceptions.json, with owner, evidence, an unexpired
  // reviewBy, the exact affected versions and dependents in that workspace's Cargo.lock, and a person's approval; deny.toml and
  // audit.toml must match it exactly.
  for (const ws of workspaces) {
    if (typeof ws !== "string") continue;
    const dir = ws === "." ? "" : ws;
    // A non-empty list only: `[^\]\s]` is one character that is neither whitespace nor the closing bracket, so `ignore = []` followed by
    // a later `]` (the next table header) is not an ignore (Pulse, 2026-10-27: `\S` matched the `]` itself).
    const ignores = ["deny.toml", ".cargo/audit.toml"].some((f) => { try { return /\bignore\s*=\s*\[[^\]]*[^\]\s][^\]]*\]/s.test(readFile(path.join(contentRoot, dir, f))); } catch { return false; } });
    if (ignores && !(dir === "" && exists(path.join(contentRoot, "ci/advisory-exceptions.json")))) add({ id: `advisory-exceptions${dir ? `:${dir}` : ""}`, class: A, internal: "advisory-exceptions", dir, exceptionsFile: ".rightkit/advisory-exceptions.json" });
  }
  // Like generated-docs, planned only when a changed file is one of the contract's inputs or projections. The generator runs from
  // the trusted scanner checkout (the import below); the exported contract and projections are data it checks.
  if (files.some((f) => SUITE_API_INPUT.test(f)) && exists(path.join(contentRoot, "contracts/right-suite-api.v1.json"))) {
    // RightKit's own generator contract (scripts/gen-suite-api.mjs) is the checked repository's code: like scripts/preflight.mjs it
    // runs only when this gate is that repository's in-tree copy (and in CI), never from a pinned or installed copy checking the
    // repository (the broker's trusted gate refused RightKit's own compile on a module it could not find, 2026-10-27).
    // The trusted home is the gate's own repository (`trustedRoot`, the checkout rightkit-gate runs for: a commit check exports the
    // commit to a snapshot and hands that as contentRoot and liveRoot); the generator is code and runs only from that checkout's
    // in-tree gate, reading the snapshot as data. Without a trusted root (library callers), the live checkout or content root decides.
    const home = trustedRoot ?? (path.resolve(liveRoot) !== path.resolve(contentRoot) ? liveRoot : contentRoot);
    if (inTreeGate(home, exists)) add({ id: "suite-api", class: A, internal: "suite-api" });
    else notes.push("suite-api: RightKit's own generator contract runs from the in-tree gate and CI, not from this installed copy");
  }
  if (rustTouched) {
    for (const ws of touchedWs) add({ id: `cargo-metadata${wsIdSuffix(ws)}`, class: A, ...cargoIn(ws, ...metadataArgs()) });
    // Locked package listings are observations in RustRight (#1216). They run through the selected executor
    // from the checked Cargo inputs, just like metadata; an executor refusal remains a failed scan.
    if (tier === "release") for (const name of crateNames.filter((n) => rustMap?.get(n).publish !== false)) add({ id: rustScanId.packageList(name), class: A, tier: "release", ...cargoIn(wsOf(name), "package", "-p", crateOf(name), "--list", "--allow-dirty", "--locked") });
  }
  for (const ws of [...configWs].sort()) {
    // Offline against the pinned database. The pin commit is part of the command (env), so a pin change is a different scan, and the
    // placeholders are filled at run time from the materialised checkout (runScans).
    const pin = advisoryPin ?? (advisoryPin = readAdvisoryPin());
    const pinEnv = { RK_ADVISORY_DB_COMMIT: pin.commit };
    add({ id: `cargo-deny${wsIdSuffix(ws)}`, class: A, ...cargoIn(ws, "deny", "--frozen", "--metadata-path", "{advisoryMetadata}", "--config", "{advisoryDenyConfig}", "check"), env: pinEnv, advisoryDb: "deny", advisoryPin: pin });
    add({ id: `cargo-audit${wsIdSuffix(ws)}`, class: A, ...cargoIn(ws, "audit", "--db", "{advisoryDb}", "--no-fetch"), env: pinEnv, advisoryDb: "audit", advisoryPin: pin });
  }
  // The audit policy is the consumer's own: every package.json script that is exactly `pnpm audit <flags>` (only --prod, --dev,
  // --audit-level <low|moderate|high|critical> are accepted) declares one, and the scan runs the strictest of them all: a dependency
  // scope only when every declaration names that same scope, and the lowest audit level any declares (one without a level means low).
  // With none declared, every dependency at every severity. The gate never runs a weaker audit than the repository declares
  // (RightSites declares `pnpm audit --prod`: production dependencies, all severities).
  const LEVELS = ["low", "moderate", "high", "critical"];
  const consumerAuditFlags = (r) => {
    let scripts = {};
    try { scripts = JSON.parse(readFile(path.join(contentRoot, r, "package.json"))).scripts ?? {}; } catch { /* none: the strict default */ }
    const declared = [];
    for (const cmd of Object.values(scripts)) {
      const m = /^\s*pnpm\s+audit((?:\s+(?:--prod|--dev|--audit-level(?:=|\s+)(?:low|moderate|high|critical)))*)\s*$/.exec(String(cmd));
      if (!m) continue;
      const words = m[1].trim().split(/\s+|=/).filter(Boolean);
      const at = words.indexOf("--audit-level");
      const scope = words.includes("--prod") && !words.includes("--dev") ? "--prod" : words.includes("--dev") && !words.includes("--prod") ? "--dev" : null;
      declared.push({ scope, level: at < 0 ? 0 : LEVELS.indexOf(words[at + 1]) });
    }
    if (!declared.length) return [];
    const scope = declared.every((d) => d.scope === declared[0].scope) ? declared[0].scope : null;
    const level = Math.min(...declared.map((d) => d.level));
    return [...(scope ? [scope] : []), ...(level > 0 ? ["--audit-level", LEVELS[level]] : [])];
  };
  for (const r of jsRoots) {
    // Class A runs no repository hooks: `install` accepts --ignore-pnpmfile and --ignore-scripts; `audit` accepts
    // neither flag, so every class A pnpm scan also sets the same two settings through the environment.
    // One pair per pnpm root, run in the root's directory; the repository root keeps the unsuffixed ids.
    const where = r === "." ? {} : { cwd: r }; const suffix = r === "." ? "" : `:${r}`;
    add({ id: `pnpm-lockfile${suffix}`, class: A, tool: "pnpm", args: ["install", "--frozen-lockfile", "--lockfile-only", "--ignore-pnpmfile", "--ignore-scripts"], env: PNPM_NO_HOOKS_ENV, root: "content", ...where });
    add({ id: `pnpm-audit${suffix}`, class: A, tool: "pnpm", args: ["audit", ...consumerAuditFlags(r)], env: PNPM_NO_HOOKS_ENV, root: "content", ...where });
  }
  const crateDirs = crateNames.map(rustDir); const pkgDirs = pkgNames.map(npmDir);
  const withChangelog = crateNames.filter((n) => exists(path.join(contentRoot, rustDir(n), "CHANGELOG.md")) && exists(path.join(contentRoot, rustDir(n), "Cargo.toml")));
  for (const name of withChangelog) add({ id: rustScanId.changelog(name), class: A, internal: "changelog-version", files: [inDir(rustDir(name), "Cargo.toml"), inDir(rustDir(name), "CHANGELOG.md")] });
  if (crateDirs.length || pkgDirs.length) {
    add({ id: "license", class: A, internal: "license", files: [...crateDirs.map((d) => inDir(d, "Cargo.toml")), ...pkgDirs.map((d) => inDir(d, "package.json"))] });
    add({ id: "secret-scan", class: A, internal: "secret-scan", dirs: [...crateDirs, ...pkgDirs] });
  }

  // ---- class B: needs the compiler or runs tests or repository code
  // The docs generator is code. It runs only from the TRUSTED checkout (liveRoot), with the exported commit
  // handed to it as data (`--root`); the content root's own copy is never executed. When no separate
  // trusted checkout exists (liveRoot is contentRoot) or it lacks the generator, the scan is marked as
  // requiring a trusted-root run: deferred (never a pass), not run from submitted code.
  // Planned only when a changed file is one of the generator's inputs (GENERATOR_INPUT).
  if (exists(path.join(contentRoot, "scripts/generate-index.mjs")) && files.some((f) => GENERATOR_INPUT.test(f))) {
    // Full class A on the live checkout (`liveExecution`) already executes the repository's own code (its tests): there the
    // generator runs from that same checkout. Everywhere else the separation above holds.
    const trusted = (liveExecution || path.resolve(liveRoot) !== path.resolve(contentRoot)) && exists(path.join(liveRoot, "scripts/generate-index.mjs"));
    add({
      id: "generated-docs", class: A, executes: true, tool: "node", args: ["scripts/generate-index.mjs", "--check", "--root", "{contentRoot}"],
      ...(trusted ? {} : { deferred: "requires a trusted-root run: the generator is repository code and no separate trusted checkout provides it; the content root's copy is never executed" }),
    });
  }
  for (const name of crateNames) {
    // Feature sets exactly as CI's gate runs them (scripts/ci/gate.mjs, ci/gate.config.json rust.<crate>):
    // clippy per `featureSets` (default features when absent); check per `checkFeatureSets` (default
    // --all-features), none when a lint set already uses --all-features. Some crates' features cannot
    // all be on together (rightkit-db sqlcipher vs sqlcipher-system; qa-driver wv2 generations).
    const own = featuresOf(name);
    const { lintSets, defaultCheck, checkSets } = rustCratePlan(own);
    const ws = wsOf(name); const crate = crateOf(name);
    // `cargo clippy --all-targets` type-checks exactly what `cargo check --all-targets` does: the default
    // check runs only when no clippy set covers default features.
    if (defaultCheck) add({ id: rustScanId.check(name), class: B, ...cargoIn(ws, ...checkArgs(crate, { locked: true })) });
    checkSets.forEach((set, i) => add({ id: rustScanId.checkFeatures(name, i), class: B, ...cargoIn(ws, ...checkArgs(crate, { features: set, locked: true })) }));
    lintSets.forEach((set, i) => add({ id: rustScanId.clippy(name, lintSets.length, i), class: B, ...cargoIn(ws, ...clippyArgs(crate, { features: set, locked: true })) }));
    // rustdoc flags go after `--` for this crate's library only: the broker owns RUSTDOCFLAGS and refuses
    // a request that sets it. A crate with no library has no API docs to check.
    if (exists(path.join(contentRoot, rustDir(name), "src", "lib.rs"))) {
      add({ id: rustScanId.doc(name), class: B, ...cargoIn(ws, "rustdoc", "-p", crate, "--lib", ...lintSets[0], "--locked", "--", ...DOC_RUSTDOC_ARGS) });
    }
  }
  // Reverse dependents: a workspace crate that is not in scope but whose lock closure reaches one that is must still compile.
  // A unit only depends on units of its own Cargo workspace (its own lock), so each workspace is judged by its own lock.
  const inScope = new Set(crateNames);
  for (const ws of [...new Set(crateNames.map(wsOf))].sort()) {
    const lock = lockOf(ws);
    if (!lock) continue;
    const memo = new Map();
    const scoped = crateNames.filter((n) => wsOf(n) === ws).map(crateOf);
    const workspace = (rustMap ? [...rustMap.keys()] : listDir(path.join(contentRoot, "crates"))).filter((n) => wsOf(n) === ws);
    const dependents = safeUnits(workspace.filter((n) => !inScope.has(n) && exists(path.join(contentRoot, rustDir(n), "Cargo.toml")) && scoped.some((t) => reach(lock, crateOf(n), memo).has(t))).sort(), "crate");
    for (const name of dependents) add({ id: `check-dependent:${name}`, class: B, ...cargoIn(ws, ...checkArgs(crateOf(name), { locked: true })) });
  }
  // A linter config in the unit's directory or any ancestor of it (a repository-level config covers every unit, as the tools look it up).
  const hasConfig = (dir, names) => ancestorDirsOf(dir).some((d) => names.some((f) => exists(path.join(liveRoot, inDir(d, f)))));
  // tsc reads a workspace sibling's types where its package.json exports point: built output (dist/*.d.ts) that no commit holds, so a
  // check of the committed tree (CI, a commit snapshot) cannot find them, and a checkout with an old build checks against stale types.
  // A package that depends on another workspace package is therefore checked through a generated project that maps each sibling's
  // exported entry points to its committed sources (TSC_PROJECT, built by the runner); every other package keeps its own tsconfig.
  // `--noEmit` stays on the command line as well as in the project: the class A launch guard judges a tsc launch by its argv alone.
  const workspaceDirs = (npmMap ? [...npmMap.values()].map((u) => u.dir) : listDir(path.join(liveRoot, "packages")).map((n) => `packages/${n}`)).sort();
  const manifestOf = (dir) => { try { return JSON.parse(readFile(path.join(liveRoot, dir, "package.json")).replace(/^﻿/, "")); } catch { return null; } };
  const workspaceNames = new Map(workspaceDirs.map((d) => [manifestOf(d)?.name, d]).filter(([n]) => typeof n === "string"));
  const usesWorkspaceSibling = (dir) => {
    const m = manifestOf(dir);
    return ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"].some((k) => Object.keys(m?.[k] ?? {}).some((n) => n !== m.name && workspaceNames.has(n)));
  };
  for (const name of pkgNames) {
    const dir = npmDir(name);
    if (exists(path.join(liveRoot, dir, "tsconfig.json"))) {
      add(usesWorkspaceSibling(dir)
        ? { id: `tsc:${name}`, class: A, executes: true, tool: "pnpm", args: ["exec", "tsc", "--noEmit", "-p", TSC_PROJECT], tscWorkspace: [...workspaceNames.values()].sort(), env: PNPM_RUN_ENV, cwd: dir }
        : { id: `tsc:${name}`, class: A, executes: true, tool: "pnpm", args: ["exec", "tsc", "--noEmit", "-p", "tsconfig.json"], env: PNPM_RUN_ENV, cwd: dir });
    }
    if (hasConfig(dir, ESLINT_CONFIG_FILES)) add({ id: `eslint:${name}`, class: A, executes: true, tool: "pnpm", args: ["exec", "eslint", "."], env: PNPM_RUN_ENV, cwd: dir });
    if (hasConfig(dir, BIOME_CONFIG_FILES)) add({ id: `biome:${name}`, class: A, executes: true, tool: "pnpm", args: ["exec", "biome", "lint", "."], env: PNPM_RUN_ENV, cwd: dir });
  }
  // One `swift build` per Swift package a changed file belongs to: the nearest Package.swift at or above the file, built in its own
  // directory (rightkit#1229: Pulse's package is swift/RightKitHub, and a root `swift build` found no package and blocked every push).
  const swiftFiles = files.filter((f) => f.startsWith("swift/") || f === "Package.swift" || f.endsWith("/Package.swift"));
  if (swiftFiles.length) {
    const packages = new Set();
    for (const f of swiftFiles) {
      const owner = ancestorDirsOf(path.posix.dirname(f)).reverse().find((d) => exists(path.join(contentRoot, inDir(d, "Package.swift"))));
      if (owner !== undefined) packages.add(owner);
      else notes.push(`${f}: no Package.swift at or above it, so no Swift package builds it`);
    }
    for (const dir of [...packages].sort()) {
      const id = dir === "" || dir === "." ? "swift-build" : `swift-build:${dir}`;
      if (platform === "darwin") add({ id, class: B, tool: "swift", args: ["build"], ...(dir === "" || dir === "." ? {} : { cwd: dir }) });
      else notes.push(`swift files changed in ${dir === "" || dir === "." ? "the root package" : dir}; \`swift build\` is required on the Mac host before its push`);
    }
  }
  if (includeTests) {
    const deferredToCi = [];
    // Tests per declared feature set, serially too where the crate asks for it, as CI's gate runs them. `quietTests: true` marks
    // every test scan of the crate for the quiet lane (laneOf): run alone, under the broker's quiet lease.
    for (const name of crateNames) {
      const own = featuresOf(name);
      const sets = rustCratePlan(own).lintSets;
      const quiet = own.quietTests === true ? { quiet: true } : {};
      if (own.quietTests === true && quietTests === "ci") {
        deferredToCi.push(...sets.map((set, i) => rustScanId.testRust(name, sets.length, i)), ...(own.alsoSerial ? sets.map((set, i) => rustScanId.testRustSerial(name, sets.length, i)) : []));
        continue;
      }
      sets.forEach((set, i) => {
        add({ id: rustScanId.testRust(name, sets.length, i), class: B, ...quiet, ...cargoIn(wsOf(name), "test", "-p", crateOf(name), ...set, "--locked", "--no-fail-fast") });
        if (own.alsoSerial) add({ id: rustScanId.testRustSerial(name, sets.length, i), class: B, ...quiet, ...cargoIn(wsOf(name), "test", "-p", crateOf(name), ...set, "--locked", "--no-fail-fast", "--", "--test-threads=1") });
      });
    }
    for (const name of pkgNames) {
      let manifest = null;
      try { manifest = JSON.parse(readFile(path.join(liveRoot, npmDir(name), "package.json"))); } catch { /* unreadable manifest: no test row */ }
      if (!manifest?.scripts?.test) continue;
      // `npm.<package>.quietTests: true` (ci/gate.config.json) marks a package whose tests are wall-clock bound (vitest timeouts,
      // Chrome for Testing shutdown): under "ci" they ride the CI shards, under "local" the quiet lane, never the 4-wide test lane.
      const own = packageFeatures?.[name] ?? {};
      if (own.quietTests === true && quietTests === "ci") { deferredToCi.push(`test:npm:${name}`); continue; }
      // A gate proves the repository's own content: sibling checkouts beside it (RightKit's suite-contract tests read the app
      // repositories when present) are not inputs, so the package tests run as an isolated checkout, as they do in CI.
      // `brokerCall` marks a test script that runs the broker (or cargo) through its script chain (scriptBrokerCall), or a unit whose app declares that
      // its test needs the broker (declaredBrokerCall): rightkit-gate defers such a scan with the Rust gate where this host has no broker.
      // A declaration (`npm.<unit>.requiresBroker`, declaredBrokerCall) covers what the script text does not show; a call the scripts do show wins.
      const declared = declaredBrokerCall(name, own.requiresBroker);
      if (declared.problem) notes.push(declared.problem);
      // `npm.<unit>.afterClassA.reason` (ci/gate.config.json): the unit's tests launch stand-ins named like compilers (stub scripts,
      // which can run anything), so they cannot run under the class A launch guard; they run after class A, with the compile scans.
      // A reason is required.
      const afterClassA = typeof own.afterClassA?.reason === "string" && own.afterClassA.reason.trim() !== "" ? own.afterClassA.reason.trim() : null;
      const brokerCall = scriptBrokerCall(manifest.scripts, "test") ?? declared.call ?? null;
      const buildCall = brokerCall ? null : scriptBuildCall(manifest.scripts, "test");
      const laterReason = brokerCall ? null : buildCall ? `its test script builds before testing (${buildCall.command}); it runs after class A, with the compile scans` : afterClassA ? `declared to run after class A (ci/gate.config.json npm.${name}.afterClassA): ${afterClassA}` : null;
      // Isolated only where the tests run on an export (contentRoot is not the live checkout): there sibling repositories are
      // absent by construction. On the live checkout (full class A, broker admission) the cross-app contract tests run for real.
      const isolatedCheckout = path.resolve(contentRoot) !== path.resolve(liveRoot);
      // `npm.<unit>.classATests.evidence` (ci/gate.config.json): the unit's tests were shown, by import inspection and launch observation
      // under the class A launch guard, to compile and build nothing (docs/GATE-CLASS-A-INVENTORY.md "Test units"); they run in class A,
      // still under the guard, which refuses any compile at run time. Never when the script reaches the broker, builds, or is declared after class A.
      const classAEvidence = typeof own.classATests?.evidence === "string" && own.classATests.evidence.trim() !== "" ? own.classATests.evidence.trim() : null;
      const inClassA = classAEvidence && !brokerCall && !buildCall && !afterClassA;
      if (classAEvidence && !inClassA) notes.push(`npm.${name}.classATests is ignored: ${brokerCall ? "its test script reaches the broker" : buildCall ? `its test script builds (${buildCall.command})` : "it is declared afterClassA"}; it runs after class A`);
      // A commit snapshot's compile (targetDirs): a test that reaches cargo writes where cargo puts the outputs of the workspace it
      // finds from the directory it runs in, which would be inside the snapshot. Bound to that workspace's checkout target directory
      // when the reachable path proves the directory (cargoReachOf); refused before launch only when it reaches Rust unprovably.
      let cargoTarget = null;
      if (targetDirs && cargoShim) {
        // Advisory only: the wrapper binds or refuses each cargo the test starts, whatever path reaches it.
        const reach = cargoReachOf({ unitDir: npmDir(name), scripts: manifest.scripts, declared });
        if (reach.unprovable && Object.keys(targetDirs).length) notes.push(`npm unit ${name}: the static walk cannot prove where its cargo writes (${reach.unprovable}); the runtime cargo wrapper binds each invocation to its workspace's checkout target directory or refuses it`);
      } else if (targetDirs) {
        const unitDir = npmDir(name);
        const scriptsOf = (dir) => { try { const m = JSON.parse(readFile(path.join(contentRoot, ...(dir === "." ? [] : dir.split("/")), "package.json"))); return m?.scripts && typeof m.scripts === "object" ? m.scripts : {}; } catch { return null; } };
        const packageDirOf = (pkg) => {
          for (const [unit, entry] of npmMap ?? []) {
            if (unit === pkg) return entry.dir;
            try { if (JSON.parse(readFile(path.join(contentRoot, ...(entry.dir === "." ? [] : entry.dir.split("/")), "package.json"))).name === pkg) return entry.dir; } catch { /* unreadable: not this one */ }
          }
          return null;
        };
        // Every call the test reaches, not the first: one CARGO_TARGET_DIR binds the whole test, so all of them must be one workspace.
        const reach = cargoReachOf({ unitDir, scripts: manifest.scripts, declared, readScripts: scriptsOf, packageDirOf });
        const hasRust = Object.keys(targetDirs).length > 0;
        if (reach.unprovable) {
          if (hasRust) { refuse(`npm unit ${name}`, `its test reaches cargo while the compile reads a commit snapshot, and which workspace's outputs it writes cannot be proven (${reach.unprovable}); outputs must be the checkout's`); continue; }
        } else if (reach.dir !== undefined || reach.dirs) {
          const workspaceOf = (from) => {
            let manifestDir = null;
            for (let d = from; ; d = path.posix.dirname(d) === "." || d === "." ? "." : path.posix.dirname(d)) {
              if (exists(path.join(contentRoot, ...(d === "." ? [] : d.split("/")), "Cargo.toml"))) { manifestDir = d; break; }
              if (d === ".") break;
            }
            const ws = manifestDir === null ? null : Object.hasOwn(targetDirs, manifestDir) ? manifestDir : ([...(rustMap?.values() ?? [])].find((u) => u.dir === manifestDir)?.cargoDir ?? null);
            return ws && targetDirs[ws] ? { ws } : { why: `its test reaches cargo from ${from}, ${manifestDir === null ? `which no Cargo.toml encloses` : `whose ${manifestDir}/Cargo.toml is not a known workspace or member`}; outputs must be the checkout's` };
          };
          const found = (reach.dirs ?? [reach.dir]).map(workspaceOf);
          const unknown = found.find((f) => f.why);
          if (unknown) { refuse(`npm unit ${name}`, unknown.why); continue; }
          const workspaces = [...new Set(found.map((f) => f.ws))];
          if (workspaces.length > 1) { refuse(`npm unit ${name}`, `its test reaches cargo in more than one workspace (${workspaces.join(", ")} from ${reach.dirs.join(", ")}); one CARGO_TARGET_DIR binds the whole test, so one of them would write into another's outputs`); continue; }
          cargoTarget = targetDirs[workspaces[0]];
        }
      }
      add({ id: `test:npm:${name}`, class: inClassA ? A : B, kind: "test", executes: true, classReason: inClassA ? `class A on recorded evidence (${classAEvidence}); run under the launch guard` : brokerCall ? "its test script reaches the broker (compiles); it runs with the compile scans" : laterReason ?? "test scripts execute project code and run after class A", ...(own.quietTests === true ? { quiet: true } : {}), ...(brokerCall ? { brokerCall } : {}), tool: "pnpm", args: ["--dir", npmDir(name), "run", "test"], env: { ...PNPM_RUN_ENV, ...(isolatedCheckout ? { RIGHTKIT_ISOLATED_CHECKOUT: "1" } : {}), ...(cargoTarget ? { CARGO_TARGET_DIR: cargoTarget } : {}) } });
    }
    if (deferredToCi.length) notes.push(`${deferredToCi.length} quiet test scan(s) left to the CI shards (one crate per runner), not run here: ${deferredToCi.join(", ")}`);
    // scripts/ci tests run when scripts/, ci/ or .github/ changed, or a file the scripts/ci sources reference (imports and
    // relative path literals, followed through packages/*.mjs) changed. Where unreadable, they run.
    const ciDir = exists(path.join(liveRoot, "scripts/ci"));
    const scriptsCiTouched = () => {
      if (files.some((f) => /^(scripts|ci|\.github)\//.test(f))) return true;
      try {
        const sources = listDir(path.join(liveRoot, "scripts/ci")).filter((n) => /\.mjs$/.test(n));
        if (!sources.length) return true;
        const inputs = new Set(); const seen = new Set();
        const visit = (rel) => {
          if (seen.has(rel)) return;
          seen.add(rel);
          let text; try { text = readFile(path.join(liveRoot, rel)); } catch { return; }
          for (const m of text.matchAll(/["'`]((?:\.\.?\/)+[^"'`\s]+)["'`]/g)) {
            const target = path.posix.normalize(path.posix.join(path.posix.dirname(rel), m[1])).replace(/\/+$/, "");
            if (!target || target === "." || target.startsWith("..")) continue;
            inputs.add(target);
            if (target.startsWith("packages/") && target.endsWith(".mjs")) visit(target);
          }
        };
        for (const n of sources) visit(`scripts/ci/${n}`);
        return files.some((f) => [...inputs].some((i) => f === i || f.startsWith(`${i}/`)));
      } catch { return true; }
    };
    if (ciDir ? scriptsCiTouched() : (!(rustMap || npmMap) && files.some((f) => f.startsWith("scripts/")))) {
      // The suite is the exported commit's test files, named explicitly: an untracked WIP test in the live
      // checkout is never run and can neither block nor pass a push.
      const suite = listDir(path.join(contentRoot, "scripts", "ci")).filter((n) => n.endsWith(".test.mjs")).sort().map((n) => `scripts/ci/${n}`);
      add({ id: "test:scripts-ci", class: B, kind: "test", executes: true, classReason: "tests execute project code and run after class A", tool: "node", args: ["--test", ...(suite.length ? suite : ["scripts/ci/*.test.mjs"])] });
    }
  }

  // Without a local compile grant the managed broker path is closed: every broker scan (class A
  // deny/audit/package list included) stays in the plan marked `deferred`, never dropped, never a pass
  // and never run outside the broker. A local gate treats any deferral as a failure (#888).
  const deferred = [];
  if (!localCompile) {
    scans = scans.map((x) => {
      if (x.tool !== "rustright" && !(x.tool === "cargo" && cargoCommand.length === 1)) return x;
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
    schema: SCAN_RESULT_SCHEMA, roots: { contentRoot, liveRoot, ...(cargoLiveRoot ? { cargoLiveRoot } : {}) }, changed: files, inputDigest, scans, notes, deferred,
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
export function resolveInvocation(command, args, { platform = process.platform, env = process.env, exists = fs.existsSync, execPath = process.execPath, readFile = (f) => fs.readFileSync(f, "utf8") } = {}) {
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
  // A Node cmd-shim (what npm, pnpm and nvm install: `"%~dp0\...\entry.js" %*`, npm's `SET "NPM_CLI_JS=%~dp0\...npm-cli.js"`) runs
  // its JS entry with node: started here as node + that entry, no shell, so any argument passes unchanged (rightkit#1261).
  const entry = cmdShimEntry(found, { readFile, exists });
  if (entry) return { file: execPath, args: [entry, ...args], verbatim: false };
  const bad = args.find((a) => !SAFE_SHIM_ARG.test(a));
  if (bad !== undefined) return { error: new Error(`refusing to pass ${JSON.stringify(bad)} to the shell shim ${found}`) };
  if (!SAFE_SHIM_PATH.test(found)) return { error: new Error(`refusing to run shell shim ${found}: unsafe path`) };
  // Outer quotes around the whole line: with /s cmd strips exactly one leading and one trailing quote.
  return { file: env.ComSpec || "cmd.exe", args: ["/d", "/s", "/c", `""${found}"${args.map((a) => ` ${a}`).join("")}"`], verbatim: true };
}

/**
 * The JS entry a Node cmd-shim at `shim` runs, or null: the line that passes the caller's arguments (`%*`) names it, either as a
 * `%~dp0`-relative path (`"%~dp0\\..\\pnpm.mjs" %*`) or through a variable set to one (npm's `"%NODE_EXE%" "%NPM_CLI_JS%" %*` with
 * `SET "NPM_CLI_JS=%~dp0\\node_modules\\npm\\bin\\npm-cli.js"`). Helpers the shim runs on other lines (npm's npm-prefix.js) are not it.
 */
export function cmdShimEntry(shim, { readFile = (f) => fs.readFileSync(f, "utf8"), exists = fs.existsSync } = {}) {
  let text;
  try { text = String(readFile(shim)); } catch { return null; }
  const vars = new Map();
  // Every value a variable is set to (npm's NPM_CLI_JS is reassigned to a global-prefix copy when one exists): the %~dp0 one is used.
  for (const m of text.matchAll(/^\s*@?SET\s+"?([A-Za-z_][A-Za-z0-9_]*)=([^"\r\n]*)"?\s*$/gim)) vars.set(m[1].toUpperCase(), [...(vars.get(m[1].toUpperCase()) ?? []), m[2]]);
  const fromDp0 = (value) => {
    const m = /^%~dp0\\?(.+\.(?:mjs|cjs|js))$/i.exec(String(value ?? "").trim());
    if (!m) return null;
    const entry = path.win32.resolve(path.win32.dirname(shim), m[1]);
    return exists(entry) ? entry : null;
  };
  for (const line of text.split(/\r?\n/)) {
    if (!line.includes("%*")) continue;
    for (const q of line.matchAll(/"([^"]+)"/g)) {
      const token = q[1];
      const direct = fromDp0(token);
      if (direct) return direct;
      const v = /^%([A-Za-z_][A-Za-z0-9_]*)%$/.exec(token);
      if (v) for (const value of vars.get(v[1].toUpperCase()) ?? []) { const viaVar = fromDp0(value); if (viaVar) return viaVar; }
    }
  }
  return null;
}

/** Default spawn: synchronous, no console window, never `shell: true`. */
export function defaultSpawn(command, args, options = {}) {
  if (options.boundedMs) return boundedSpawn(command, args, options);
  const inv = resolveInvocation(command, args, { env: options.env ?? process.env });
  if (inv.error) return { status: null, stdout: "", stderr: "", error: inv.error };
  return spawnSync(inv.file, inv.args, { encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024, windowsVerbatimArguments: inv.verbatim, ...options, shell: false });
}

const OUTPUT_KEEP = 4 * 1024 * 1024;
/**
 * defaultSpawn with a deadline (`options.boundedMs`), for short identity probes that must never hang the caller: the command runs
 * in a helper node process through defaultSpawnAsync, which on the deadline kills the command's whole process tree
 * (killProcessTree) and reports `timedOut: true`. The helper records the command's pid first; if the helper itself overruns its
 * backstop (`backstopMs`, default deadline + 15 s), it is killed and the command's tree is killed from here by that pid, so no
 * probe outlives the call. A launch failure keeps its `error` (e.g. ENOENT); a command that ran keeps its `status`.
 */
export function boundedSpawn(command, args, { boundedMs, backstopMs = boundedMs + 15000, cwd, env, pidFile = null } = {}) {
  const dir = pidFile ? null : fs.mkdtempSync(path.join(os.tmpdir(), "rk-probe-"));
  const pids = pidFile ?? path.join(dir, "pid");
  const script = `import(${JSON.stringify(new URL(import.meta.url).href)}).then(async (m) => {
    const [command, args, ms, pidFile] = JSON.parse(process.argv[1]); const fs = await import("node:fs");
    const ac = new AbortController(); const timer = setTimeout(() => ac.abort(), ms);
    const r = await m.defaultSpawnAsync(command, args, { signal: ac.signal, onSpawn: (pid) => fs.writeFileSync(pidFile, String(pid)) }); clearTimeout(timer);
    process.stdout.write(JSON.stringify({ status: r.status, stdout: r.stdout, stderr: r.stderr, timedOut: r.cancelled === true, error: r.cancelled ? null : (r.error ? { code: r.error.code ?? null, message: r.error.message } : null) }));
  });`;
  try {
    const r = spawnSync(process.execPath, ["--input-type=module", "-e", script, JSON.stringify([command, args, boundedMs, pids])], { cwd, env: env ?? process.env, encoding: "utf8", windowsHide: true, stdio: ["ignore", "pipe", "pipe"], timeout: backstopMs, killSignal: "SIGKILL", maxBuffer: 16 * 1024 * 1024 });
    const timeout = (why) => ({ status: null, stdout: "", stderr: "", timedOut: true, error: Object.assign(new Error(`timed out after ${boundedMs} ms${why}`), { code: "ETIMEDOUT" }) });
    if (r.error?.code === "ETIMEDOUT") {
      let pid = null; try { pid = Number(fs.readFileSync(pids, "utf8")) || null; } catch { /* never started */ }
      if (pid) killProcessTree({ pid });
      return timeout(" (backstop)");
    }
    try {
      const o = JSON.parse(r.stdout);
      if (o.timedOut) return timeout("");
      return { status: o.status, stdout: o.stdout ?? "", stderr: o.stderr ?? "", ...(o.error ? { error: Object.assign(new Error(o.error.message), { code: o.error.code }) } : {}) };
    } catch { return { status: null, stdout: "", stderr: r.stderr ?? "", error: new Error(`the bounded probe gave no result (${r.error?.message ?? `exit ${r.status}`})`) }; }
  } finally { if (dir) fs.rmSync(dir, { recursive: true, force: true }); }
}

/** The environment additions that load the class A launch guard into a test row's Node processes (NODE_OPTIONS appended, not replaced). */
export function launchGuardEnv(env = process.env) {
  const current = String(env.NODE_OPTIONS ?? "");
  return { RIGHTKIT_CLASS_A_LAUNCH_GUARD: "1", NODE_OPTIONS: current.includes(LAUNCH_GUARD_IMPORT) ? current : `${current} ${LAUNCH_GUARD_IMPORT}`.trim() };
}

/** Kill `child` and everything it started: taskkill /T /F on Windows, the whole process group elsewhere (the child leads one when spawned with a signal). */
export function killProcessTree(child, { platform = process.platform, run = spawnSync, kill = (pid, sig) => process.kill(pid, sig) } = {}) {
  if (!child?.pid) return;
  if (platform === "win32") {
    try { run("taskkill", ["/T", "/F", "/PID", String(child.pid)], { windowsHide: true, stdio: "ignore" }); } catch { /* already gone */ }
    return;
  }
  try { kill(-child.pid, "SIGKILL"); } catch { try { child.kill("SIGKILL"); } catch { /* already gone */ } }
}

/**
 * The same invocation as defaultSpawn without blocking, so scans can run side by side. Keeps the last 4 MiB of each stream.
 * With `options.signal` (an AbortSignal), an abort kills the whole process tree and the result carries `cancelled: true`.
 * With `options.stdoutFile`, stdout is written to that file whole instead of kept in memory.
 */
export function defaultSpawnAsync(command, args, options = {}) {
  const { signal, stdoutFile, onSpawn, ...rest } = options;
  const inv = resolveInvocation(command, args, { env: rest.env ?? process.env });
  if (inv.error) return Promise.resolve({ status: null, stdout: "", stderr: "", error: inv.error });
  if (signal?.aborted) return Promise.resolve({ status: null, stdout: "", stderr: "", error: new Error("cancelled"), cancelled: true });
  return new Promise((resolve) => {
    let stdout = ""; let stderr = ""; let error = null;
    let truncated = false;
    const keep = (buf, chunk) => { const next = buf + chunk; if (next.length > OUTPUT_KEEP) { truncated = true; return next.slice(-OUTPUT_KEEP); } return next; };
    let child;
    try {
      // stdin is closed: no scan reads it, and an open pipe nobody writes can block a tool that probes it (Windows).
      // A detached child (POSIX, its own group for the abort kill) is recorded under the held build ownership (build-ownership.mjs).
      child = spawnUnderOwnership(spawnChild, inv.file, inv.args, { windowsHide: true, windowsVerbatimArguments: inv.verbatim, stdio: ["ignore", "pipe", "pipe"], ...(signal && process.platform !== "win32" ? { detached: true } : {}), ...rest, shell: false, stdio: ["ignore", "pipe", "pipe"] });
    } catch (e) { resolve({ status: null, stdout: "", stderr: "", error: e }); return; }
    if (onSpawn && child.pid) onSpawn(child.pid);
    const onAbort = () => killProcessTree(child);
    signal?.addEventListener("abort", onAbort, { once: true });
    child.stdout.setEncoding("utf8"); child.stderr.setEncoding("utf8");
    const sink = stdoutFile ? fs.createWriteStream(stdoutFile) : null;
    if (sink) child.stdout.pipe(sink); else child.stdout.on("data", (c) => { stdout = keep(stdout, c); });
    child.stderr.on("data", (c) => { stderr = keep(stderr, c); });
    child.on("error", (e) => { error = e; });
    child.on("close", async (status) => {
      if (sink) await new Promise((done) => (sink.writableFinished ? done() : sink.on("finish", done).on("error", done)));
      signal?.removeEventListener("abort", onAbort);
      const cancelled = signal?.aborted === true;
      resolve({ status, stdout, stderr, ...(truncated ? { truncated: true } : {}), ...(error ? { error } : cancelled ? { error: new Error("cancelled") } : {}), ...(cancelled ? { cancelled: true } : {}) });
    });
    if (signal?.aborted) onAbort();
  });
}

// Lanes for concurrent class B runs. The broker admits check and clippy side by side; rustdoc and
// package are serialized by the broker anyway. Tests and repository code run after the compile lanes drain, `testJobs` at a
// time; a test scan marked `quiet` (ci/gate.config.json `rust.<crate>.quietTests`: the crate's tests share ports, devices,
// wall-clock budgets or the broker's own queue, so they fail under load) runs last, one at a time, under the broker's quiet
// lease when the gate can take one (runScans `quietLane`).
/** The counts a test runner printed last: node:test's `ℹ tests N` / `ℹ pass` / `ℹ fail` / `ℹ skipped` / `ℹ todo` lines, or vitest's
 * `Tests  N passed | M skipped | K failed (T)` line. Null when neither is present. Several node:test summaries (a script that runs
 * the runner more than once) are summed. */
/** null when a test summary is evidence of a pass; otherwise { code, detail }. Used for fresh and cached test rows alike. */
export function testResultsProblem(results) {
  if (!results) return { code: "NO_TEST_SUMMARY", detail: "no test counts recorded" };
  if (!(results.tests > 0)) return { code: "NO_TESTS", detail: "the test scan executed no test" };
  if (results.fail > 0) return { code: "SUMMARY_FAILED", detail: `the test runner exited 0 but its summary reports ${results.fail} failed test(s)` };
  // A suite that ran tests but passed none of them (every test skipped or todo) proves nothing (D17, rightkit#1236).
  if (!(results.pass > 0)) return { code: "ALL_SKIPPED", detail: `the test scan passed no test: ${results.skipped ?? 0} skipped, ${results.todo ?? 0} todo of ${results.tests}` };
  return null;
}

export function parseTestSummary(text) {
  const lines = String(text ?? "").split(/\r?\n/);
  const node = { tests: 0, pass: 0, fail: 0, skipped: 0, todo: 0 }; let sawNode = false;
  for (const line of lines) {
    const m = /^[ℹ#]\s+(tests|pass|fail|skipped|todo)\s+(\d+)\s*$/.exec(line.trim());
    if (m) { node[m[1]] += Number(m[2]); sawNode = true; }
  }
  if (sawNode) return { runner: "node:test", ...node };
  const vt = lines.map((l) => l.replace(/\x1b\[[0-9;]*m/g, "")).reverse().find((l) => /^\s*Tests\s+.*\(\d+\)\s*$/.test(l));
  if (vt) {
    const n = (re) => Number((re.exec(vt) ?? [0, 0])[1]);
    const tests = Number(/\((\d+)\)\s*$/.exec(vt)[1]);
    return { runner: "vitest", tests, pass: n(/(\d+) passed/), fail: n(/(\d+) failed/), skipped: n(/(\d+) skipped/), todo: n(/(\d+) todo/) };
  }
  return null;
}

/** The largest inline `run:` script a workflow may carry while the pinned actionlint is 1.7.12 (rhysd/actionlint#702). */
export const RUN_SCRIPT_LIMIT_BYTES = 3800;
/** Every block `run:` script (`run: |` or `run: >`) of a workflow text larger than `limit` bytes, as "<file>:<line> <bytes> bytes".
 * Line-based and conservative: the block is every following line indented deeper than the `run:` key (blank lines included),
 * with that common indentation removed, as YAML reads a literal block. */
export function oversizedRunScripts(text, file, limit = RUN_SCRIPT_LIMIT_BYTES) {
  const lines = String(text).split(/\r?\n/);
  const out = [];
  for (let i = 0; i < lines.length; i += 1) {
    const m = /^(\s*)(?:-\s+)?run:\s*[|>][-+0-9]*\s*$/.exec(lines[i]);
    if (!m) continue;
    const keyIndent = lines[i].indexOf("run:");
    const body = [];
    let j = i + 1;
    for (; j < lines.length; j += 1) {
      const line = lines[j];
      if (line.trim() === "") { body.push(""); continue; }
      if (line.length - line.trimStart().length <= keyIndent) break;
      body.push(line);
    }
    while (body.length && body[body.length - 1] === "") body.pop();
    const indent = Math.min(...body.filter(Boolean).map((l) => l.length - l.trimStart().length));
    const script = body.map((l) => l.slice(Number.isFinite(indent) ? indent : 0)).join("\n") + "\n";
    const bytes = Buffer.byteLength(script);
    if (bytes > limit) out.push(`${file}:${i + 1} ${bytes} bytes`);
    i = j - 1;
  }
  return out;
}

/** The time limit of one scan: its own `timeoutMs`, then the run's `scanTimeoutMs`, else 5 min for actionlint, 30 min for class A and
 * 60 min for class B (a cold compile of the largest crate). */
/** The broker's quiet lease ceiling (rustright-state quiet.rs QUIET_LEASE_MAX_TTL_MS) and the quiet suite timeout under it. */
export const QUIET_LEASE_MAX_MS = 30 * 60_000;
export const QUIET_SCAN_MAX_MS = 26 * 60_000;
/** A tool version probe's bound; with QUIET_SCAN_MAX_MS it keeps a whole leased quiet suite (probes + run) under the 30 min lease. */
export const PROBE_TIMEOUT_MS = 60_000;
export function scanTimeoutMs(scan, options = {}) {
  if (Number.isFinite(scan?.timeoutMs) && scan.timeoutMs > 0) return scan.timeoutMs;
  if (Number.isFinite(options.scanTimeoutMs) && options.scanTimeoutMs > 0) return options.scanTimeoutMs;
  if (scan?.tool === "actionlint") return 5 * 60_000;
  return scan?.class === SCAN_CLASS.B ? 60 * 60_000 : 30 * 60_000;
}

export function laneOf(scan) {
  if (/^(clippy|check|check-features|check-dependent):/.test(scan.id)) return "compile";
  if (/^doc:/.test(scan.id)) return "doc";
  return scan.quiet === true ? "quiet" : "tests";
}

// The lines a reader needs from a failed scan: every line that names a failure (a failed test, a panic, a compiler or tool error,
// cargo test's `failures:` block header, pnpm's ERR_ codes), at most twelve of them, then the last four lines of stdout and of
// stderr. cargo test writes results to stdout and "Running ..." to stderr, so a plain tail showed only harness chatter.
const FAILURE_LINE = /(^|[\s\[])(FAILED|panicked at|error(\[E\d+\])?:|error\[|^failures:|ERR_PNPM_\w+|Error:|assertion `?left|^thread '.*' panicked)/;
export function failureDetail(r) {
  const lines = (text) => String(text ?? "").split(/\r?\n/).map((l) => l.trimEnd()).filter((l) => l.trim());
  const out = lines(r?.stdout); const err = lines(r?.stderr);
  const named = [...out, ...err].filter((l) => FAILURE_LINE.test(l)).slice(0, 12);
  const ends = [...out.slice(-4), ...err.slice(-4)];
  return [...new Set([...named, ...ends])].join(" | ");
}
const tail = failureDetail;

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
  // A Promise has no status: an async spawn here would report a present rustfmt as missing.
  if (typeof which?.then === "function") throw new TypeError("findRustfmt needs a synchronous spawn");
  return which && which.status === 0 ? String(which.stdout).split(/\r?\n/)[0].trim() : null;
}

async function defaultLoadToml() {
  const mod = await import("smol-toml");
  return mod.parse;
}
// Parsers come from this package's own trusted location (the runner's copy), never from the content root,
// which is submitted code. `smol-toml` resolves through this package's own dependencies.
const TRUSTED_YAML_PARSER = fileURLToPath(new URL("./workflow-yaml.mjs", import.meta.url));
async function defaultLoadYaml() {
  if (!fs.existsSync(TRUSTED_YAML_PARSER)) throw new Error(`strict YAML parser not found at ${TRUSTED_YAML_PARSER}`);
  return (await import(pathToFileURL(TRUSTED_YAML_PARSER).href)).parseYaml;
}
async function defaultScanSecrets() {
  return (await import("./publish-cargo.mjs")).scanCrateForSecrets;
}

/**
 * For a content root that is a git worktree's toplevel: a predicate over absolute paths that is true for every git-ignored file or
 * directory there (`git ls-files --others --ignored --exclude-standard --directory`, which lists an ignored directory once, with a
 * trailing slash). Null when the root is not a git toplevel (a committed export) or git cannot answer: then nothing is skipped.
 */
export async function ignoredPathSkip(ctx) {
  const git = async (args) => { try { return await ctx.deps.spawn("git", args, { cwd: ctx.contentRoot, encoding: "utf8", env: ctx.deps.env }); } catch { return null; } };
  const top = await git(["rev-parse", "--show-toplevel"]);
  if (!top || top.status !== 0) return null;
  const same = (a, b) => { try { return fs.realpathSync(a) === fs.realpathSync(b); } catch { return path.resolve(a) === path.resolve(b); } };
  if (!same(String(top.stdout ?? "").trim(), ctx.contentRoot)) return null;
  const listed = await git(["ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--directory"]);
  if (!listed || listed.status !== 0) return null;
  const entries = String(listed.stdout ?? "").split("\0").filter(Boolean);
  const dirs = entries.filter((e) => e.endsWith("/"));
  const files = new Set(entries.filter((e) => !e.endsWith("/")));
  const root = ctx.contentRoot;
  return (absolute) => {
    const rel = path.relative(root, absolute).replaceAll("\\", "/");
    if (!rel || rel.startsWith("..")) return false;
    return files.has(rel) || dirs.some((d) => rel === d.slice(0, -1) || rel.startsWith(d));
  };
}

// What rustfmt needs from a Cargo.toml, read by table so an `edition` under [package.metadata.x] or [dependencies.x] is not the
// package's: `pkg` is the [package] edition (a literal year, or "workspace" for `edition.workspace = true` / `edition = { workspace = true }`),
// `ws` the [workspace.package] edition literal, `isWorkspace` whether the manifest declares a [workspace]. An `edition` entry in one of
// those two tables that is none of the recognised spellings (`edition = 2021`, `edition.workspace = false`) is kept as `pkgBad` / `wsBad`
// (the line), never skipped, so it cannot fall back to a default. Keys before any table header count as [package] keys (the fixtures of
// this package's tests write manifests that way; cargo itself rejects them).
export function cargoEditions(text) {
  const out = { pkg: undefined, ws: undefined, pkgBad: undefined, wsBad: undefined, isWorkspace: false };
  let table = "";
  for (const raw of String(text).split(/\r?\n/)) {
    const header = /^\s*\[\[?([^\]]+)\]\]?\s*(?:#.*)?$/.exec(raw);
    if (header) {
      table = header[1].trim();
      if (table === "workspace" || table.startsWith("workspace.")) out.isWorkspace = true;
      continue;
    }
    if (table !== "" && table !== "package" && table !== "workspace.package") continue;
    const line = raw.replace(/^(\s*)(["'])edition\2/, "$1edition");
    if (!/^\s*edition\s*(?:[.=]|$)/.test(line)) continue;
    let value;
    if (/^\s*edition\.workspace\s*=\s*true\s*(?:#.*)?$/.test(line) || /^\s*edition\s*=\s*\{\s*workspace\s*=\s*true\s*\}\s*(?:#.*)?$/.test(line)) value = "workspace";
    else value = (/^\s*edition\s*=\s*(?:"(\d{4})"|'(\d{4})')\s*(?:#.*)?$/.exec(line) || []).slice(1).find(Boolean);
    if (table === "workspace.package") {
      if (value !== undefined && value !== "workspace") out.ws = value; else out.wsBad = raw.trim();
    } else if (value !== undefined) out.pkg = value;
    else out.pkgBad = raw.trim();
  }
  return out;
}

/**
 * The edition rustfmt must run a Rust file under: `{ edition }`, or `{ manifest, problem }` when it cannot be resolved.
 * The nearest Cargo.toml at or above the file decides. A literal `edition = "NNNN"` is that edition; `edition.workspace = true`
 * (or `edition = { workspace = true }`) is the `[workspace.package] edition` of the owning workspace root, the nearest manifest at or
 * above it that declares `[workspace]`, which is the manifest itself for a root package. An inheritance that cannot be followed
 * (no workspace root, an unreadable manifest, or a root with no literal `[workspace.package] edition`) is a `problem`, never a default.
 * A manifest that names no edition at all, and a file below no manifest, keep rustfmt's "2021" as before.
 */
function rustEditionResolver({ contentRoot, exists, readFile }) {
  const root = path.normalize(contentRoot);
  const inside = (dir) => { const rel = path.relative(root, dir); return rel === "" || !(rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)); };
  const rel = (dir) => posix(path.relative(root, path.join(dir, "Cargo.toml")));
  const parsed = new Map();
  const manifestAt = (dir) => {
    if (!parsed.has(dir)) {
      let value;
      try { value = cargoEditions(readFile(path.join(dir, "Cargo.toml"))); } catch (e) { value = { unreadable: String(e?.message ?? e).split("\n")[0] }; }
      parsed.set(dir, value);
    }
    return parsed.get(dir);
  };
  const ancestors = function* (start) {
    for (let dir = start; inside(dir); dir = path.dirname(dir)) {
      yield dir;
      if (path.dirname(dir) === dir) return;
    }
  };
  return (file) => {
    const dir = [...ancestors(path.dirname(path.join(root, file)))].find((d) => exists(path.join(d, "Cargo.toml")));
    if (dir === undefined) return { edition: "2021" };
    const manifest = rel(dir);
    const info = manifestAt(dir);
    if (info.unreadable) return { manifest, problem: `${manifest} cannot be read (${info.unreadable}), so its edition is unknown` };
    if (info.pkgBad !== undefined) return { manifest, problem: `${manifest} has an edition entry the scan cannot read (${info.pkgBad}); write edition = "NNNN" or edition.workspace = true` };
    if (info.pkg === undefined) return { edition: info.ws ?? "2021" };
    if (info.pkg !== "workspace") return { edition: info.pkg };
    const inherits = `${manifest} sets edition.workspace = true`;
    for (const up of ancestors(dir)) {
      if (!exists(path.join(up, "Cargo.toml"))) continue;
      const w = manifestAt(up);
      if (w.unreadable) return { manifest, problem: `${inherits}, but ${rel(up)} cannot be read (${w.unreadable}), so its edition is unknown` };
      if (!w.isWorkspace) continue;
      if (w.wsBad !== undefined) return { manifest, problem: `${inherits}, but its workspace root ${rel(up)} has a [workspace.package] edition entry the scan cannot read (${w.wsBad})` };
      if (w.ws === undefined) return { manifest, problem: `${inherits}, but its workspace root ${rel(up)} declares no literal [workspace.package] edition = "NNNN" to inherit` };
      return { edition: w.ws };
    }
    return { manifest, problem: `${inherits}, but no Cargo.toml at or above it declares a [workspace], so there is no [workspace.package] edition to inherit` };
  };
}

// A crate that can never be published (`publish = false` or `publish = []` in its [package] table) needs no license key, like a
// package.json with `"private": true`; cargo-manifest-text.mjs holds the one reading of that rule, which the secret scan shares.
function cargoUnpublished(text) { return cargoManifestState(text).unpublished; }

const INTERNAL = {
  "json-parse": (s, ctx) => {
    const bad = [];
    for (const f of s.files) {
      // tsconfig/jsconfig and editor settings are JSON with comments by their own spec (TypeScript reads them that way).
      const jsonc = /(^|\/)(tsconfig|jsconfig)(\.[^/]*)?\.json$|(^|\/)\.vscode\/[^/]+\.json$/.test(f);
      try { const text = ctx.read(f).replace(/^\uFEFF/, ""); JSON.parse(jsonc ? stripJsonComments(text) : text); } catch (e) { bad.push(`${f}: ${e.message}`); }
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
  "advisory-exceptions": (s, ctx) => (s.dir === undefined
    ? checkAdvisoryExceptions({ root: ctx.contentRoot, readFile: ctx.read })
    : checkAdvisoryExceptions({ root: path.join(ctx.contentRoot, s.dir), readFile: (p) => ctx.read(path.join(s.dir, p)), exceptionsFile: s.exceptionsFile }).map((problem) => `${s.dir || "."}: ${problem}`)),
  // Every Right Suite API projection must equal what the generator writes from the contract. The pure check is imported from the
  // scanner's own checkout (scripts/ sits beside packages/release), never from the content root.
  "suite-api": async (s, ctx) => {
    const { suiteApiProblems } = await import("../../scripts/gen-suite-api.mjs");
    return suiteApiProblems((f) => ctx.read(f));
  },
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
        if (!/^\s*(license|license-file)\s*(=|\.workspace)/m.test(text) && !cargoUnpublished(text)) bad.push(`${f}: no license or license-file`);
      } else {
        try { const m = JSON.parse(text); if (m.private !== true && !(typeof m.license === "string" && m.license.trim()) && !m.licenses) bad.push(`${f}: no license field`); } catch (e) { bad.push(`${f}: ${e.message}`); }
      }
    }
    return bad;
  },
  // The repository gate reads env templates and test vectors against the reviewed records of the gated commit
  // (.rightkit/secret-scan-records.json, read from the content root by the scanner); crate packaging stays strict.
  // Each violation names its repository-relative path, so a unit that is both a crate and a package reports a file once.
  "secret-scan": async (s, ctx) => {
    const scan = await ctx.deps.scanSecrets();
    // A committed export holds only tracked files. A live tree (--to dev|test: the content root is the checkout itself) also holds
    // git-ignored build output (.right-release/, DerivedData, a vendored checkout under it), which the gate never judges: the scanner
    // skips what `git ls-files --ignored` names there. Outside a git toplevel nothing is skipped (Pulse, 2026-10-27).
    const skip = await ignoredPathSkip(ctx);
    const bad = new Set();
    for (const dir of s.dirs) {
      const abs = path.join(ctx.contentRoot, dir);
      if (!ctx.deps.exists(abs)) { bad.add(`${dir}: directory not found`); continue; }
      for (const v of scan(abs, { envTemplates: true, repoRoot: ctx.contentRoot, skip })) bad.add(v);
    }
    return [...bad];
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
    spawn: Number(options.jobs ?? 1) > 1 || options.signal ? defaultSpawnAsync : defaultSpawn,
    // Locating a tool (rustfmt's `which`/`where` fallback) is synchronous whatever `spawn` is.
    lookupSpawn: defaultSpawn, exists: fs.existsSync, readFile: (f) => fs.readFileSync(f, "utf8"), readdir: fs.readdirSync,
    env: process.env, platform: process.platform, execPath: process.execPath,
    loadToml: defaultLoadToml, loadYaml: defaultLoadYaml, scanSecrets: defaultScanSecrets, findRustfmt,
    ...options,
  };
  const classes = options.classes ?? [SCAN_CLASS.A, SCAN_CLASS.B];
  const exclude = options.exclude ?? (() => false);
  // An AbortSignal: once aborted, no further scan starts, running children are killed (process tree) and every unfinished scan is "not-run" with code CANCELLED.
  const signal = options.signal; const sig = signal ? { signal } : {};
  const { contentRoot, liveRoot, cargoLiveRoot = null } = plan.roots;
  const rootDir = (r) => (r === "content" ? contentRoot : r === "parent" ? path.dirname(contentRoot) : r === "cargoLive" ? (cargoLiveRoot ?? liveRoot) : liveRoot);
  const selected = plan.scans.filter((s) => classes.includes(s.class) && !exclude(s));
  const ordered = [...selected.filter((s) => s.class === SCAN_CLASS.A), ...selected.filter((s) => s.class === SCAN_CLASS.B)];
  const ctx = { deps, contentRoot, read: (f) => deps.readFile(path.join(contentRoot, f)) };
  const toolVersions = {}; const probed = new Map(); const advisoryDbs = new Map();
  const results = [];
  // options.cache = { read(key), write(key, row), digest(scan) => string | null } (null: not cacheable, which is how the live class A checks opt out).
  const cache = options.cache && typeof options.cache.read === "function" && typeof options.cache.write === "function" && typeof options.cache.digest === "function" ? options.cache : null;
  const cacheHits = [];
  let classAFailed = false;
  // Class B needs class A first: in this call (classes includes A) or valid prior class A evidence for the
  // same class A scan set (options.priorClassA: an earlier runScans result whose class A rows all passed).
  const priorA = options.priorClassA;
  const priorRows = Array.isArray(priorA?.scans) ? priorA.scans.filter((r) => r.class === SCAN_CLASS.A) : [];
  // Prior evidence binds to the input content (inputDigest of the changed files), the class A scan set, and a
  // class A run that actually included class A.
  // ... and to the checker that produced it: evidence from another checker (other version, or same version with changed bytes)
  // does not unlock class B, and an unknown identity on either side refuses.
  const checker = CHECKER_IDENTITY;
  const priorClassAValid = Boolean(priorA) && priorA.schema === SCAN_RESULT_SCHEMA && Boolean(plan.classDigests?.A) && Boolean(plan.inputDigest)
    && priorA.inputDigest === plan.inputDigest && Array.isArray(priorA.classes) && priorA.classes.includes(SCAN_CLASS.A)
    && priorA.classDigests?.A === plan.classDigests.A && digestScanSet(priorRows) === plan.classDigests.A && priorRows.every((r) => r.status === "pass")
    && typeof checker === "string" && priorA.checker === checker;
  // Class A ran "in this call" only when it was selected, the plan has class A scans, and none was excluded:
  // excluding every class A scan (or a plan with none) never unlocks class B; explicit prior evidence is needed.
  const planA = plan.scans.filter((x) => x.class === SCAN_CLASS.A);
  const inCallA = classes.includes(SCAN_CLASS.A) && planA.length > 0 && selected.filter((x) => x.class === SCAN_CLASS.A).length === planA.length;
  const classBBlocked = !inCallA && !priorClassAValid;

  const toolCommand = (tool) => (tool === "node" ? deps.execPath : tool === "rustfmt" ? deps.findRustfmt({ env: deps.env, exists: deps.exists, readdir: deps.readdir, platform: deps.platform, spawn: deps.lookupSpawn }) : tool);
  async function probe(tool, scanDir = null) {
    if (probed.has(tool)) return probed.get(tool);
    let outcome;
    const command = toolCommand(tool);
    if (!command) outcome = { ok: false, why: "not found" };
    else {
      // A version probe never runs inside the repository: a tool started there reads the repository's own configuration first
      // (pnpm's .npmrc and pnpmfile, corepack's packageManager pin, cargo's .cargo/config), before any scan's protections apply.
      // rustright is the one exception: the RustRight client refuses to run anywhere that is not a Cargo workspace inside a repository
      // (RK_NOT_PRIMARY_CHECKOUT, "workspace discovery failed"), so from a neutral directory every cargo scan read as TOOL_MISSING on
      // every broker host (rightkit#1215). Its probe runs where the scan itself will run; `cargo --version` reads no repository config.
      const cwd = (tool === "rustright" || (tool === "cargo" && cargoLiveRoot)) && scanDir ? scanDir : (deps.probeDir ?? probeDir());
      // A version probe is bounded (PROBE_TIMEOUT_MS): a hung tool reads as unavailable instead of holding a lane, or a quiet
      // lease, open.
      const probeMs = Number.isFinite(options.probeTimeoutMs) && options.probeTimeoutMs > 0 ? options.probeTimeoutMs : PROBE_TIMEOUT_MS;
      const probeSignal = signal ? AbortSignal.any([signal, AbortSignal.timeout(probeMs)]) : AbortSignal.timeout(probeMs);
      const r = await deps.spawn(command, PROBES[tool] ?? ["--version"], { cwd, env: { ...deps.env, ...PNPM_NO_HOOKS_ENV, COREPACK_ENABLE_AUTO_PIN: "0" }, signal: probeSignal });
      if (!r || r.error || r.status !== 0) outcome = { ok: false, why: r?.error?.message ?? `exit ${r?.status}` };
      else {
        const text = String(r.stdout || r.stderr || "");
        const version = PROBE_VERSION[tool] ? PROBE_VERSION[tool](text) : text.split(/\r?\n/).find((l) => l.trim())?.trim() ?? "unknown";
        outcome = { ok: true, command, version: version ?? "unknown", pinProblem: pinnedVersionProblem(tool, version) };
      }
    }
    toolVersions[tool] = outcome.ok ? outcome.version : null;
    probed.set(tool, outcome);
    return outcome;
  }

  const jobs = Math.max(1, Number(options.jobs ?? 1) || 1);
  const testJobs = Math.max(1, Number(options.testJobs ?? jobs) || jobs);
  // { acquire: async () => ({ id, env }) | null, release: async (id) => {} }: the broker's quiet lease around the quiet lane. A
  // failed acquire runs the lane without one (the caller reports it); the lease is released however the lane ends.
  const quietLane = options.quietLane && typeof options.quietLane.acquire === "function" ? options.quietLane : null;
  const onResult = typeof options.onResult === "function" ? options.onResult : () => {};
  const total = ordered.length; let done = 0;
  // Every class A failure latches, whichever branch produced it (Astra, 2026-10-26: two early returns bypassed the latch below, so a
  // missing advisory database or a failed deny metadata could be followed by a class B compile). Class B reads the latch before it starts.
  async function runOne(s, extraEnv = null) {
    const row = await runOneBody(s, extraEnv);
    if (row.status === "fail" && s.class === SCAN_CLASS.A) classAFailed = true;
    return row;
  }
  async function runOneBody(s, extraEnv = null) {
    if (signal?.aborted) return { id: s.id, class: s.class, command: s.command, tool: s.tool ?? null, toolVersion: null, status: "not-run", code: "CANCELLED", detail: "cancelled before it started" };
    const started = Date.now();
    const row = await scanRow(s, extraEnv);
    done += 1;
    try { onResult(row, { done, total, seconds: Math.round((Date.now() - started) / 1000) }); } catch { /* reporting never fails a scan */ }
    return row;
  }
  // Class A may run concurrently. Class B runs one scan per component at a time, `classBJobs` components at once (below).
  const aScans = ordered.filter((s) => s.class === SCAN_CLASS.A);
  const bScans = ordered.filter((s) => s.class === SCAN_CLASS.B);
  // Class A scans are independent content checks (and short broker calls: metadata, package list, deny, audit): they run
  // `classAJobs` at a time (default: `jobs`); the rows keep plan order. Class B starts only once every class A row is in.
  const aJobs = Math.max(1, Number(options.classAJobs ?? jobs) || jobs);
  {
    // Class A is everything mechanical and deterministic that does not compile (Adrian, 2026-10-27): the content checks first,
    // `classAJobs` at a time, then the class A tests (kind "test"), `testJobs` at a time, then the quiet ones alone under the
    // broker's quiet lease. A failed content check stops the tests: they run on content class A already judged.
    const slots = new Array(aScans.length);
    const content = []; const tests = []; const quietTests = [];
    aScans.forEach((s, i) => (s.kind === "test" ? (s.quiet === true ? quietTests : tests) : content).push(i));
    const drain = (queue, extraEnv = null) => async () => {
      while (queue.length) {
        const i = queue.shift();
        slots[i] = aScans[i].kind === "test" && classAFailed
          ? { id: aScans[i].id, class: aScans[i].class, command: aScans[i].command, tool: aScans[i].tool ?? null, toolVersion: null, status: "not-run", detail: "a class A content check failed; the class A tests start only after it passes" }
          : await runOne(aScans[i], extraEnv);
      }
    };
    await Promise.all(Array.from({ length: aJobs }, drain(content)));
    await Promise.all(Array.from({ length: testJobs }, drain(tests)));
    if (quietTests.length) {
      // One broker quiet lease per quiet suite (the broker caps a lease at 30 min and has no renewal): each suite runs alone,
      // under its own lease, with a timeout shorter than the lease, so a lease never expires while its suite still runs.
      for (const i of quietTests) {
        if (classAFailed || signal?.aborted) {
          slots[i] = { id: aScans[i].id, class: aScans[i].class, command: aScans[i].command, tool: aScans[i].tool ?? null, toolVersion: null, status: "not-run", detail: signal?.aborted ? "cancelled" : "a class A check failed; the quiet suites start only on a green class A" };
          continue;
        }
        let lease = null;
        if (quietLane) { try { lease = (await quietLane.acquire({ ttlMs: QUIET_LEASE_MAX_MS })) ?? null; } catch { lease = null; } }
        if (!lease && options.requireQuietLease) {
          slots[i] = { id: aScans[i].id, class: aScans[i].class, command: aScans[i].command, tool: aScans[i].tool ?? null, toolVersion: null, status: "fail", code: "QUIET_LEASE_UNAVAILABLE", detail: "the broker's quiet lease could not be held; this conflict-prone suite does not run alongside other jobs on a managed host. Run again when the broker can grant the lease." };
          classAFailed = true; // a refused class A suite is a red class A: no class B scan starts after it
          continue;
        }
        try { slots[i] = await runOne({ ...aScans[i], timeoutMs: Math.min(scanTimeoutMs(aScans[i], options), QUIET_SCAN_MAX_MS) }, lease?.env ?? null); } finally {
          if (lease && typeof quietLane.release === "function") { try { await quietLane.release(lease.id); } catch { /* the lease expires on its own */ } }
        }
      }
    }
    results.push(...slots);
  }
  {
    const slots = new Array(bScans.length);
    const lanes = { compile: [], doc: [], tests: [], quiet: [] };
    bScans.forEach((s, i) => lanes[laneOf(s)].push(i));
    // RightKit is many components, never one build (Adrian, 2026-10-29; rightkit#1285): class B scans of different components run
    // concurrently, `classBJobs` at a time (the broker queues beyond its build capacity). A component is the directory a scan runs in
    // (`cwd`, its own Cargo workspace and output directory); scans of one component stay in plan order, because they share its
    // output directory and cargo locks it.
    const bJobs = Math.max(1, Number(options.classBJobs ?? 1) || 1);
    const drain = async (queue, extraEnv = null) => {
      const busy = new Set(); const running = new Set();
      const keyOf = (i) => bScans[i].cwd ?? ".";
      while (queue.length || running.size) {
        const at = running.size < bJobs ? queue.findIndex((i) => !busy.has(keyOf(i))) : -1;
        if (at === -1) { await Promise.race(running); continue; }
        const [i] = queue.splice(at, 1); const key = keyOf(i); busy.add(key);
        const job = runOne(bScans[i], extraEnv).then((row) => { slots[i] = row; }).finally(() => { busy.delete(key); running.delete(job); });
        running.add(job);
      }
    };
    await drain(lanes.compile);
    await drain(lanes.doc);
    await drain(lanes.tests);
    if (lanes.quiet.length) {
      // One lease per quiet suite, as in class A above.
      for (const i of lanes.quiet) {
        if (classAFailed || classBBlocked || signal?.aborted) {
          slots[i] = { id: bScans[i].id, class: bScans[i].class, command: bScans[i].command, tool: bScans[i].tool ?? null, toolVersion: null, status: "not-run", detail: signal?.aborted ? "cancelled" : classAFailed ? "class A is red; nothing in class B starts" : "class B is blocked" };
          continue;
        }
        let lease = null;
        if (quietLane) { try { lease = (await quietLane.acquire({ ttlMs: QUIET_LEASE_MAX_MS })) ?? null; } catch { lease = null; } }
        if (!lease && options.requireQuietLease) {
          slots[i] = { id: bScans[i].id, class: bScans[i].class, command: bScans[i].command, tool: bScans[i].tool ?? null, toolVersion: null, status: "fail", code: "QUIET_LEASE_UNAVAILABLE", detail: "the broker's quiet lease could not be held; this conflict-prone suite does not run alongside other jobs on a managed host. Run again when the broker can grant the lease." };
          continue;
        }
        try { slots[i] = await runOne({ ...bScans[i], timeoutMs: Math.min(scanTimeoutMs(bScans[i], options), QUIET_SCAN_MAX_MS) }, lease?.env ?? null); } finally {
          if (lease && typeof quietLane.release === "function") { try { await quietLane.release(lease.id); } catch { /* the lease expires on its own */ } }
        }
      }
    }
    results.push(...slots);
  }

  // The pinned advisory database, materialised once per run on first use (a failure is remembered and refuses every advisory scan).
  function prepareAdvisory(s) {
    const key = s.advisoryPin.commit;
    if (!advisoryDbs.has(key)) {
      try {
        const ensure = deps.ensureAdvisoryDb ?? ensureAdvisoryDb;
        advisoryDbs.set(key, { dir: ensure({ cacheRoot: deps.advisoryCacheRoot ?? advisoryCacheRoot({ env: deps.env, platform: deps.platform }), pin: s.advisoryPin }) });
      } catch (e) { advisoryDbs.set(key, { error: e }); }
    }
    const db = advisoryDbs.get(key);
    if (db.error) throw db.error;
    if (s.advisoryDb !== "deny") return { dbDir: db.dir };
    const prepare = deps.prepareDenyOverlay ?? prepareDenyOverlay;
    let denyToml = "";
    try { denyToml = deps.readFile(path.join(rootDir(s.root), s.cwd ?? "", "deny.toml")); } catch {
      // No deny.toml in the workspace: RightKit's own policy (deny-default.toml) applies, never cargo-deny's empty default,
      // which allows no licence at all.
      denyToml = deps.defaultDenyToml ?? fs.readFileSync(new URL("./deny-default.toml", import.meta.url), "utf8");
    }
    const overlay = prepare({ denyToml, dbDir: db.dir, pin: s.advisoryPin });
    return { dbDir: db.dir, configPath: overlay.configPath, cleanup: overlay.cleanup };
  }

  // `extraEnv` (the quiet lease id) reaches the child but never the cache key: a lease changes nothing a scan proves.
  async function scanRow(s, extraEnv = null) {
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
        const scanDir = path.join(rootDir(s.root), s.cwd ?? "");
        const p = await probe(s.tool, scanDir);
        row.toolVersion = p.ok ? p.version : null;
        let missingDep = null; let depVersionProblem = null; const depVersions = [];
        for (const dep of s.requires ?? []) {
          const d = await probe(dep, scanDir);
          if (!d.ok) { missingDep = `${dep} (${d.why})`; break; }
          if (d.pinProblem) { depVersionProblem = d.pinProblem; break; }
          depVersions.push(d.version);
        }
        if (!p.ok) { status = "fail"; code = "TOOL_MISSING"; detail = `required tool ${s.tool} is missing or not runnable (${p.why}); a required scan is never skipped`; }
        else if (p.pinProblem) { status = "fail"; code = "TOOL_VERSION"; detail = `TOOL_VERSION: ${p.pinProblem} (at ${p.command})`; }
        else if (missingDep) { status = "fail"; code = "TOOL_MISSING"; detail = `${s.tool} needs ${missingDep}, which is missing or not runnable; without it the scan would pass unchecked, so it fails`; }
        else if (depVersionProblem) { status = "fail"; code = "TOOL_VERSION"; detail = `TOOL_VERSION: ${s.tool} runs ${depVersionProblem}`; }
        else {
          let cacheKey = null;
          // A cargo request that builds the commit snapshot through the broker (options.snapshotBroker) is never served from or written
          // to the scan cache: each run must carry its own request and verify the broker's own signed evidence.
          const viaSnapshotBroker = Boolean(deps.snapshotBroker) && s.root === "cargoLive" && (s.tool === "rustright" || s.tool === "cargo");
          if (cache && !viaSnapshotBroker) {
            try {
              const digest = cache.digest(s);
              // The analyzers a scan runs through (actionlint's shellcheck) judge it too, so their versions key it as well.
              const toolVersion = [p.version, ...depVersions].join(" + ");
              if (digest) cacheKey = scanCacheKey({ id: s.id, command: s.command, toolVersion, env: s.env ?? {}, inheritedEnv: inheritedEnvDigest({ ...deps.env, ...s.env }), digest });
            } catch { cacheKey = null; }
            const hit = cacheKey ? cache.read(cacheKey) : null;
            // A cached test pass counts only with the counts it recorded: without them its coverage is unknown, so it runs again.
            // ...and only if it passed at least one test: a cached all-skipped "pass" (written before D17) runs again (rightkit#1236).
            const testEvidence = s.kind !== "test" || testResultsProblem(hit?.row?.results) === null;
            if (hit && hit.key === cacheKey && hit.row?.status === "pass" && hit.row.id === s.id && hit.row.command === s.command && testEvidence) {
              cacheHits.push(s.id);
              return { ...row, status: "pass", cached: true, detail: `cached ${String(hit.at ?? "").slice(0, 10) || "earlier"}`, ...(s.kind === "test" ? { results: hit.row.results } : {}) };
            }
          }
          let advisory = null; let args = s.args;
          if (s.advisoryDb) {
            try { advisory = prepareAdvisory(s); } catch (e) { return { ...row, status: "fail", code: "ADVISORY_DB", detail: e.message }; }
            // cargo-deny --frozen (--offline + --locked: the nested `cargo fetch` it runs may not rewrite Cargo.lock, so the broker admits it as analysis) must not fetch the advisory database, but its own `cargo metadata` would then be offline too and
            // fail on a runner whose cargo cache is cold. The workspace's metadata is resolved first, online and --locked (the
            // lockfile decides every version), and handed to cargo-deny as a file.
            if (s.advisoryDb === "deny") {
              const at = args.indexOf("deny");
              // The whole document is needed (the concurrent spawn keeps only the tail of stdout), so it streams to the file; a
              // spawn that does not know `stdoutFile` returns stdout, which is written below.
              // cargo-deny --frozen reads crate sources only from the local cargo cache, and `cargo metadata` does not download them
              // (fullA-19: rightkit-qa 0.9.1 was missing). They are fetched first, online and --locked (bound to the lockfile, so the
              // broker admits it as analysis). A fetch that fails, is refused or times out fails the row before metadata or deny run:
              // an offline or unreachable registry is red, never green.
              const fetchLimit = scanTimeoutMs(s, options);
              const fetchTimer = new AbortController();
              const fetchClock = setTimeout(() => fetchTimer.abort(), fetchLimit);
              let fetched;
              try { fetched = await deps.spawn(p.command, [...args.slice(0, at), "fetch", "--locked"], { cwd: path.join(rootDir(s.root), s.cwd ?? ""), env: { ...deps.env, ...s.env }, signal: signal ? AbortSignal.any([signal, fetchTimer.signal]) : fetchTimer.signal, timeout: fetchLimit }); } finally { clearTimeout(fetchClock); }
              if (!fetched || fetched.error || fetched.status !== 0) {
                advisory.cleanup?.();
                if (signal?.aborted) return { ...row, status: "not-run", code: "CANCELLED", detail: "cancelled while running" };
                if (fetchTimer.signal.aborted || fetched?.error?.code === "ETIMEDOUT") return { ...row, status: "fail", code: "TIMEOUT", detail: `cargo fetch --locked for cargo-deny ran longer than ${Math.round(fetchLimit / 60000)} min and was killed with its process tree` };
                return { ...row, status: "fail", code: "FETCH", detail: `cargo fetch --locked for cargo-deny failed (crate sources must be local before deny): ${fetched?.error?.message ?? (tail(fetched) || `exit ${fetched?.status}`)}` };
              }
              const metadataDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-deny-metadata-"));
              const metadataFile = path.join(metadataDir, "metadata.json");
              const metadata = await deps.spawn(p.command, [...args.slice(0, at), ...metadataArgs({ noDeps: false })], { cwd: path.join(rootDir(s.root), s.cwd ?? ""), env: { ...deps.env, ...s.env }, stdoutFile: metadataFile, ...sig });
              // AUX_METADATA: the auxiliary metadata that feeds deny could not be produced; not a deny finding, so never a deterministic red (rightkit#1257).
              if (!metadata || metadata.error || metadata.status !== 0) { fs.rmSync(metadataDir, { recursive: true, force: true }); advisory.cleanup?.(); return { ...row, status: "fail", code: "AUX_METADATA", detail: `cargo metadata for cargo-deny failed: ${metadata?.error?.message ?? (tail(metadata) || `exit ${metadata?.status}`)}` }; }
              if (!fs.existsSync(metadataFile)) fs.writeFileSync(metadataFile, String(metadata.stdout ?? ""));
              advisory.metadataPath = metadataFile;
              const overlayCleanup = advisory.cleanup;
              advisory.cleanup = () => { fs.rmSync(metadataDir, { recursive: true, force: true }); overlayCleanup?.(); };
            }
            args = args.map((a) => (a === "{advisoryDb}" ? advisory.dbDir : a === "{advisoryDenyConfig}" ? advisory.configPath : a === "{advisoryMetadata}" ? advisory.metadataPath : a));
          }
          let r;
          // actionlint 1.7.12 writes each run: script to shellcheck's stdin before starting it (rhysd/actionlint#702): a script over the
          // pipe buffer (~4 KB on Windows) deadlocks. Oversized scripts are refused here, before actionlint starts, so it cannot hang.
          if (s.runScriptLimit) {
            const over = s.runScriptLimit.files.flatMap((file) => { try { return oversizedRunScripts(ctx.read(file), file, s.runScriptLimit.bytes); } catch { return [`${file}: unreadable`]; } });
            if (over.length) return { ...row, status: "fail", code: "RUN_SCRIPT_TOO_LARGE", detail: `run: scripts over ${s.runScriptLimit.bytes} bytes (actionlint 1.7.12 deadlocks on them, rhysd/actionlint#702; split the step): ${over.join("; ")}` };
          }
          // Every scan has a time limit (fails closed, names the scan, kills the process tree): a hung tool is a check that never
          // fails, i.e. a run that never ends (actionlint on the Dell, 2026-10-27).
          // A tsc row that names TSC_PROJECT checks its package against the committed sources of its workspace siblings: the project is
          // generated outside the checked tree and removed after the run, so the check reads nothing a commit lacks and writes nothing.
          let tscProject = null;
          if (args.includes(TSC_PROJECT)) {
            try { tscProject = writeTscProject({ contentRoot, packageDir: path.join(rootDir(s.root), s.cwd ?? ""), workspace: s.tscWorkspace ?? [] }); }
            catch (e) { advisory?.cleanup?.(); return { ...row, status: "fail", code: "TSC_PROJECT", detail: `${s.id}: the check project could not be built from the committed workspace: ${e.message}` }; }
            args = args.map((a) => (a === TSC_PROJECT ? tscProject.file : a));
          }
          const limitMs = scanTimeoutMs(s, options);
          const timer = new AbortController();
          const clock = setTimeout(() => timer.abort(), limitMs);
          const scanSignal = signal ? AbortSignal.any([signal, timer.signal]) : timer.signal;
          // A class A test row runs under the launch guard (class-a-launch-guard.mjs): its Node processes may not launch a compile.
          // Every class A row that executes repository code (tests, lint/typecheck with repository config, generators) runs under the
          // launch guard: class A runs before any compile, so none of them may start one (joint plan step 4; inventory gap 3).
          const guardEnv = s.class === SCAN_CLASS.A && (s.kind === "test" || s.executes === true) ? launchGuardEnv({ ...deps.env, ...s.env, ...(extraEnv ?? {}) }) : {};
          // The broker snapshot carrier (joint plan step 4d): this one cargo request names the commit, the snapshot, the ownership
          // handle and its own argv and working directory, under a fresh nonce; the ownership records the submit first.
          const finalArgs = args.map((a) => (a === "{contentRoot}" ? contentRoot : a));
          let carrier = null;
          if (viaSnapshotBroker) {
            const sb = deps.snapshotBroker;
            const cwd = path.join(rootDir(s.root), s.cwd ?? "");
            const refDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-snapreq-"));
            try {
              const cargoArgv = s.tool === "rustright" ? finalArgs.slice(1) : finalArgs;
              // The ownership must record the submit, under this gate's handle, before anything is launched: a missing hook or a
              // mark that did not land refuses the request (the reclaim proof reads broker.submitted).
              if (typeof sb.markBrokerSubmit !== "function") throw new Error("no build-ownership submit hook: the broker submit cannot be recorded");
              const marked = sb.markBrokerSubmit();
              if (marked?.broker?.submitted !== true || marked.handle !== sb.handle) throw new Error(`the build ownership did not record the broker submit under ${sb.handle}`);
              const made = writeSnapshotRequest({ controlRoot: sb.controlRoot, commit: sb.commit, snapshot: sb.snapshot, declaredInputs: sb.declaredInputs ?? [], handle: sb.handle, primaryCwd: fs.realpathSync.native(cwd), argv: cargoArgv, requestIdFile: path.join(refDir, "request.json") });
              carrier = { made, refDir, cargoArgv };
            } catch (e) { fs.rmSync(refDir, { recursive: true, force: true }); clearTimeout(clock); advisory?.cleanup?.(); return { ...row, status: "fail", code: "SNAPSHOT_REQUEST", detail: `the snapshot request for ${s.id} could not be written: ${e.message}` }; }
          }
          // From here to the end of its verification the carrier and the request reference are this request's: every exit (cancel,
          // timeout, a thrown spawn, a refused receipt) removes them.
          // A wrapped snapshot compile with several Cargo workspaces (options.cargoSentinel, item 7): a scan whose own env binds no
          // CARGO_TARGET_DIR gets an empty directory of its own as one. Every cargo reached through the wrapper is bound elsewhere; only
          // a cargo that bypassed it (an absolute path, `rustup run`, a reset PATH) can write here, and the scan then fails below.
          let sentinel = null;
          if (options.cargoSentinel && !Object.hasOwn(s.env ?? {}, "CARGO_TARGET_DIR")) {
            sentinel = path.join(options.cargoSentinel, sha256(s.id).slice(0, 16));
            fs.rmSync(sentinel, { recursive: true, force: true });
            fs.mkdirSync(sentinel, { recursive: true });
          }
          const sentinelEnv = sentinel ? { CARGO_TARGET_DIR: sentinel, CARGO_BUILD_TARGET_DIR: sentinel } : {};
          try {
          try { r = await deps.spawn(p.command, finalArgs, { cwd: path.join(rootDir(s.root), s.cwd ?? ""), env: { ...deps.env, ...sentinelEnv, ...s.env, ...(extraEnv ?? {}), ...guardEnv, ...(carrier ? carrier.made.env : {}) }, signal: scanSignal, timeout: limitMs }); } finally { clearTimeout(clock); advisory?.cleanup?.(); }
          if (signal?.aborted && (!r || r.error || r.status !== 0)) return { ...row, status: "not-run", code: "CANCELLED", detail: "cancelled while running" };
          if ((timer.signal.aborted || r?.error?.code === "ETIMEDOUT") && (!r || r.error || r.status !== 0)) return { ...row, status: "fail", code: "TIMEOUT", detail: `${s.id} ran longer than ${Math.round(limitMs / 60000)} min and was killed with its process tree; a scan that does not finish fails` };
          if (!r || r.error) { status = "fail"; code = "SPAWN_ERROR"; detail = r?.error?.message ?? "no result"; }
          else if (r.status !== 0) { status = "fail"; code = "EXIT"; detail = tail(r) || `exit ${r.status}`; }
          // cargo-deny reports some failures only as an [ERROR] line and still exits 0 (under the broker, its nested offline
          // `cargo fetch` of crate sources is refused: "failed to fetch crates"). A deny row passes only with no [ERROR] line in
          // its complete output; output cut to the kept tail cannot prove that, so it fails too (RR, reviewer 2026-10-28).
          if (sentinel) {
            let written = [];
            try { written = fs.readdirSync(sentinel); } catch (e) { written = [`(unreadable: ${e.message})`]; }
            if (written.length) { status = "fail"; code = "RK_CARGO_UNBOUND"; detail = `${s.id}: a cargo that bypassed the gate's wrapper wrote ${written.slice(0, 5).join(", ")} into the unbound sentinel ${sentinel}; with several Cargo workspaces no output directory can be inferred for it, so the scan proves nothing about where its outputs went`; }
          }
          if (s.advisoryDb === "deny" && code !== "RK_CARGO_UNBOUND") {
            const denyError = `${r.stdout ?? ""}\n${r.stderr ?? ""}`.split(/\r?\n/).find((line) => /\[ERROR\]/.test(line));
            if (r.truncated) { status = "fail"; code = "DENY_ERROR"; detail = "cargo-deny output exceeded the kept tail, so the absence of an [ERROR] line cannot be shown"; }
            else if (denyError) { status = "fail"; code = "DENY_ERROR"; detail = `cargo-deny exited 0 but reported an error: ${denyError.trim().slice(0, 300)}`; }
          }
          // The broker's signed account of that request: the receipt verifies against this host's pins and its snapshotEvidence names
          // this commit, this snapshot unchanged from START to exit, this ownership and the output directory. Anything less fails the row.
          if (carrier) {
            try {
              const ref = readRequestRef(path.join(carrier.refDir, "request.json"));
              const receipt = await deps.snapshotBroker.receiptOf(ref.requestId);
              const { evidence } = verifySnapshotReceipt(receipt, { pins: deps.snapshotBroker.pins, jobId: ref.requestId, carrier: carrier.made.carrier, argv: carrier.cargoArgv, workspace: ref.workspace, observedEnv: observedEnvOf({ ...deps.env, ...s.env, ...(extraEnv ?? {}) }, deps.platform ?? process.platform) });
              row.snapshotEvidence = { requestId: ref.requestId, outputDir: evidence.outputDir, treeAtStart: evidence.snapshot.treeAtStart, treeAtExit: evidence.snapshot.treeAtExit };
            } catch (e) {
              status = "fail"; code = "SNAPSHOT_EVIDENCE"; detail = `${s.id}: the broker's snapshot evidence did not verify (${e.code ?? "error"}: ${e.message})`;
            }
          }
          } finally {
            if (carrier) { fs.rmSync(carrier.made.file, { force: true }); fs.rmSync(carrier.refDir, { recursive: true, force: true }); }
            if (tscProject) fs.rmSync(tscProject.dir, { recursive: true, force: true });
          }
          if (status === "fail" && options.scanLogDir && r) {
            try { fs.mkdirSync(options.scanLogDir, { recursive: true }); const file = path.join(options.scanLogDir, `${s.id.replace(/[^A-Za-z0-9._-]+/g, "_")}.log`); fs.writeFileSync(file, `$ ${s.command ?? s.id}\nexit ${r.status}\n--- stdout\n${r.stdout ?? ""}\n--- stderr\n${r.stderr ?? ""}\n`); row.log = file; detail = `${detail} (full output: ${file})`; } catch { /* the tail in the row remains */ }
          }
          if (s.kind === "test" && r && !r.error && r.status === 0) {
            // What the suite executed and skipped is evidence, not decoration: a later stage reuses this row only with these counts.
            const counts = parseTestSummary(`${r.stdout ?? ""}\n${r.stderr ?? ""}`);
            if (counts) row.results = counts;
            else if (status === "pass") { status = "fail"; code = "NO_TEST_SUMMARY"; detail = "the test scan passed but printed no recognisable summary (node:test or vitest); its coverage cannot be recorded"; }
            else row.results = null;
            // One rule for fresh and cached rows (testResultsProblem): tests ran, none failed, at least one passed.
            const problem = status === "pass" && row.results ? testResultsProblem(row.results) : null;
            if (problem) { status = "fail"; code = problem.code; detail = problem.detail; }
          }
          if (status === "pass" && cacheKey && !signal?.aborted) cache.write(cacheKey, { id: s.id, class: s.class, command: s.command, tool: s.tool ?? null, toolVersion: row.toolVersion, status: "pass", ...(row.results ? { results: row.results } : {}) });
        }
      }
    } catch (e) { status = "fail"; code = "SCAN_ERROR"; detail = e.message; }
    if (status === "fail" && s.class === SCAN_CLASS.A) classAFailed = true;
    return { ...row, status, ...(code ? { code } : {}), ...(detail ? { detail } : {}) };
  }

  const cancelled = signal?.aborted === true;
  return {
    schema: SCAN_RESULT_SCHEMA, ok: !cancelled && results.every((r) => r.status === "pass"), complete: !cancelled && results.every((r) => r.status === "pass"), classes, checker, ...(cancelled ? { cancelled: true } : {}),
    inputDigest: plan.inputDigest ?? null,
    deferred: results.filter((r) => r.status === "deferred").map((r) => r.id),
    scans: results, toolVersions, cacheHits: [...cacheHits].sort(), notes: plan.notes ?? [],
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
