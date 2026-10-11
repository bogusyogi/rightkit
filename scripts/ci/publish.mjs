#!/usr/bin/env node
// RightKit publication helper, driven by .github/workflows/publish.yml. RightKit is a public
// repository: this runs in GitHub Actions only, never on a developer host.
//
//   publish.mjs plan     validate the request, skip versions already published, order the rest
//   publish.mjs pack     npm: `pnpm pack` every planned package, in order, and record hashes
//   publish.mjs publish  npm publish / cargo publish the planned units (real or dry run)
//   publish.mjs ledger <receipt.json...>   record receipts in packages/release/rightkit-versions.json
//
// Inputs arrive only through the environment (PUBLISH_REGISTRY, PUBLISH_UNITS, PUBLISH_DRY_RUN),
// never through the command line and never through shell interpolation. Every unit name is
// matched against a strict pattern and against the workspace before any process starts.
// Secrets (NODE_AUTH_TOKEN, CARGO_REGISTRY_TOKEN) are read only by `publish`, and `publish`
// refuses to run a dry run while one of them is present.
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

export const NPM_NAME = /^(?:@[a-z0-9][a-z0-9._-]{0,63}\/)?[a-z0-9][a-z0-9._-]{0,63}$/;
export const CRATE_NAME = /^[a-z][a-z0-9_-]{0,63}$/;
export const PLAIN_VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
export const MAX_UNITS = 60;

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
  const nums = [];
  for (const part of m.slice(2)) { if (part === undefined || /^[xX*]$/.test(part)) break; nums.push(Number(part)); }
  if (nums.length === 0) return () => true;
  const n = nums.length;
  const lo = [nums[0], nums[1] ?? 0, nums[2] ?? 0];
  const bump = (index) => { const hi = [...lo]; hi[index] += 1; for (let i = index + 1; i < 3; i += 1) hi[i] = 0; return hi; };
  const range = (hi) => (v) => cmp(v, lo) >= 0 && cmp(v, hi) < 0;
  let op = m[1];
  if (!op) op = bare === "caret" ? "^" : n === 3 ? "=" : "x";
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
      for (const [name, spec] of Object.entries(manifest[kind] ?? {})) deps.push({ name, spec: String(spec), kind, dev: false });
    }
    units.set(manifest.name, { name: manifest.name, version: manifest.version ?? "", dir: `packages/${entry.name}`, private: manifest.private === true, deps });
  }
  return units;
}

/**
 * Minimal Cargo.toml reader for the facts publication needs: package name, version, `publish`, and
 * dependency requirements. Single-line dependency entries only; a multi-line inline table for a
 * workspace crate is refused rather than guessed at.
 */
