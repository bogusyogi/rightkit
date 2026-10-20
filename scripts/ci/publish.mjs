#!/usr/bin/env node
// RightKit publication helper, driven by .github/workflows/publish.yml. RightKit is a public
// repository: this runs in GitHub Actions only, never on a developer host.
//
//   publish.mjs plan     validate the request, skip versions already published, order the rest
//   publish.mjs build    unprivileged, npm: build only the planned units and their workspace dependency closure
//   publish.mjs pack     unprivileged: build the exact artifacts once (npm tarballs, .crate files)
//                        and seal them with a manifest of name, version, size and hashes
//   publish.mjs verify   unprivileged, once per host: re-hash the sealed bytes, check those bytes,
//                        require the gate's per-unit pass evidence, write this host's receipt
//   publish.mjs ledger <receipt.json...>   record receipts in packages/release/rightkit-versions.json
//
// Uploading is NOT here: scripts/ci/upload.mjs is the only code that sees a registry token. This file
// holds no secret, refuses to run with one in its environment, and never publishes.
//
// Inputs arrive only through the environment (PUBLISH_REGISTRY, PUBLISH_UNITS, PUBLISH_DRY_RUN,
// PUBLISH_ALLOW_HOST_EXCLUSION, PUBLISH_ALLOW_NO_TEST, PUBLISH_ALLOW_IGNORED, PUBLISH_ALLOW_MISSING_ASSETS), never through the command line and never
// through shell interpolation. Every unit name is matched against a strict pattern and against the
// workspace before any process starts.
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { consumeCrates, consumeNpm } from "./consume.mjs";
import { stageSourceVersions } from "../../packages/release/rightkit-sync.mjs";
import { pnpmBuildClosureArgs } from "./select.mjs";
import { archiveScanProblems, checkNpmTarball, childEnv, integrity, LINUX_HOST, linuxRequiredFor, loadSealed, PASS_STATUSES, sha, TOKEN_VARS } from "./upload.mjs";
import { registrySpecProblem } from "./registry-spec.mjs";
import { featureLabels } from "./publication-record.mjs";

// A directory for tar's -C. Git for Windows puts a GNU tar on PATH that reads a backslash as an escape, so
// "C:\\x\\y" is not found; both it and the system bsdtar accept forward slashes.
const tarDir = (dir) => (process.platform === "win32" ? dir.replaceAll("\\", "/") : dir);

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

export const NPM_NAME = /^(?:@[a-z0-9][a-z0-9._-]{0,63}\/)?[a-z0-9][a-z0-9._-]{0,63}$/;
export const CRATE_NAME = /^[a-z][a-z0-9_-]{0,63}$/;
export const PLAIN_VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
export const MAX_UNITS = 60;
export const HOSTS = ["macos", "windows", LINUX_HOST];

// ---- version requirements ---------------------------------------------------------------------

const triple = (v) => PLAIN_VERSION.exec(v)?.slice(1).map(Number) ?? null;
const cmp = (a, b) => { for (let i = 0; i < 3; i += 1) if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1; return 0; };

/**
 * One comparator such as "^0.3", "~1.2.3", ">=0.2.0", "=1.2.3", "1.2", "1.x" or "*" as a predicate over
 * [major, minor, patch]. `bare` says what a bare version means: "caret" (Cargo) or "exact" (npm).
 * Anything with a prerelease or build suffix is refused: unproven requirements fail closed.
 */
function comparator(token, bare) {
  if (token === "*" || token === "x" || token === "X" || token === "") return () => true;
  const m = /^(\^|~|>=|<=|>|<|=)?(\d+|[xX*])(?:\.(\d+|[xX*]))?(?:\.(\d+|[xX*]))?$/.exec(token);
  if (!m) throw new Error(`unsupported version requirement "${token}"`);
  const nums = []; let wild = false;
  for (const part of m.slice(2)) {
    if (part === undefined) break;
    if (/^[xX*]$/.test(part)) { wild = true; continue; }
    if (wild) throw new Error(`unsupported version requirement "${token}": a number follows a wildcard`);
    nums.push(Number(part));
  }
  if (nums.length === 0) return () => true;
  const n = nums.length;
  const lo = [nums[0], nums[1] ?? 0, nums[2] ?? 0];
  const bump = (index) => { const hi = [...lo]; hi[index] += 1; for (let i = index + 1; i < 3; i += 1) hi[i] = 0; return hi; };
  const range = (hi) => (v) => cmp(v, lo) >= 0 && cmp(v, hi) < 0;
  let op = m[1];
  // "1.2.*" and "1.x" are ranges of their own (Cargo: >=1.2.0, <1.3.0), not the caret of "1.2".
  if (!op) op = wild ? "x" : bare === "caret" ? "^" : n === 3 ? "=" : "x";
  switch (op) {
    case "=": return n === 3 ? (v) => cmp(v, lo) === 0 : range(bump(n - 1));
    case "x": return range(bump(n - 1));
    case "^": {
      if (n === 3) return range(lo[0] > 0 ? bump(0) : lo[1] > 0 ? bump(1) : bump(2));
      if (n === 2) return range(lo[0] > 0 ? bump(0) : bump(1));
      return range(bump(0));
    }
    case "~": return range(n === 1 ? bump(0) : bump(1));
    case ">=": return (v) => cmp(v, lo) >= 0;
    case ">": return n === 3 ? (v) => cmp(v, lo) > 0 : (v) => cmp(v, bump(n - 1)) >= 0;
    case "<": return (v) => cmp(v, lo) < 0;
    case "<=": return n === 3 ? (v) => cmp(v, lo) <= 0 : (v) => cmp(v, bump(n - 1)) < 0;
    default: throw new Error(`unsupported version requirement "${token}"`);
  }
}

/** Does plain version `version` satisfy `requirement`? Cargo: comma-separated, bare = caret. npm: `||` alternatives, space-separated. */
export function satisfies(version, requirement, style) {
  const v = triple(version);
  if (!v) return false; // prerelease and odd versions never satisfy a plain requirement
  const bare = style === "cargo" ? "caret" : "exact";
  const normalized = String(requirement).trim().replace(/(\^|~|>=|<=|>|<|=)\s+/g, "$1");
  const alternatives = style === "cargo" ? [normalized] : normalized.split("||");
  return alternatives.some((alt) => {
    const tokens = (style === "cargo" ? alt.split(",") : alt.split(/\s+/)).map((t) => t.trim()).filter((t) => t !== "" || alt.trim() === "");
    return tokens.every((token) => comparator(token, bare)(v));
  });
}

// ---- workspace readers ------------------------------------------------------------------------

const readJson = (file, fallback) => { try { return JSON.parse(fs.readFileSync(file, "utf8")); } catch { return fallback; } };

export function readNpmWorkspace(root = ROOT) {
  const units = new Map();
  for (const entry of fs.readdirSync(path.join(root, "packages"), { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const manifest = readJson(path.join(root, "packages", entry.name, "package.json"), null);
    if (!manifest?.name) continue;
    const deps = [];
    for (const kind of ["dependencies", "optionalDependencies", "peerDependencies"]) {
      for (const [name, spec] of Object.entries(manifest[kind] ?? {})) deps.push({ name, spec, kind, dev: false });
    }
    units.set(manifest.name, { name: manifest.name, version: manifest.version ?? "", dir: `packages/${entry.name}`, private: manifest.private === true, hasTest: Boolean(manifest.scripts?.test), deps });
  }
  return units;
}

/**
 * The publishable facts of a Cargo workspace, taken from `cargo metadata --format-version 1 --no-deps`
 * output. Cargo itself resolves quoted keys, `dependency.workspace = true` inheritance, renamed
 * dependencies, multi-line tables and `[target.*]` sections, so nothing is parsed by hand and nothing
 * is silently dropped. A dependency with a path and no version requirement reports req "*" and no
 * registry source; it becomes spec null (the registry rejects it).
 */
export function crateWorkspaceFromMetadata(meta) {
  if (!meta || !Array.isArray(meta.packages) || typeof meta.workspace_root !== "string") throw new Error("cargo metadata output has no packages/workspace_root");
  const members = new Set(meta.workspace_members ?? []);
  const root = meta.workspace_root.replace(/\\/g, "/").replace(/\/$/, "");
  const units = new Map();
  for (const pkg of meta.packages) {
    if (members.size && !members.has(pkg.id)) continue;
    const manifest = String(pkg.manifest_path).replace(/\\/g, "/");
    const dir = path.posix.dirname(manifest).startsWith(`${root}/`) ? path.posix.dirname(manifest).slice(root.length + 1) : path.posix.dirname(manifest);
    const deps = (pkg.dependencies ?? []).map((d) => {
      if (typeof d.name !== "string" || typeof d.req !== "string") throw new Error(`${pkg.name}: cargo metadata dependency without a name or requirement`);
      const pathOnly = (d.source ?? null) === null;
      return { name: d.name, rename: d.rename ?? null, spec: pathOnly && d.req === "*" ? null : d.req, dev: d.kind === "dev", path: pathOnly, optional: d.optional === true, source: d.source ?? null };
    });
    const publish = pkg.publish;
    units.set(pkg.name, { name: pkg.name, version: pkg.version ?? "", dir, private: Array.isArray(publish) && !publish.includes("crates-io"), deps, metadata: pkg });
  }
  return units;
}

export function readCargoMetadata(root = ROOT, spawn = spawnSync, args = []) {
  const result = spawn("cargo", ["metadata", "--format-version", "1", "--no-deps", ...args], { cwd: root, encoding: "utf8", maxBuffer: 512 * 1024 * 1024 });
  if (result.error || result.status !== 0) throw new Error(`cargo metadata failed: ${String(result.stderr ?? result.error?.message ?? "").slice(0, 400)}`);
  return JSON.parse(result.stdout);
}

export function readCrateWorkspace(root = ROOT, spawn = spawnSync) {
  return crateWorkspaceFromMetadata(readCargoMetadata(root, spawn));
}

// ---- registries (anonymous HTTPS reads, no token) ---------------------------------------------

const USER_AGENT = "rightkit-publish (https://github.com/bogusyogi/rightkit)";

async function getText(url, fetchImpl, headers = {}) {
  let lastError;
  for (let attempt = 1; attempt <= 4; attempt += 1) {
    try {
      const response = await fetchImpl(url, { headers: { "user-agent": USER_AGENT, ...headers }, signal: AbortSignal.timeout(30_000) });
      if (response.status === 404) return null;
      if (response.ok) return await response.text();
      lastError = new Error(`HTTP ${response.status}`);
      if (response.status < 500 && response.status !== 429) break;
    } catch (error) { lastError = error; }
    await new Promise((resolve) => setTimeout(resolve, 500 * attempt));
  }
  throw new Error(`registry read failed for ${url}: ${lastError?.message ?? "unknown"}`);
}

/** Published npm versions of `name`: Map version -> { yanked: false }. Empty map when the package does not exist. */
export async function npmRegistryVersions(name, fetchImpl = fetch) {
  const text = await getText(`https://registry.npmjs.org/${name.replace("/", "%2F")}`, fetchImpl, { accept: "application/vnd.npm.install-v1+json" });
  const out = new Map();
  if (text === null) return out;
  for (const version of Object.keys(JSON.parse(text).versions ?? {})) out.set(version, { yanked: false });
  return out;
}

export function crateIndexPath(name) {
  const n = name.toLowerCase();
  if (n.length === 1) return `1/${n}`;
  if (n.length === 2) return `2/${n}`;
  if (n.length === 3) return `3/${n[0]}/${n}`;
  return `${n.slice(0, 2)}/${n.slice(2, 4)}/${n}`;
}

/** Published crate versions via the crates.io sparse index: Map version -> { yanked, cksum }. */
export async function crateRegistryVersions(name, fetchImpl = fetch) {
  const text = await getText(`https://index.crates.io/${crateIndexPath(name)}`, fetchImpl);
  const out = new Map();
  if (text === null) return out;
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    const row = JSON.parse(line);
    out.set(row.vers, { yanked: row.yanked === true, cksum: row.cksum ?? null });
  }
  return out;
}

// ---- plan -------------------------------------------------------------------------------------

/** Split and validate the `units` input. Throws one error naming every bad token. */
export function parseUnitNames(text, registry) {
  const pattern = registry === "npm" ? NPM_NAME : CRATE_NAME;
  const tokens = String(text ?? "").split(/\s+/).filter(Boolean);
  if (tokens.length === 0) throw new Error("units is empty");
  if (tokens.length > MAX_UNITS) throw new Error(`units names ${tokens.length} units; the limit is ${MAX_UNITS}`);
  const bad = tokens.filter((t) => !pattern.test(t));
  if (bad.length) throw new Error(`not a valid ${registry} name: ${bad.map((t) => JSON.stringify(t.slice(0, 80))).join(", ")}`);
  return [...new Set(tokens)];
}

/** An optional space-separated allow list: valid names only, each one of the requested units. */
export function parseAllowList(text, registry, units, label) {
  const tokens = String(text ?? "").split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return [];
  const pattern = registry === "npm" ? NPM_NAME : CRATE_NAME;
  const bad = tokens.filter((t) => !pattern.test(t) || !units.includes(t));
  if (bad.length) throw new Error(`${label} names ${bad.map((t) => JSON.stringify(t.slice(0, 80))).join(", ")}, which are not units of this request`);
  return [...new Set(tokens)];
}

/**
 * Validate a publish request against the workspace and the registry. Returns
 * { registry, dryRun, units: [{ name, version, dir }] (publish order), skipped: [{ name, version }],
 *   allowHostExclusion, allowNoTest }
 * or throws one Error listing every refusal.
 */
export async function planPublication({ registry, unitsText, dryRun, allowHostExclusionText = "", allowNoTestText = "", allowIgnoredText = "", allowMissingAssetsText = "", root = ROOT, fetchImpl = fetch, workspace }) {
  if (registry !== "npm" && registry !== "crates") throw new Error(`registry must be npm or crates, got ${JSON.stringify(registry)}`);
  const names = parseUnitNames(unitsText, registry);
  const allowHostExclusion = parseAllowList(allowHostExclusionText, registry, names, "allow_host_exclusion");
  const allowNoTest = parseAllowList(allowNoTestText, registry, names, "allow_no_test");
  const allowIgnored = parseAllowList(allowIgnoredText, registry, names, "allow_ignored");
  const allowMissingAssets = parseAllowList(allowMissingAssetsText, registry, names, "allow_missing_assets");
  const style = registry === "npm" ? "npm" : "cargo";
  const members = workspace ?? (registry === "npm" ? readNpmWorkspace(root) : readCrateWorkspace(root));
  const lookup = registry === "npm" ? npmRegistryVersions : crateRegistryVersions;
  const knownRed = readJson(path.join(root, "ci", "known-red.json"), {});
  const gateConfig = readJson(path.join(root, "ci", "gate.config.json"), {});
  // Units whose tests need assets the publish workflow does not provision (see ci/asset-gated.json).
  const assetGated = readJson(path.join(root, "ci", "asset-gated.json"), {})[registry === "npm" ? "npm" : "crates"] ?? {};
  const redKeys = new Set(Object.values(knownRed).flatMap((host) => Object.keys(host ?? {})));
  const errors = [];
  const where = registry === "npm" ? "npm" : "crates.io";

  const needed = { exclusion: new Set(), noTest: new Set(), assets: new Set() };
  const requested = [];
  for (const name of names) {
    const unit = members.get(name);
    if (!unit) { errors.push(`${name}: not a ${registry === "npm" ? "package under packages/" : "crate of this workspace"}`); continue; }
    if (unit.private) { errors.push(`${name}: ${registry === "npm" ? "private package" : "publish = false"}; it is never published`); continue; }
    if (!PLAIN_VERSION.test(unit.version)) { errors.push(`${name}: version ${JSON.stringify(unit.version)} is not a plain x.y.z release version`); continue; }
    const red = [name, `rust:${name}`, unit.dir, `npm:${unit.dir}`].find((key) => redKeys.has(key));
    if (red) { errors.push(`${name}: listed in ci/known-red.json (${red}); a known-red unit is not published`); continue; }
    if (registry === "crates") {
      const hosts = gateConfig.rust?.[name]?.os;
      if (hosts && !hosts.includes("macos")) errors.push(`${name}: ci/gate.config.json restricts it to ${hosts.join(", ")}; crates are packed on the macOS runner`);
      else if (hosts && !hosts.includes("windows")) {
        needed.exclusion.add(name);
        if (!allowHostExclusion.includes(name)) errors.push(`${name}: ci/gate.config.json restricts it to ${hosts.join(", ")}, so Windows verification would skip it; name it in allow_host_exclusion or lift the restriction`);
      }
    } else if (!unit.hasTest) {
      needed.noTest.add(name);
      if (!allowNoTest.includes(name)) errors.push(`${name}: package.json has no test script, so nothing would verify it; name it in allow_no_test to publish it untested`);
    }
    if (Object.hasOwn(assetGated, name)) {
      needed.assets.add(name);
      if (!allowMissingAssets.includes(name)) errors.push(`${name}: its tests need assets this workflow does not provision (${assetGated[name]}); name it in allow_missing_assets to publish it with those tests skipped and recorded`);
    }
    requested.push(unit);
  }
  for (const name of allowMissingAssets) if (!needed.assets.has(name)) errors.push(`allow_missing_assets names ${name}, which ci/asset-gated.json does not list`);
  for (const name of allowHostExclusion) if (!needed.exclusion.has(name)) errors.push(`allow_host_exclusion names ${name}, which no host excludes`);
  for (const name of allowNoTest) if (!needed.noTest.has(name)) errors.push(`allow_no_test names ${name}, which has a test script or is not an npm package of this request`);

  const registryCache = new Map();
  const published = async (name) => {
    if (!registryCache.has(name)) registryCache.set(name, await lookup(name, fetchImpl));
    return registryCache.get(name);
  };

  const toPublish = []; const skipped = [];
  for (const unit of requested) {
    const versions = await published(unit.name);
    if (versions.has(unit.version)) skipped.push({ name: unit.name, version: unit.version });
    else toPublish.push(unit);
  }
  const planned = new Map(toPublish.map((u) => [u.name, u]));

  // Every workspace dependency must be satisfiable when this unit publishes: already on the
  // registry at a version meeting the requirement, or published earlier in this same request.
  const edges = new Map(toPublish.map((u) => [u.name, new Set()]));
  for (const unit of toPublish) {
    for (const dep of unit.deps) {
      const target = members.get(dep.name);
      if (registry === "npm") {
        const problem = registrySpecProblem(dep.spec, { workspaceMember: Boolean(target) });
        if (problem) { errors.push(`${unit.name}: dependency ${dep.name} ${problem}: ${JSON.stringify(dep.spec)}`); continue; }
      }
      if (!target || dep.name === unit.name) continue;
      if (dep.dev && !dep.spec) continue; // path-only dev-dependency: cargo strips it on publish
      if (registry === "crates" && typeof dep.source === "string" && dep.source.startsWith("git+")) {
        errors.push(`${unit.name}: dependency ${dep.name} is a git dependency; the registry rejects it`); continue;
      }
      if (target.private) { errors.push(`${unit.name}: depends on ${registry === "npm" ? "private package" : "publish = false crate"} ${dep.name}`); continue; }
      if (!dep.spec) { errors.push(`${unit.name}: dependency ${dep.name} has no version requirement; the registry rejects a path-only dependency`); continue; }
      let requirement = dep.spec; let exactWorkspace = false;
      if (registry === "npm" && requirement.startsWith("workspace:")) {
        requirement = requirement.slice("workspace:".length);
        if (requirement === "*" || requirement === "^" || requirement === "~") exactWorkspace = true;
      }
      try {
        const wantsPlanned = planned.has(dep.name) && (exactWorkspace || satisfies(target.version, requirement, style));
        if (wantsPlanned) { edges.get(unit.name).add(dep.name); continue; }
        if (exactWorkspace) {
          if ((await published(dep.name)).has(target.version)) continue;
          errors.push(`${unit.name}: ${dep.name}@${target.version} (workspace version) is not on ${where} and is not in this request`); continue;
        }
        const live = [...(await published(dep.name))].filter(([, meta]) => !meta.yanked).map(([version]) => version);
        if (!live.some((version) => satisfies(version, requirement, style))) {
          errors.push(`${unit.name}: ${dep.name} ${requirement} is not on ${where} and ${planned.has(dep.name) ? `this request's ${target.version} does not meet it` : "it is not in this request"}`);
        }
      } catch (error) { errors.push(`${unit.name}: dependency ${dep.name}: ${error.message}`); }
    }
  }

  // Dependencies first; ties keep the order the requester gave.
  const order = []; const state = new Map();
  const visit = (name, trail) => {
    if (state.get(name) === "done") return;
    if (state.get(name) === "open") { errors.push(`dependency cycle: ${[...trail, name].join(" -> ")}`); return; }
    state.set(name, "open");
    for (const dep of [...edges.get(name)].sort((a, b) => names.indexOf(a) - names.indexOf(b))) visit(dep, [...trail, name]);
    state.set(name, "done");
    order.push(name);
  };
  for (const name of names) if (edges.has(name)) visit(name, []);

  if (errors.length) throw new Error(`publish request refused:\n  - ${errors.join("\n  - ")}`);
  return {
    registry, dryRun,
    units: order.map((name) => ({ name, version: planned.get(name).version, dir: planned.get(name).dir })),
    skipped,
    allowHostExclusion: allowHostExclusion.filter((n) => planned.has(n)),
    allowNoTest: allowNoTest.filter((n) => planned.has(n)),
    allowIgnored: allowIgnored.filter((n) => planned.has(n)),
    allowMissingAssets: allowMissingAssets.filter((n) => planned.has(n)),
  };
}

// ---- command line -----------------------------------------------------------------------------

const log = (line) => process.stdout.write(`${line}\n`);
const hostName = () => (process.platform === "win32" ? "windows" : process.platform === "darwin" ? "macos" : "linux");

function setOutputs(values, env = process.env) {
  const lines = Object.entries(values).map(([key, value]) => {
    const text = String(value);
    if (!/^[A-Za-z0-9@/._ :,\-[\]{}"]*$/.test(text) || /[\r\n]/.test(text)) throw new Error(`unsafe job output for ${key}`);
    return `${key}=${text}`;
  });
  if (env.GITHUB_OUTPUT) fs.appendFileSync(env.GITHUB_OUTPUT, `${lines.join("\n")}\n`);
  else log(lines.join("\n"));
}

function dryRunInput(env) {
  const raw = env.PUBLISH_DRY_RUN ?? "";
  if (raw !== "true" && raw !== "false") throw new Error(`PUBLISH_DRY_RUN must be true or false, got ${JSON.stringify(raw)}`);
  return raw === "true";
}

async function commandPlan(env = process.env) {
  const plan = await planPublication({ registry: env.PUBLISH_REGISTRY ?? "", unitsText: env.PUBLISH_UNITS ?? "", dryRun: dryRunInput(env), allowHostExclusionText: env.PUBLISH_ALLOW_HOST_EXCLUSION ?? "", allowNoTestText: env.PUBLISH_ALLOW_NO_TEST ?? "", allowIgnoredText: env.PUBLISH_ALLOW_IGNORED ?? "", allowMissingAssetsText: env.PUBLISH_ALLOW_MISSING_ASSETS ?? "" });
  const names = new Set(plan.units.map((u) => u.name));
  // Units whose plan requires a Linux verify row (ci/linux-required.json). An unreadable list refuses the whole plan.
  const linux = linuxRequiredFor({ registry: plan.registry, units: plan.units.map((u) => u.name) });
  if (linux.problems.length) throw new Error(`publish request refused:\n  - ${linux.problems.join("\n  - ")}`);
  const rows = [
    `## Publish plan (${plan.registry}, ${plan.dryRun ? "dry run" : "real publish"})`, "",
    "| Order | Unit | Version |", "|---|---|---|",
    ...plan.units.map((u, i) => `| ${i + 1} | ${u.name} | ${u.version} |`),
    "", plan.skipped.length ? `Already on the registry, skipped: ${plan.skipped.map((u) => `${u.name}@${u.version}`).join(", ")}` : "Nothing skipped.",
    linux.units.length ? `Linux verification required for: ${linux.units.join(", ")}` : "No unit requires Linux verification.",
  ].join("\n");
  if (env.GITHUB_STEP_SUMMARY) fs.appendFileSync(env.GITHUB_STEP_SUMMARY, `${rows}\n`);
  log(rows);
  setOutputs({
    registry: plan.registry,
    dry_run: String(plan.dryRun),
    count: plan.units.length,
    units: plan.units.map((u) => u.name).join(" "),
    linux_count: linux.units.length,
    linux_units: linux.units.join(" "),
    skipped: plan.skipped.map((u) => u.name).join(" "),
    allow_host_exclusion: plan.allowHostExclusion.filter((n) => names.has(n)).join(" "),
    allow_no_test: plan.allowNoTest.filter((n) => names.has(n)).join(" "),
    allow_ignored: plan.allowIgnored.filter((n) => names.has(n)).join(" "),
    allow_missing_assets: plan.allowMissingAssets.filter((n) => names.has(n)).join(" "),
  }, env);
}

const packDir = (env = process.env) => env.PUBLISH_PACK_DIR || path.join(env.RUNNER_TEMP || os.tmpdir(), "rk-pack");
const tarballName = (name, version) => `${name.replace(/^@/, "").replace("/", "-")}-${version}.tgz`;

/** The pack and verify jobs hold no secret. Refuse to run if one is present. */
export function requireNoTokens(env) {
  for (const variable of TOKEN_VARS) if (env[variable]) throw new Error(`${variable} is present in an unprivileged step; pack and verify must not receive secrets`);
  if (env.ACTIONS_ID_TOKEN_REQUEST_URL || env.ACTIONS_ID_TOKEN_REQUEST_TOKEN) throw new Error("an OIDC token is present in an unprivileged step; pack and verify must not request id-token");
}

export function requireActionsOnMain(env, spawn = spawnSync) {
  if (env.GITHUB_ACTIONS !== "true") throw new Error("publication runs in GitHub Actions only");
  if (env.GITHUB_REF !== "refs/heads/main") throw new Error(`publication runs from refs/heads/main only, not ${env.GITHUB_REF}`);
  const head = String(spawn("git", ["rev-parse", "HEAD"], { cwd: ROOT, encoding: "utf8" }).stdout ?? "").trim();
  if (!/^[0-9a-f]{40}$/.test(head) || head !== env.GITHUB_SHA) throw new Error(`checkout ${head} is not the dispatched commit ${env.GITHUB_SHA}`);
  return head;
}

/** Re-derive the ordered, validated unit list inside the job that acts on it: the plan output is not trusted blindly. */
function plannedUnits(env, members) {
  const registry = env.PUBLISH_REGISTRY;
  const names = parseUnitNames(env.PUBLISH_UNITS, registry);
  return names.map((name) => {
    const unit = members.get(name);
    if (!unit || unit.private) throw new Error(`${name}: not a publishable ${registry} unit`);
    return unit;
  });
}

/**
 * The crates.io publish metadata (the JSON half of the publish request) for one packaged crate,
 * from `cargo metadata --no-deps` of the PACKAGED Cargo.toml, where Cargo has already resolved
 * workspace inheritance and removed path keys. Field names follow the `NewCrate` struct of Cargo's
 * `crates-io` crate and the registry web API "publish" endpoint.
 */
export function buildCrateUploadMetadata(pkg, readmeText = null) {
  const deps = (pkg.dependencies ?? []).map((d) => ({
    optional: d.optional === true,
    default_features: d.uses_default_features !== false,
    name: d.name,
    features: d.features ?? [],
    version_req: d.req,
    target: d.target ?? null,
    kind: d.kind ?? "normal",
    registry: d.registry ?? null,
    explicit_name_in_toml: d.rename ?? null,
  }));
  return {
    name: pkg.name,
    vers: pkg.version,
    deps,
    features: pkg.features ?? {},
    authors: pkg.authors ?? [],
    description: pkg.description ?? null,
    documentation: pkg.documentation ?? null,
    homepage: pkg.homepage ?? null,
    readme: readmeText,
    readme_file: pkg.readme ?? null,
    keywords: pkg.keywords ?? [],
    categories: pkg.categories ?? [],
    license: pkg.license ?? null,
    license_file: pkg.license_file ?? null,
    repository: pkg.repository ?? null,
    badges: {},
    links: pkg.links ?? null,
    rust_version: pkg.rust_version ?? null,
  };
}

function packNpm({ units, root, outDir, env, spawn }) {
  const entries = [];
  // The feed a tarball ships must list the version it ships with (#319): stage every unit of this publish first.
  const versionsFile = path.join(root, "packages", "release", "rightkit-versions.json");
  if (fs.existsSync(versionsFile)) stageSourceVersions(units.map((u) => ({ registry: "npm", name: u.name, version: u.version })), { file: versionsFile, root, log });
  for (const unit of units) {
    const result = spawn("pnpm", ["--dir", unit.dir, "pack", "--pack-destination", outDir], { cwd: root, stdio: "inherit", env });
    if (result.error || result.status !== 0) throw new Error(`pnpm pack failed for ${unit.name}`);
    const file = path.join(outDir, tarballName(unit.name, unit.version));
    if (!fs.existsSync(file)) throw new Error(`pnpm pack did not produce ${path.basename(file)}`);
    const checked = checkNpmTarball({ file, name: unit.name, version: unit.version, dir: unit.dir, spawn });
    if (checked.problems.length) throw new Error(`${unit.name}: packed tarball refused:\n  - ${checked.problems.join("\n  - ")}`);
    const bytes = fs.readFileSync(file);
    entries.push({ name: unit.name, version: unit.version, dir: unit.dir, file: path.basename(file), size: bytes.length, sha256: sha("sha256", bytes), sha1: sha("sha1", bytes), integrity: integrity(bytes) });
    log(`packed ${unit.name}@${unit.version} sha256 ${entries.at(-1).sha256}`);
  }
  return entries;
}

function packCrates({ units, root, outDir, env, spawn, metadata }) {
  const packed = spawn("cargo", ["package", "--locked", ...units.flatMap((u) => ["-p", u.name])], { cwd: root, stdio: "inherit", env });
  if (packed.error || packed.status !== 0) throw new Error("cargo package failed; it compiles and verifies each crate without any token");
  const scratch = fs.mkdtempSync(path.join(env.RUNNER_TEMP || os.tmpdir(), "rk-crate-"));
  const entries = [];
  try {
    for (const unit of units) {
      const crate = `${unit.name}-${unit.version}.crate`;
      const source = path.join(metadata.target_directory, "package", crate);
      if (!fs.existsSync(source)) throw new Error(`cargo package did not produce ${crate} under ${metadata.target_directory}/package`);
      const file = path.join(outDir, crate);
      fs.copyFileSync(source, file);
      const unpack = spawn("tar", ["-xzf", crate, "-C", tarDir(scratch)], { cwd: outDir, encoding: "utf8", env: childEnv(env) });
      if (unpack.error || unpack.status !== 0) throw new Error(`cannot unpack ${crate}`);
      const inner = path.join(scratch, `${unit.name}-${unit.version}`);
      const meta = readCargoMetadata(inner, spawn, ["--manifest-path", path.join(inner, "Cargo.toml")]);
      const pkg = meta.packages.find((p) => p.name === unit.name && p.version === unit.version);
      if (!pkg) throw new Error(`${crate}: packaged Cargo.toml does not describe ${unit.name}@${unit.version}`);
      const readme = pkg.readme ? fs.readFileSync(path.join(inner, pkg.readme), "utf8") : null;
      const metaFile = `${unit.name}-${unit.version}.upload.json`;
      const metaText = `${JSON.stringify(buildCrateUploadMetadata(pkg, readme))}\n`;
      fs.writeFileSync(path.join(outDir, metaFile), metaText);
      const bytes = fs.readFileSync(file);
      entries.push({ name: unit.name, version: unit.version, dir: unit.dir, file: crate, size: bytes.length, sha256: sha("sha256", bytes), metaFile, metaSha256: sha("sha256", Buffer.from(metaText)) });
      log(`packed ${crate} sha256 ${entries.at(-1).sha256}`);
    }
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
  return entries;
}

/**
 * Build the planned npm units and their workspace dependency closure (`pnpm --filter "<name>..."`), in dependency
 * order, and nothing else: an unrelated package that does not build cannot block a publish, and nothing outside
 * the closure is compiled into the sealed bytes' build tree.
 */
export function commandBuild({ env = process.env, spawn = spawnSync, root = ROOT } = {}) {
  requireNoTokens(env);
  if (env.GITHUB_ACTIONS !== "true") throw new Error("publication runs in GitHub Actions only");
  if (env.PUBLISH_REGISTRY !== "npm") throw new Error(`build applies to npm runs, not ${JSON.stringify(env.PUBLISH_REGISTRY)}`);
  const units = plannedUnits(env, readNpmWorkspace(root));
  const args = pnpmBuildClosureArgs(units.map((u) => u.name));
  log(`building ${units.map((u) => u.name).join(", ")} and their workspace dependencies`);
  const result = spawn("pnpm", args, { cwd: root, stdio: "inherit", env });
  if (result.error || result.status !== 0) throw new Error("pnpm build of the planned units and their dependencies failed");
  return args;
}

/** Build the exact artifacts once, without any secret, and seal them with manifest.json. */
export function commandPack({ env = process.env, spawn = spawnSync, root = ROOT } = {}) {
  requireNoTokens(env);
  const registry = env.PUBLISH_REGISTRY;
  const commit = requireActionsOnMain(env, spawn);
  const outDir = packDir(env);
  fs.rmSync(outDir, { recursive: true, force: true });
  fs.mkdirSync(outDir, { recursive: true });
  let entries;
  if (registry === "npm") entries = packNpm({ units: plannedUnits(env, readNpmWorkspace(root)), root, outDir, env, spawn });
  else if (registry === "crates") {
    const metadata = readCargoMetadata(root, spawn);
    entries = packCrates({ units: plannedUnits(env, crateWorkspaceFromMetadata(metadata)), root, outDir, env, spawn, metadata });
  } else throw new Error(`registry must be npm or crates, got ${JSON.stringify(registry)}`);
  const manifest = { schema: 1, registry, commit, repository: env.GITHUB_REPOSITORY ?? "", runId: env.GITHUB_RUN_ID ?? "", runAttempt: env.GITHUB_RUN_ATTEMPT ?? "", units: entries };
  fs.writeFileSync(path.join(outDir, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}

const GENERATED_IN_CRATE = new Set(["Cargo.toml", ".cargo_vcs_info.json", "Cargo.lock"]);

/**
 * The sealed .crate must be this commit's source: every packaged file equals the blob at `commit`
 * (read with `git cat-file`, so a CRLF checkout cannot disturb it), and Cargo.toml.orig equals the
 * crate's own Cargo.toml. Cargo.toml, Cargo.lock and .cargo_vcs_info.json are generated by packaging.
 */
export function crateSourceProblems({ file, unit, commit, scratchRoot, spawn = spawnSync }) {
  const problems = [];
  const scratch = fs.mkdtempSync(path.join(scratchRoot, "rk-verify-crate-"));
  try {
    const unpack = spawn("tar", ["-xzf", path.basename(file), "-C", tarDir(scratch)], { cwd: path.dirname(file), encoding: "utf8", env: childEnv(process.env) });
    if (unpack.error || unpack.status !== 0) return [`${unit.name}: cannot unpack ${path.basename(file)}`];
    const inner = path.join(scratch, `${unit.name}-${unit.version}`);
    if (!fs.existsSync(inner)) return [`${unit.name}: ${path.basename(file)} has no ${unit.name}-${unit.version}/ root`];
    const blob = (candidate) => {
      const r = spawn("git", ["cat-file", "blob", `${commit}:${candidate}`], { cwd: ROOT, encoding: "buffer", maxBuffer: 256 * 1024 * 1024 });
      return r.status === 0 ? Buffer.from(r.stdout) : null;
    };
    const walk = (dir) => fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(path.join(dir, e.name)) : [path.join(dir, e.name)]));
    for (const full of walk(inner)) {
      const rel = path.relative(inner, full).split(path.sep).join("/");
      if (GENERATED_IN_CRATE.has(rel)) continue;
      const candidates = rel === "Cargo.toml.orig" ? [`${unit.dir}/Cargo.toml`] : [`${unit.dir}/${rel}`, rel];
      const bytes = fs.readFileSync(full);
      const match = candidates.map(blob).find((b) => b && b.equals(bytes));
      if (!match) problems.push(`${unit.name}: ${rel} in the sealed crate does not match ${candidates.join(" or ")} at ${commit.slice(0, 12)}`);
    }
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
  return problems;
}

/** Evidence the gate wrote for this host: one row per requested unit. */
function gateEvidence(env, spawn, registry, commit) {
  const dir = path.join(env.RUNNER_TEMP || os.tmpdir(), "rightkit-ci");
  const readBound = (name) => {
    const file = path.join(dir, name);
    let bytes;
    try { bytes = fs.readFileSync(file); } catch { throw new Error(`no gate evidence at ${file}; the gate must run before verify`); }
    let doc;
    try { doc = JSON.parse(bytes.toString("utf8")); } catch { throw new Error(`invalid JSON in gate evidence ${file}`); }
    return { doc, hash: crypto.createHash("sha256").update(bytes).digest("hex") };
  };
  const { doc: unitEvidence, hash: unitEvidenceSha256 } = readBound("unit-evidence.json");
  const { doc: summary, hash: summarySha256 } = readBound("summary.json");
  if (unitEvidence.schema !== 2 || !Array.isArray(unitEvidence.units) || summary.host !== unitEvidence.host
    || summary.sha !== unitEvidence.sha || summary.sha !== commit || unitEvidence.sha !== commit
    || unitEvidence.registry !== registry || unitEvidence.ok !== true || !Array.isArray(summary.results)) {
    throw new Error("gate summary and unit evidence are unavailable or inconsistent with this publication run");
  }
  const tree = String(spawn("git", ["rev-parse", "HEAD^{tree}"], { cwd: ROOT, encoding: "utf8" }).stdout ?? "").trim();
  if (!/^[0-9a-f]{40}$/.test(tree)) throw new Error("cannot bind gate evidence to the current Git tree");
  return { unitEvidence, summary, sourceGate: { summarySha256, unitEvidenceSha256, dependentClosure: unitEvidence.dependentClosure ?? null, tree } };
}

function canonicalPublicationEvidence({ unit, row, summary, registry, commit, tree, env }) {
  if (!row) throw new Error(`${unit.name}: no gate unit evidence exists for publication verification`);
  if (["allowed-exclusion", "allowed-no-test"].includes(row.status)) {
    return { publicationEvidence: null, publicationEvidenceWaiver: row.status };
  }
  const id = registry === "npm" ? `npm:${unit.dir}` : `rust:${unit.name}`;
  const results = summary.results.filter((result) => result.unit === id && result.isTest === true);
  if (!results.length || results.some((result) => result.code !== 0 || result.timedOut !== false
    || !Number.isSafeInteger(result.seconds) || result.seconds < 0 || !Number.isSafeInteger(result.executed)
    || !Number.isSafeInteger(result.failed))) {
    throw new Error(`${unit.name}: gate summary lacks complete passing test command results`);
  }
  const passed = results.reduce((sum, result) => sum + result.executed, 0);
  const failed = results.reduce((sum, result) => sum + result.failed, 0);
  if (passed <= 0 || failed !== 0 || results.some((result) => result.ok !== true)) {
    throw new Error(`${unit.name}: gate summary does not prove a positive passing native test run`);
  }
  const features = featureLabels(registry, unit.name, results.map((result) => result.command));
  const operator = `${env.GITHUB_ACTOR ?? ""}/${env.GITHUB_JOB ?? ""}`;
  if (!env.GITHUB_ACTOR || !env.GITHUB_JOB) throw new Error("GITHUB_ACTOR and GITHUB_JOB are required to identify the native publication operator");
  return { publicationEvidence: {
    schema: 1, kind: registry === "npm" ? "npm" : "cargo", name: unit.name, version: unit.version, commit,
    tree, artifactSha256: unit.sha256,
    runs: [{ commit, host: { os: process.platform, arch: process.arch }, features, artifactSha256: unit.sha256,
      command: results.map((result) => result.command).join(" && "), operator, timestamp: new Date().toISOString(), passed, failed }],
  } };
}

/**
 * Verify the SEALED bytes on this host and write this host's receipt. Re-hashes every sealed file,
 * checks the npm tarballs (name, version, entry points, provenance repository) or the crate sources,
 * requires the gate's explicit per-unit evidence, and binds all of it to the manifest hash.
 */
export async function commandVerify({ env = process.env, spawn = spawnSync, host = hostName(), consume = { npm: consumeNpm, crates: consumeCrates }, scanArchive = async (options) => (await import("./archive-secret-scan.mjs")).scanArchiveForSecrets(options) } = {}) {
  requireNoTokens(env);
  const registry = env.PUBLISH_REGISTRY;
  const commit = requireActionsOnMain(env, spawn);
  const dir = packDir(env);
  const { manifest, manifestSha256, problems } = loadSealed({ packDir: dir, registry, unitsText: env.PUBLISH_UNITS, run: { runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT } });
  if (!/^\d+$/.test(env.GITHUB_RUN_ATTEMPT ?? "")) problems.push("GITHUB_RUN_ATTEMPT is not set; the receipt cannot be bound to this attempt");
  if (manifest && manifest.commit !== commit) problems.push(`sealed commit ${manifest.commit} is not the dispatched commit ${commit}`);
  const hostId = host;
  if (!HOSTS.includes(hostId)) problems.push(`verify runs on macOS, Windows or Linux, not ${hostId}`);
  const allowHostExclusion = String(env.PUBLISH_ALLOW_HOST_EXCLUSION ?? "").split(/\s+/).filter(Boolean);
  const allowNoTest = String(env.PUBLISH_ALLOW_NO_TEST ?? "").split(/\s+/).filter(Boolean);
  const gate = gateEvidence(env, spawn, registry, commit);
  const evidence = gate.unitEvidence;
  const splitList = (text) => String(text ?? "").split(/\s+/).filter(Boolean);
  const allowIgnored = splitList(env.PUBLISH_ALLOW_IGNORED); const allowMissingAssets = splitList(env.PUBLISH_ALLOW_MISSING_ASSETS);
  if (evidence.host !== hostId || evidence.registry !== registry || evidence.sha !== commit) problems.push(`gate evidence is for ${evidence.registry} on ${evidence.host} at ${evidence.sha}, not ${registry} on ${hostId} at ${commit}`);
  const rows = [];
  const scratchRoot = env.RUNNER_TEMP || os.tmpdir();
  const fail = () => { throw new Error(`verification of the sealed artifacts failed on ${hostId}:\n  - ${problems.join("\n  - ")}`); };
  // Linux verifies ONLY the units whose plan requires it, re-derived here from the reviewed ci/linux-required.json
  // (never from the plan output alone). The gate evidence must cover exactly that set, and a Linux run with nothing
  // required is refused rather than written as an empty pass. macOS and Windows verify every sealed unit.
  let scope = manifest?.units ?? [];
  if (hostId === LINUX_HOST && manifest) {
    const required = linuxRequiredFor({ registry, units: manifest.units.map((u) => u.name) });
    problems.push(...required.problems);
    scope = manifest.units.filter((u) => required.units.includes(u.name));
    if (scope.length === 0 && required.problems.length === 0) problems.push("no sealed unit requires Linux verification; this host must not run");
    const scopeNames = scope.map((u) => u.name).sort().join(" ");
    const planned = splitList(env.PUBLISH_LINUX_UNITS).sort().join(" ");
    if (planned !== scopeNames) problems.push(`the Linux units the plan named [${planned}] differ from the Linux-required sealed units [${scopeNames}]`);
    const gated = (evidence.units ?? []).map((u) => u.name).sort().join(" ");
    if (gated !== scopeNames) problems.push(`the gate ran [${gated}] on Linux, not exactly the Linux-required units [${scopeNames}]`);
  }
  if (problems.length) fail();
  // Scan every sealed unit before any consumer installs, extracts or executes shipped contents.
  const archiveScans = new Map();
  for (const unit of scope) {
    let scan;
    try { scan = await scanArchive({ file: path.join(dir, unit.file), registry: registry === "npm" ? "npm" : "cargo", name: unit.name, version: unit.version, scratchRoot }); }
    catch { problems.push(`${unit.name}: archive secret scan failed`); continue; }
    archiveScans.set(unit.name, scan);
    for (const problem of archiveScanProblems(scan, unit.sha256)) problems.push(`${unit.name}: ${problem}`);
  }
  if (problems.length) fail();
  // Consumer verification: install/extract the SEALED artifacts together and use them as a consumer would.
  let consumed = new Map();
  if (manifest && problems.length === 0) {
    const excluded = (evidence.units ?? []).filter((u) => u.status === "allowed-exclusion").map((u) => u.name);
    const common = { units: scope, packDir: dir, scratchRoot, spawn, env };
    try { consumed = registry === "npm" ? consume.npm(common) : consume.crates({ ...common, excluded, uploadMetadata: buildCrateUploadMetadata }); } catch (error) { for (const unit of scope) consumed.set(unit.name, { problems: [`${unit.name}: consumer verification crashed: ${error.message}`], report: {} }); }
  }
  for (const unit of scope) {
    const file = path.join(dir, unit.file);
    let unitProblems;
    try {
      unitProblems = registry === "npm"
        ? checkNpmTarball({ file, name: unit.name, version: unit.version, dir: unit.dir, spawn }).problems
        : crateSourceProblems({ file, unit, commit, scratchRoot: env.RUNNER_TEMP || os.tmpdir(), spawn });
    } catch (error) { unitProblems = [`${unit.name}: ${error.message}`]; }
    const consumer = consumed.get(unit.name) ?? { problems: [], report: {}, missing: true };
    const consumerOk = !consumer.missing && consumer.problems.length === 0;
    unitProblems = [...unitProblems, ...consumer.problems];
    problems.push(...unitProblems);
    const row = (evidence.units ?? []).find((u) => u.name === unit.name);
    if (!row) problems.push(`${unit.name}: the gate left no evidence for it on ${hostId}`);
    else if (!PASS_STATUSES.includes(row.status)) problems.push(`${unit.name}: gate status ${JSON.stringify(row.status)} on ${hostId} is not a pass`);
    rows.push({ name: unit.name, version: unit.version, file: unit.file, sha256: unit.sha256, archiveScan: archiveScans.get(unit.name), bytesVerified: unitProblems.length === 0, status: row?.status ?? "missing", executed: row?.executed ?? 0, ignored: row?.ignored ?? 0, skipped: row?.skipped ?? 0, consumer: { verified: consumerOk, ...consumer.report }, steps: row?.steps ?? [], ...canonicalPublicationEvidence({ unit, row, summary: gate.summary, registry, commit, tree: gate.sourceGate.tree, env }) });
  }
  if (problems.length) throw new Error(`verification of the sealed artifacts failed on ${hostId}:\n  - ${problems.join("\n  - ")}`);
  const receipt = { schema: 1, kind: "verify", host: hostId, registry, commit, manifestSha256, manifestRunId: manifest.runId, manifestRunAttempt: manifest.runAttempt, repository: env.GITHUB_REPOSITORY ?? "", runId: env.GITHUB_RUN_ID ?? "", runAttempt: env.GITHUB_RUN_ATTEMPT ?? "", allowHostExclusion, allowNoTest, allowIgnored, allowMissingAssets, sourceGate: gate.sourceGate, units: rows };
  const out = path.join(env.RUNNER_TEMP || os.tmpdir(), "verify-receipt");
  fs.mkdirSync(out, { recursive: true });
  fs.writeFileSync(path.join(out, `verify-${hostId}.json`), `${JSON.stringify(receipt, null, 2)}\n`);
  log(`verified ${rows.length} sealed ${registry} unit(s) on ${hostId}`);
  return receipt;
}

/** The ledger rows a publication receipt supports: only units published AND byte-verified on the registry. */
export function ledgerEntries(doc, file = "receipt") {
  if (doc?.schema !== 1) throw new Error(`${file}: unknown receipt schema`);
  if (doc.dryRun) throw new Error(`${file}: a dry-run receipt is not a publication`);
  if (!Array.isArray(doc.receipts)) throw new Error(`${file}: receipt has no receipts list`);
  const entries = []; const skipped = [];
  for (const r of doc.receipts) {
    if (!r.published || !r.registryVerified) { skipped.push(`${r.unit}@${r.version}`); continue; }
    entries.push({ registry: doc.registry === "crates" ? "cargo" : "npm", name: r.unit, version: r.version });
  }
  return { entries, skipped };
}

/** After recording: every expected version is in the ledger (or a later one), and the session-hook copy is byte-identical. Returns problems. */
export function verifyLedger({ entries, versionsFile, copyFile, compareVersions }) {
  const problems = [];
  let raw; let versions;
  try { raw = fs.readFileSync(versionsFile); versions = JSON.parse(raw.toString("utf8")); } catch (error) { return [`cannot read ${versionsFile} after recording: ${error.message}`]; }
  for (const { registry, name, version } of entries) {
    const have = versions.registry?.[registry]?.[name];
    let ok = false;
    try { ok = typeof have === "string" && compareVersions(have, version) >= 0; } catch { ok = false; }
    if (!ok) problems.push(`${registry} ${name}@${version} is not in the ledger after recording (registry pin ${JSON.stringify(have)})`);
  }
  try { if (!fs.readFileSync(copyFile).equals(raw)) problems.push(`${path.basename(copyFile)} differs from ${path.basename(versionsFile)} after recording`); } catch (error) { problems.push(`cannot read ${copyFile} after recording: ${error.message}`); }
  return problems;
}

/**
 * Record verified, real publications in the version ledger (the registry feed `rightkit-versions.json`).
 * Strict: a failed write or consumer-copy refresh throws, and the ledger is read back to prove every expected
 * version and the consumer copy are present before this returns.
 */
export async function commandLedger(files, { root = ROOT, sync = null } = {}) {
  if (files.length === 0) throw new Error("usage: publish.mjs ledger <publication-receipt.json...>");
  const mod = sync ?? await import(pathToFileURL(path.join(root, "packages", "release", "rightkit-sync.mjs")).href);
  const versionsFile = path.join(root, "packages", "release", "rightkit-versions.json");
  const copyFile = path.join(root, "packages", "build-control-policy", "src", "ownership-session-versions.json");
  const expected = [];
  for (const file of files) {
    const { entries, skipped } = ledgerEntries(JSON.parse(fs.readFileSync(file, "utf8")), file);
    for (const row of skipped) log(`skipped ${row}: not published and byte-verified`);
    if (entries.length === 0) throw new Error(`${file}: no unit is published and byte-verified; nothing to record`);
    for (const entry of entries) { mod.recordPublished(entry, { strict: true, file: versionsFile, root }); expected.push(entry); }
  }
  const problems = verifyLedger({ entries: expected, versionsFile, copyFile, compareVersions: mod.compareVersions });
  if (problems.length) throw new Error(`the ledger is incomplete:\n  - ${problems.join("\n  - ")}`);
  log(`ledger holds ${expected.length} recorded version(s) and its consumer copy matches`);
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const [command, ...rest] = process.argv.slice(2);
  const run = { plan: commandPlan, build: () => commandBuild(), pack: () => commandPack(), verify: () => commandVerify(), ledger: () => commandLedger(rest) }[command];
  if (!run) { process.stderr.write("usage: publish.mjs plan|build|pack|verify|ledger\n"); process.exit(2); }
  try { await run(); } catch (error) { process.stderr.write(`publish: ${error.message}\n`); process.exit(1); }
}