export function parseCargoManifest(text, label = "Cargo.toml") {
  const sections = [];
  let current = null;
  for (const line of String(text).replace(/\r\n/g, "\n").split("\n")) {
    const header = /^\[([^\[\]]+)\]\s*(?:#.*)?$/.exec(line);
    if (header) { current = { name: header[1].trim().replace(/\s+/g, ""), lines: [] }; sections.push(current); continue; }
    if (/^\[\[/.test(line)) { current = { name: "[[array]]", lines: [] }; sections.push(current); continue; }
    current?.lines.push(line);
  }
  const pkg = sections.find((s) => s.name === "package");
  if (!pkg) return null;
  const field = (key) => {
    const hit = pkg.lines.map((l) => new RegExp(`^${key}\\s*=\\s*(.+?)\\s*(?:#.*)?$`).exec(l)).find(Boolean);
    return hit?.[1];
  };
  const str = (raw) => /^"([^"]*)"$/.exec(raw ?? "")?.[1];
  const name = str(field("name"));
  const version = str(field("version"));
  if (!name) throw new Error(`${label}: package name is not a literal string`);
  const publishRaw = field("publish");
  const publishable = !(publishRaw === "false" || publishRaw === "[]");
  const deps = [];
  const add = (key, body, dev) => {
    const real = /\bpackage\s*=\s*"([^"]+)"/.exec(body)?.[1] ?? key;
    const spec = /^\s*"([^"]*)"\s*$/.exec(body)?.[1] ?? /\bversion\s*=\s*"([^"]*)"/.exec(body)?.[1] ?? null;
    deps.push({ name: real, spec, dev, path: /\bpath\s*=/.test(body), optional: /\boptional\s*=\s*true/.test(body) });
  };
  for (const section of sections) {
    const m = /^(?:target\..+\.)?(dependencies|dev-dependencies|build-dependencies)(?:\.([A-Za-z0-9_-]+))?$/.exec(section.name);
    if (!m) continue;
    const dev = m[1] === "dev-dependencies";
    if (m[2]) { add(m[2], section.lines.join(" "), dev); continue; }
    for (const line of section.lines) {
      const entry = /^([A-Za-z0-9_-]+)\s*=\s*(.+?)\s*$/.exec(line);
      if (!entry) continue; // blank, comment, or `name.workspace = true` (never a rightkit crate)
      const body = entry[2].replace(/\s+#.*$/, "");
      if (body.startsWith("{") && !body.includes("}")) {
        if (entry[1].startsWith("rightkit-")) throw new Error(`${label}: multi-line dependency table for ${entry[1]} is not supported; put it on one line`);
        continue;
      }
      add(entry[1], body.replace(/^\{|\}.*$/g, ""), dev);
    }
  }
  return { name, version: version ?? "", publishable, deps };
}

export function readCrateWorkspace(root = ROOT) {
  const units = new Map();
  for (const entry of fs.readdirSync(path.join(root, "crates"), { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    let text;
    try { text = fs.readFileSync(path.join(root, "crates", entry.name, "Cargo.toml"), "utf8"); } catch { continue; }
    const manifest = parseCargoManifest(text, `crates/${entry.name}/Cargo.toml`);
    if (manifest) units.set(manifest.name, { name: manifest.name, version: manifest.version, dir: `crates/${entry.name}`, private: !manifest.publishable, deps: manifest.deps });
  }
  return units;
}

// ---- registries (anonymous HTTPS reads, no token) ---------------------------------------------

const USER_AGENT = "rightkit-publish (https://github.com/Orthic-Labs/rightkit)";

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

/**
 * Validate a publish request against the workspace and the registry. Returns
 * { registry, dryRun, units: [{ name, version, dir }] (publish order), skipped: [{ name, version }] }
 * or throws one Error listing every refusal.
 */
export async function planPublication({ registry, unitsText, dryRun, root = ROOT, fetchImpl = fetch, workspace }) {
  if (registry !== "npm" && registry !== "crates") throw new Error(`registry must be npm or crates, got ${JSON.stringify(registry)}`);
  const names = parseUnitNames(unitsText, registry);
  const style = registry === "npm" ? "npm" : "cargo";
  const members = workspace ?? (registry === "npm" ? readNpmWorkspace(root) : readCrateWorkspace(root));
  const lookup = registry === "npm" ? npmRegistryVersions : crateRegistryVersions;
  const knownRed = readJson(path.join(root, "ci", "known-red.json"), {});
  const gateConfig = readJson(path.join(root, "ci", "gate.config.json"), {});
  const redKeys = new Set(Object.values(knownRed).flatMap((host) => Object.keys(host ?? {})));
  const errors = [];
  const where = registry === "npm" ? "npm" : "crates.io";

  const requested = [];
  for (const name of names) {
    const unit = members.get(name);
    if (!unit) { errors.push(`${name}: not a ${registry === "npm" ? "package under packages/" : "crate under crates/"} of this workspace`); continue; }
    if (unit.private) { errors.push(`${name}: ${registry === "npm" ? "private package" : "publish = false"}; it is never published`); continue; }
    if (!PLAIN_VERSION.test(unit.version)) { errors.push(`${name}: version ${JSON.stringify(unit.version)} is not a plain x.y.z release version`); continue; }
    const red = [name, `rust:${name}`, unit.dir, `npm:${unit.dir}`].find((key) => redKeys.has(key));
    if (red) { errors.push(`${name}: listed in ci/known-red.json (${red}); a known-red unit is not published`); continue; }
    if (registry === "crates") {
      const os = gateConfig.rust?.[name]?.os;
      if (os && !os.includes("macos")) errors.push(`${name}: ci/gate.config.json restricts it to ${os.join(", ")}; crates publish from the macOS runner`);
    }
    requested.push(unit);
  }

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
      if (!target || dep.name === unit.name) continue;
      if (dep.dev && !dep.spec) continue; // path-only dev-dependency: cargo strips it on publish
      if (registry === "npm" && dep.spec.startsWith("workspace:") === false && /^(file|link|git|github|https?):/.test(dep.spec)) {
        errors.push(`${unit.name}: dependency ${dep.name} uses a non-registry specifier ${dep.spec}`); continue;
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
  };
}

// ---- command line -----------------------------------------------------------------------------

const env = (key) => process.env[key] ?? "";
const log = (line) => process.stdout.write(`${line}\n`);

function setOutputs(values) {
  const lines = Object.entries(values).map(([key, value]) => {
    const text = String(value);
    if (!/^[A-Za-z0-9@/._ :,\-[\]{}"]*$/.test(text) || /[\r\n]/.test(text)) throw new Error(`unsafe job output for ${key}`);
    return `${key}=${text}`;
  });
  if (process.env.GITHUB_OUTPUT) fs.appendFileSync(process.env.GITHUB_OUTPUT, `${lines.join("\n")}\n`);
  else log(lines.join("\n"));
}

function dryRunInput() {
  const raw = env("PUBLISH_DRY_RUN");
  if (raw !== "true" && raw !== "false") throw new Error(`PUBLISH_DRY_RUN must be true or false, got ${JSON.stringify(raw)}`);
  return raw === "true";
}

async function commandPlan() {
  const plan = await planPublication({ registry: env("PUBLISH_REGISTRY"), unitsText: env("PUBLISH_UNITS"), dryRun: dryRunInput() });
  const rows = [
    `## Publish plan (${plan.registry}, ${plan.dryRun ? "dry run" : "real publish"})`, "",
    "| Order | Unit | Version |", "|---|---|---|",
    ...plan.units.map((u, i) => `| ${i + 1} | ${u.name} | ${u.version} |`),
    "", plan.skipped.length ? `Already on the registry, skipped: ${plan.skipped.map((u) => `${u.name}@${u.version}`).join(", ")}` : "Nothing skipped.",
  ].join("\n");
  if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${rows}\n`);
  log(rows);
  setOutputs({
    registry: plan.registry,
    dry_run: String(plan.dryRun),
    count: plan.units.length,
    units: plan.units.map((u) => u.name).join(" "),
    skipped: plan.skipped.map((u) => u.name).join(" "),
  });
}

const sha = (algorithm, bytes) => crypto.createHash(algorithm).update(bytes).digest("hex");
const git = (...args) => spawnSync("git", args, { cwd: ROOT, encoding: "utf8" });

/** Re-derive the ordered, validated unit list inside the job that acts on it: the plan output is not trusted blindly. */
function plannedUnits() {
  const registry = env("PUBLISH_REGISTRY");
  const names = parseUnitNames(env("PUBLISH_UNITS"), registry);
  const members = registry === "npm" ? readNpmWorkspace() : readCrateWorkspace();
  return names.map((name) => {
    const unit = members.get(name);
    if (!unit || unit.private) throw new Error(`${name}: not a publishable ${registry} unit`);
    return unit;
  });
}

function requireActionsOnMain() {
  if (env("GITHUB_ACTIONS") !== "true") throw new Error("publication runs in GitHub Actions only");
  if (env("GITHUB_REF") !== "refs/heads/main") throw new Error(`publication runs from refs/heads/main only, not ${env("GITHUB_REF")}`);
  const head = git("rev-parse", "HEAD").stdout.trim();
  if (!/^[0-9a-f]{40}$/.test(head) || head !== env("GITHUB_SHA")) throw new Error(`checkout ${head} is not the dispatched commit ${env("GITHUB_SHA")}`);
  return head;
}

const packDir = () => path.join(env("RUNNER_TEMP") || os.tmpdir(), "rk-pack");
const receiptDir = () => path.join(env("RUNNER_TEMP") || os.tmpdir(), "publication-receipt");
const tarballName = (name, version) => `${name.replace(/^@/, "").replace("/", "-")}-${version}.tgz`;

function commandPack() {
  if (env("PUBLISH_REGISTRY") !== "npm") throw new Error("pack is for the npm registry");
  requireActionsOnMain();
  const units = plannedUnits();
  fs.rmSync(packDir(), { recursive: true, force: true });
  fs.mkdirSync(packDir(), { recursive: true });
  const packed = [];
  for (const unit of units) {
    const result = spawnSync("pnpm", ["--dir", unit.dir, "pack", "--pack-destination", packDir()], { cwd: ROOT, stdio: "inherit" });
    if (result.status !== 0) throw new Error(`pnpm pack failed for ${unit.name}`);
    const file = path.join(packDir(), tarballName(unit.name, unit.version));
    if (!fs.existsSync(file)) throw new Error(`pnpm pack did not produce ${path.basename(file)}`);
    const manifest = spawnSync("tar", ["-xOzf", file, "package/package.json"], { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
    if (manifest.status !== 0) throw new Error(`cannot read package.json from ${path.basename(file)}`);
    const inner = JSON.parse(manifest.stdout);
    if (inner.name !== unit.name || inner.version !== unit.version) throw new Error(`${path.basename(file)} holds ${inner.name}@${inner.version}, expected ${unit.name}@${unit.version}`);
    if (/"workspace:/.test(manifest.stdout)) throw new Error(`${unit.name}: packed package.json still holds a workspace: specifier`);
    const bytes = fs.readFileSync(file);
    packed.push({ name: unit.name, version: unit.version, file: path.basename(file), sha256: sha("sha256", bytes), sha1: sha("sha1", bytes) });
    log(`packed ${unit.name}@${unit.version} sha256 ${packed.at(-1).sha256}`);
  }
  fs.writeFileSync(path.join(packDir(), "pack.json"), JSON.stringify(packed, null, 2));
}

class Receipt {
  constructor(registry, commit, dryRun) {
    this.file = path.join(receiptDir(), "publication-receipt.json");
    fs.mkdirSync(receiptDir(), { recursive: true });
    this.doc = {
      schema: 1, registry, dryRun, commit,
      repository: env("GITHUB_REPOSITORY"), runId: env("GITHUB_RUN_ID"), runAttempt: env("GITHUB_RUN_ATTEMPT"),
      receipts: [],
    };
    this.flush();
  }
  record(entry) {
    const existing = this.doc.receipts.findIndex((r) => r.unit === entry.unit);
    const full = { ...entry, registry: this.doc.registry, commit: this.doc.commit, dryRun: this.doc.dryRun, timestamp: new Date().toISOString() };
    if (existing >= 0) this.doc.receipts[existing] = full; else this.doc.receipts.push(full);
    this.flush();
  }
  flush() { fs.writeFileSync(this.file, `${JSON.stringify(this.doc, null, 2)}\n`); }
}

/** Fail closed if the secret for this registry is present in a dry run, or absent in a real publish. */
function requireTokenState(variable, dryRun) {
  const present = Boolean(process.env[variable]);
  if (dryRun && present) throw new Error(`${variable} is present in a dry run; the dry-run step must not receive secrets`);
  if (!dryRun && !present) throw new Error(`${variable} is not set for a real publish`);
}

async function commandPublish() {
  const registry = env("PUBLISH_REGISTRY");
  const dryRun = dryRunInput();
  const commit = requireActionsOnMain();
  const units = plannedUnits();
  const receipt = new Receipt(registry, commit, dryRun);
  if (registry === "npm") await publishNpm(units, dryRun, receipt);
  else if (registry === "crates") await publishCrates(units, dryRun, receipt);
  else throw new Error(`registry must be npm or crates, got ${JSON.stringify(registry)}`);
}

async function publishNpm(units, dryRun, receipt) {
  requireTokenState("NODE_AUTH_TOKEN", dryRun);
  const packed = JSON.parse(fs.readFileSync(path.join(packDir(), "pack.json"), "utf8"));
  const tmp = fs.mkdtempSync(path.join(env("RUNNER_TEMP") || os.tmpdir(), "rk-npmrc-"));
  const userconfig = path.join(tmp, "npmrc");
  // npm substitutes ${NODE_AUTH_TOKEN} itself; the file holds no secret. A dry run has no token to
  // substitute, so it gets no auth line at all.
  fs.writeFileSync(userconfig, dryRun ? "" : "//registry.npmjs.org/:_authToken=${NODE_AUTH_TOKEN}\n", { mode: 0o600 });
  const redact = (text) => (process.env.NODE_AUTH_TOKEN ? String(text ?? "").split(process.env.NODE_AUTH_TOKEN).join("<redacted>") : String(text ?? ""));
  try {
    for (const unit of units) {
      const entry = packed.find((p) => p.name === unit.name && p.version === unit.version);
      if (!entry) throw new Error(`${unit.name}@${unit.version} was not packed`);
      const file = path.join(packDir(), entry.file);
      const bytes = fs.readFileSync(file);
      if (sha("sha256", bytes) !== entry.sha256) throw new Error(`${entry.file} changed after packing`);
      const args = ["publish", file, "--access", "public", "--registry", "https://registry.npmjs.org/", "--userconfig", userconfig];
      if (dryRun) args.push("--dry-run"); else args.push("--provenance");
      const result = spawnSync("npm", args, { encoding: "utf8", env: process.env });
      const out = redact(`${result.stdout ?? ""}${result.stderr ?? ""}`).trim();
      if (out) log(out);
      if (result.error || result.status !== 0) throw new Error(`npm publish ${dryRun ? "--dry-run " : ""}failed for ${unit.name}@${unit.version}`);
      const base = { unit: unit.name, version: unit.version, artifact: entry.file, sha256: entry.sha256, sha1: entry.sha1 };
      if (dryRun) { receipt.record({ ...base, published: false, registryVerified: false }); continue; }
      receipt.record({ ...base, published: true, registryVerified: false });
      // Reused: packages/release/publication-gate.mjs. Polls `npm view <name>@<version> dist.shasum` until it
      // equals the packed tarball's sha1; a different value throws.
      const { verifyNpmRegistryBytes } = await import(pathToFileURL(path.join(ROOT, "packages", "release", "publication-gate.mjs")).href);
      verifyNpmRegistryBytes({ name: unit.name, version: unit.version, sha1: entry.sha1 });
      receipt.record({ ...base, published: true, registryVerified: true });
    }
  } finally { fs.rmSync(tmp, { recursive: true, force: true }); }
}

function crateFile(name, version) {
  return path.join(env("CARGO_TARGET_DIR") || path.join(ROOT, "target"), "package", `${name}-${version}.crate`);
}

async function verifyCrateOnIndex(unit, cksum) {
  const deadline = Date.now() + 10 * 60 * 1000;
  for (;;) {
    const hit = (await crateRegistryVersions(unit.name)).get(unit.version);
    if (hit) {
      if (hit.cksum !== cksum) throw new Error(`crates.io checksum ${hit.cksum} != published .crate ${cksum} for ${unit.name}@${unit.version}; inspect the registry`);
      return;
    }
    if (Date.now() > deadline) throw new Error(`${unit.name}@${unit.version} is not in the crates.io index 10 minutes after publish`);
    await new Promise((resolve) => setTimeout(resolve, 15_000));
  }
}

async function publishCrates(units, dryRun, receipt) {
  requireTokenState("CARGO_REGISTRY_TOKEN", dryRun);
  const cargo = (args) => {
    const result = spawnSync("cargo", args, { cwd: ROOT, stdio: "inherit", env: process.env });
    if (result.error || result.status !== 0) throw new Error(`cargo ${args.slice(0, 2).join(" ")} failed`);
  };
  if (dryRun) {
    // One invocation so cargo (>= 1.90, multi-package publish) resolves a crate's unpublished sibling from the
    // packages it is packaging together instead of from crates.io.
    cargo(["publish", "--dry-run", "--locked", ...units.flatMap((u) => ["-p", u.name])]);
    for (const unit of units) {
      const file = crateFile(unit.name, unit.version);
      receipt.record({ unit: unit.name, version: unit.version, artifact: path.basename(file), sha256: fs.existsSync(file) ? sha("sha256", fs.readFileSync(file)) : null, published: false, registryVerified: false });
    }
    return;
  }
  for (const unit of units) {
    // One crate per invocation, dependencies first. Since cargo 1.66 `cargo publish` waits after the upload until
    // the crate is visible in the registry index, so the next crate (which depends on this one) can resolve it.
    cargo(["publish", "-p", unit.name, "--locked"]);
    const file = crateFile(unit.name, unit.version);
    const base = { unit: unit.name, version: unit.version, artifact: path.basename(file), sha256: null };
    receipt.record({ ...base, published: true, registryVerified: false });
    if (!fs.existsSync(file)) throw new Error(`${path.basename(file)} not found after publishing ${unit.name}; cannot compare it with the index`);
    const cksum = sha("sha256", fs.readFileSync(file));
    receipt.record({ ...base, sha256: cksum, published: true, registryVerified: false });
    await verifyCrateOnIndex(unit, cksum);
    receipt.record({ ...base, sha256: cksum, published: true, registryVerified: true });
  }
}

/** Record verified, real publications in the version ledger (the registry feed `rightkit-versions.json`). */
async function commandLedger(files) {
  if (files.length === 0) throw new Error("usage: publish.mjs ledger <publication-receipt.json...>");
  const { recordPublished } = await import(pathToFileURL(path.join(ROOT, "packages", "release", "rightkit-sync.mjs")).href);
  for (const file of files) {
    const doc = JSON.parse(fs.readFileSync(file, "utf8"));
    if (doc.schema !== 1) throw new Error(`${file}: unknown receipt schema`);
    if (doc.dryRun) throw new Error(`${file}: a dry-run receipt is not a publication`);
    for (const r of doc.receipts) {
      if (!r.published || !r.registryVerified) { log(`skipped ${r.unit}@${r.version}: not published and byte-verified`); continue; }
      recordPublished({ registry: doc.registry === "crates" ? "cargo" : "npm", name: r.unit, version: r.version });
    }
  }
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const [command, ...rest] = process.argv.slice(2);
  const run = { plan: commandPlan, pack: commandPack, publish: commandPublish, ledger: () => commandLedger(rest) }[command];
  if (!run) { process.stderr.write("usage: publish.mjs plan|pack|publish|ledger\n"); process.exit(2); }
  try { await run(); } catch (error) { process.stderr.write(`publish: ${error.message}\n`); process.exit(1); }
}
