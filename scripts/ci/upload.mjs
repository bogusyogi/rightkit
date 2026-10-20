#!/usr/bin/env node
// RightKit sealed-artifact upload. The ONLY code that runs in the publish workflow's `upload` job, the
// only job that holds a registry token or an OIDC token. It has no dependencies beyond node built-ins
// and compiles, installs and builds nothing: it
//   1. re-checks the sealed artifacts packed by the unprivileged `pack` job against manifest.json
//      (name, version, size, sha256) and rejects unlisted files,
//   1b. requires ci/approved-artifacts.json (read from the dispatched commit's checkout, never from an input)
//      to hold a reviewer-signed entry whose registry, name, version and sha256 equal every sealed unit; any
//      missing, mismatched or malformed approval refuses before a registry or token is touched,
//   2. re-checks that BOTH verify hosts (macos and windows) produced a receipt for exactly those bytes
//      at this commit, with explicit pass evidence for every unit,
//   3. uploads those bytes: `npm publish <tgz> --ignore-scripts --provenance`, or a PUT of the sealed
//      `.crate` to the crates.io publish API with the token on that one HTTP request only.
// A retry of failed jobs (same run id, later attempt) starts from the previous attempt's publication receipt,
// re-checks the registry for every planned unit before sending anything, records a unit whose exact version
// AND sealed hash are already there as published by an earlier attempt, and refuses a version that exists
// with different bytes.
// `run --dry-run` does steps 1 and 2 and builds the exact commands and request bodies, sends nothing,
// and refuses to start when any registry token is present. publish.mjs imports the pure helpers here.
//
//   upload.mjs run [--dry-run]     (inputs arrive through the environment, never the command line)
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const REPOSITORY = "bogusyogi/rightkit"; // moved 2026-10-17 (#342)
export const REQUIRED_HOSTS = ["macos", "windows"];
export const NPM_REGISTRY_URL = "https://registry.npmjs.org/";
export const CRATES_PUBLISH_URL = "https://crates.io/api/v1/crates/new";
export const CRATES_INDEX_URL = "https://index.crates.io/";
export const TOKEN_VARS = ["NODE_AUTH_TOKEN", "CARGO_REGISTRY_TOKEN"];
export const PASS_STATUSES = ["pass", "allowed-skips", "allowed-exclusion", "allowed-no-test"];
const USER_AGENT = `rightkit-publish (https://github.com/${REPOSITORY})`;
const SAFE_FILE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,200}$/;

export const sha = (algorithm, bytes) => crypto.createHash(algorithm).update(bytes).digest("hex");
export const integrity = (bytes) => `sha512-${crypto.createHash("sha512").update(bytes).digest("base64")}`;
const isSha = (value) => /^[0-9a-f]{40}$/.test(value ?? "");
const splitNames = (text) => String(text ?? "").split(/\s+/).filter(Boolean);

// ---- child processes --------------------------------------------------------------------------

const ENV_NAMES = new Set(["PATH", "Path", "PATHEXT", "HOME", "USERPROFILE", "TEMP", "TMP", "TMPDIR", "SystemRoot", "SYSTEMROOT", "windir", "ComSpec", "LANG", "LC_ALL", "CI", "NODE_EXTRA_CA_CERTS"]);
const ENV_PREFIXES = ["GITHUB_", "RUNNER_", "ACTIONS_ID_TOKEN_"];

/**
 * The environment for any child process: a short allow-list of runner basics plus the GitHub and
 * OIDC variables npm provenance reads, plus exactly the variables in `extra`. process.env is never
 * forwarded wholesale and no registry token reaches a child unless `extra` names it.
 */
export function childEnv(source, extra = {}) {
  const out = {};
  for (const [key, value] of Object.entries(source ?? {})) {
    if (value === undefined || TOKEN_VARS.includes(key) || key === "GITHUB_TOKEN") continue;
    if (ENV_NAMES.has(key) || ENV_PREFIXES.some((prefix) => key.startsWith(prefix))) out[key] = value;
  }
  return { ...out, ...extra };
}

/** Run tar against `file` from its own directory by relative name: no drive-letter or host:path parsing. */
function tarCall(file, flags, spawn, encoding) {
  const result = spawn("tar", [...flags, path.basename(file)], { cwd: path.dirname(file), encoding, maxBuffer: 256 * 1024 * 1024, env: childEnv(process.env) });
  if (result.error || result.status !== 0) throw new Error(`tar ${flags.join(" ")} failed for ${path.basename(file)}: ${String(result.stderr ?? result.error?.message ?? "").slice(0, 300)}`);
  return result.stdout;
}
export const tarList = (file, spawn = spawnSync) => String(tarCall(file, ["-tzf"], spawn, "utf8")).split("\n").map((l) => l.replace(/\r$/, "")).filter(Boolean);
export function tarEntry(file, entry, spawn = spawnSync) {
  const result = spawn("tar", ["-xOzf", path.basename(file), entry], { cwd: path.dirname(file), encoding: "buffer", maxBuffer: 256 * 1024 * 1024, env: childEnv(process.env) });
  if (result.error || result.status !== 0) throw new Error(`cannot read ${entry} from ${path.basename(file)}`);
  return Buffer.from(result.stdout);
}

// ---- npm tarball checks -----------------------------------------------------------------------

const normalizeRepoUrl = (url) => String(url ?? "").replace(/^git\+/, "").replace(/\.git$/, "").replace(/\/$/, "");

/** npm provenance needs package.json `repository` to name the repository the workflow runs in. */
export function repositoryProblems(pkg, dir) {
  const repo = pkg?.repository;
  if (!repo || typeof repo !== "object") return [`${pkg?.name}: repository must be an object naming https://github.com/${REPOSITORY}; provenance fails without it`];
  const problems = [];
  if (normalizeRepoUrl(repo.url) !== `https://github.com/${REPOSITORY}`) problems.push(`${pkg.name}: repository.url ${JSON.stringify(repo.url)} must name https://github.com/${REPOSITORY}`);
  if (repo.directory !== dir) problems.push(`${pkg.name}: repository.directory ${JSON.stringify(repo.directory)} must be ${JSON.stringify(dir)}`);
  return problems;
}

function entryPoints(pkg) {
  const found = new Set();
  const rel = (value) => (typeof value === "string" ? value.replace(/^\.\//, "") : "");
  const add = (value) => { const r = rel(value); if (r && !r.includes("*")) found.add(r); };
  for (const key of ["main", "module", "types", "typings"]) add(pkg[key]);
  if (typeof pkg.bin === "string") add(pkg.bin);
  else if (pkg.bin && typeof pkg.bin === "object") Object.values(pkg.bin).forEach(add);
  const walk = (value) => { if (typeof value === "string") { if (value.startsWith("./")) add(value); } else if (value && typeof value === "object") Object.values(value).forEach(walk); };
  walk(pkg.exports);
  return [...found];
}

/** Everything checkable about one sealed npm tarball without installing or running it. Returns { problems, pkg, files }. */
export function checkNpmTarball({ file, name, version, dir, spawn = spawnSync }) {
  const problems = [];
  const files = new Set(tarList(file, spawn));
  let pkg = null;
  try { pkg = JSON.parse(tarEntry(file, "package/package.json", spawn).toString("utf8")); } catch (error) { return { problems: [error.message], pkg: null, files }; }
  if (pkg.name !== name || pkg.version !== version) problems.push(`${path.basename(file)} holds ${pkg.name}@${pkg.version}, expected ${name}@${version}`);
  if (/"workspace:|"file:|"link:/.test(JSON.stringify(pkg.dependencies ?? {}) + JSON.stringify(pkg.optionalDependencies ?? {}) + JSON.stringify(pkg.peerDependencies ?? {}))) problems.push(`${name}: packed package.json still holds a workspace:, file: or link: specifier`);
  problems.push(...repositoryProblems(pkg, dir));
  for (const entry of entryPoints(pkg)) if (!files.has(`package/${entry}`)) problems.push(`${name}: package.json points at ${entry}, which is not in the tarball`);
  return { problems, pkg, files };
}

// ---- crates.io upload body --------------------------------------------------------------------

/**
 * The crates.io publish request body (Cargo registry web API, "publish"): a 32-bit little-endian length
 * of the JSON metadata, the metadata, a 32-bit little-endian length of the .crate, the .crate bytes.
 */
export function buildCratePublishBody(metadataJson, crateBytes) {
  const json = Buffer.from(metadataJson, "utf8");
  const head = Buffer.alloc(4); head.writeUInt32LE(json.length, 0);
  const mid = Buffer.alloc(4); mid.writeUInt32LE(crateBytes.length, 0);
  return Buffer.concat([head, json, mid, crateBytes]);
}

// ---- sealed artifacts and verify receipts -----------------------------------------------------

export function listFiles(root) {
  const out = [];
  const walk = (dir) => { for (const entry of fs.readdirSync(dir, { withFileTypes: true })) { const full = path.join(dir, entry.name); if (entry.isDirectory()) walk(full); else out.push(full); } };
  if (fs.existsSync(root)) walk(root);
  return out;
}

/** Read manifest.json and re-hash every sealed file. Returns { manifest, manifestSha256, problems }. */
export function loadSealed({ packDir, registry, unitsText, run = {} }) {
  const problems = [];
  const manifestFile = path.join(packDir, "manifest.json");
  if (!fs.existsSync(manifestFile)) return { manifest: null, manifestSha256: null, problems: [`no manifest.json in ${packDir}`] };
  const bytes = fs.readFileSync(manifestFile);
  let manifest;
  try { manifest = JSON.parse(bytes.toString("utf8")); } catch { return { manifest: null, manifestSha256: null, problems: ["manifest.json is not JSON"] }; }
  const manifestSha256 = sha("sha256", bytes);
  if (manifest.schema !== 1) problems.push(`unknown manifest schema ${manifest.schema}`);
  if (manifest.registry !== registry) problems.push(`manifest registry ${manifest.registry} is not ${registry}`);
  if (!isSha(manifest.commit)) problems.push("manifest.commit is not a 40-hex commit");
  // The sealed artifact must come from THIS run (downloads default to the current run; this makes it checked, not assumed).
  if (run.runId !== undefined) {
    if (!/^\d+$/.test(String(manifest.runId ?? ""))) problems.push("manifest.runId is missing; the sealed bytes are not bound to a run");
    else if (String(manifest.runId) !== String(run.runId)) problems.push(`manifest run ${manifest.runId} is not this run ${run.runId}`);
    if (!/^\d+$/.test(String(manifest.runAttempt ?? ""))) problems.push("manifest.runAttempt is missing");
    else if (run.runAttempt !== undefined && Number(manifest.runAttempt) > Number(run.runAttempt)) problems.push(`manifest run attempt ${manifest.runAttempt} is later than this attempt ${run.runAttempt}`);
  }
  const names = (manifest.units ?? []).map((u) => u.name);
  const wanted = splitNames(unitsText);
  if (JSON.stringify(names) !== JSON.stringify(wanted)) problems.push(`sealed units [${names.join(", ")}] differ from the planned units [${wanted.join(", ")}]`);
  const listed = new Set(["manifest.json"]);
  for (const unit of manifest.units ?? []) {
    const files = [[unit.file, unit.sha256, unit.size], ...(unit.metaFile ? [[unit.metaFile, unit.metaSha256, undefined]] : [])];
    for (const [file, expected, size] of files) {
      if (!SAFE_FILE.test(file ?? "")) { problems.push(`${unit.name}: unsafe sealed file name ${JSON.stringify(file)}`); continue; }
      listed.add(file);
      const full = path.join(packDir, file);
      if (!fs.existsSync(full)) { problems.push(`${unit.name}: sealed file ${file} is missing`); continue; }
      const data = fs.readFileSync(full);
      if (sha("sha256", data) !== expected) problems.push(`${unit.name}: ${file} sha256 does not match the manifest; it changed after packing`);
      if (size !== undefined && data.length !== size) problems.push(`${unit.name}: ${file} is ${data.length} bytes, manifest says ${size}`);
    }
  }
  for (const file of listFiles(packDir)) {
    const rel = path.relative(packDir, file).split(path.sep).join("/");
    if (!listed.has(rel)) problems.push(`unlisted file in the sealed artifact: ${rel}`);
  }
  return { manifest, manifestSha256, problems };
}

/** Both hosts must have signed off on exactly the sealed bytes of this run. Returns problems. */
export function checkReceipts({ manifest, manifestSha256, receiptsDir, allow = {}, run = {} }) {
  const problems = [];
  const docs = [];
  for (const file of listFiles(receiptsDir)) {
    if (!file.endsWith(".json")) continue;
    try { const doc = JSON.parse(fs.readFileSync(file, "utf8")); if (doc?.kind === "verify") docs.push({ file, doc }); } catch { problems.push(`unreadable receipt ${path.basename(file)}`); }
  }
  const allowExcl = new Set(allow.hostExclusion ?? []); const allowNoTest = new Set(allow.noTest ?? []);
  const allowIgnored = new Set(allow.ignored ?? []); const allowAssets = new Set(allow.missingAssets ?? []);
  const sameSet = (a, b) => JSON.stringify([...(a ?? [])].sort()) === JSON.stringify([...b].sort());
  if (!run.runId) problems.push("this run's id is unknown; receipts cannot be bound to it");
  const statusByUnit = new Map(manifest.units.map((u) => [u.name, []]));
  for (const host of REQUIRED_HOSTS) {
    const mine = docs.filter((d) => d.doc.host === host);
    if (mine.length !== 1) { problems.push(`expected exactly one verify receipt from ${host}, found ${mine.length}`); continue; }
    const { doc } = mine[0];
    const bad = (why) => problems.push(`${host} receipt: ${why}`);
    if (doc.schema !== 1) bad(`unknown schema ${doc.schema}`);
    if (doc.commit !== manifest.commit) bad(`commit ${doc.commit} is not the sealed commit ${manifest.commit}`);
    if (doc.registry !== manifest.registry) bad(`registry ${doc.registry} is not ${manifest.registry}`);
    if (doc.manifestSha256 !== manifestSha256) bad("it verified a different manifest than the one being uploaded");
    if (run.runId && String(doc.runId) !== String(run.runId)) bad(`run ${doc.runId} is not this run ${run.runId}`);
    // Attempts: a re-run may reuse the sealed bytes (same run id, earlier attempt), but a receipt can neither
    // predate the manifest's attempt nor come from an attempt that has not happened, and it must name the
    // manifest attempt it verified.
    const attempt = Number(doc.runAttempt);
    if (!Number.isInteger(attempt) || attempt < 1) bad(`run attempt ${JSON.stringify(doc.runAttempt)} is not a positive integer`);
    else {
      if (manifest.runAttempt !== undefined && attempt < Number(manifest.runAttempt)) bad(`run attempt ${attempt} predates the sealed attempt ${manifest.runAttempt}`);
      if (run.runAttempt !== undefined && attempt > Number(run.runAttempt)) bad(`run attempt ${attempt} is later than this attempt ${run.runAttempt}`);
    }
    if (String(doc.manifestRunId ?? "") !== String(manifest.runId ?? "") || String(doc.manifestRunAttempt ?? "") !== String(manifest.runAttempt ?? "")) bad(`it verified manifest run ${doc.manifestRunId}/${doc.manifestRunAttempt}, not ${manifest.runId}/${manifest.runAttempt}`);
    if (!sameSet(doc.allowHostExclusion, allowExcl)) bad("it ran with different allow_host_exclusion than this request");
    if (!sameSet(doc.allowNoTest, allowNoTest)) bad("it ran with different allow_no_test than this request");
    if (!sameSet(doc.allowIgnored, allowIgnored)) bad("it ran with different allow_ignored than this request");
    if (!sameSet(doc.allowMissingAssets, allowAssets)) bad("it ran with different allow_missing_assets than this request");
    const byName = new Map((doc.units ?? []).map((u) => [u.name, u]));
    if ((doc.units ?? []).length !== manifest.units.length) bad(`covers ${(doc.units ?? []).length} units, the manifest has ${manifest.units.length}`);
    for (const unit of manifest.units) {
      const row = byName.get(unit.name);
      if (!row) { bad(`no evidence for ${unit.name}`); continue; }
      if (row.version !== unit.version || row.file !== unit.file || row.sha256 !== unit.sha256) bad(`${unit.name}: it verified different bytes (${row.file} ${String(row.sha256).slice(0, 12)}) than the sealed ${unit.file} ${unit.sha256.slice(0, 12)}`);
      if (row.bytesVerified !== true) bad(`${unit.name}: the sealed bytes were not verified`);
      if (!PASS_STATUSES.includes(row.status)) bad(`${unit.name}: status ${JSON.stringify(row.status)} is not a pass`);
      if (row.status === "allowed-exclusion" && !allowExcl.has(unit.name)) bad(`${unit.name}: excluded from this host without allow_host_exclusion`);
      if (row.status === "allowed-no-test" && !allowNoTest.has(unit.name)) bad(`${unit.name}: has no test script and is not named in allow_no_test`);
      if (row.status === "pass" && !(row.executed > 0 && (row.ignored ?? 0) === 0 && (row.skipped ?? 0) === 0)) bad(`${unit.name}: status pass without a positive executed count and zero ignored/skipped (executed ${row.executed}, ignored ${row.ignored}, skipped ${row.skipped})`);
      if (row.status === "allowed-skips") {
        if (!(row.executed > 0)) bad(`${unit.name}: allowed-skips without a positive executed count`);
        if ((row.ignored ?? 0) > 0 && !allowIgnored.has(unit.name)) bad(`${unit.name}: ${row.ignored} ignored test(s) and not named in allow_ignored`);
        if ((row.skipped ?? 0) > 0 && !allowAssets.has(unit.name)) bad(`${unit.name}: ${row.skipped} skipped test(s) and not named in allow_missing_assets`);
        if (!((row.ignored ?? 0) > 0 || (row.skipped ?? 0) > 0)) bad(`${unit.name}: allowed-skips with nothing ignored or skipped`);
      }
      // Each recorded test command is a required row on its own: a positive total never covers a row that ran nothing.
      if (row.status === "pass" || row.status === "allowed-skips") {
        for (const step of Array.isArray(row.steps) ? row.steps : []) if (step?.executed !== undefined && !(step.executed > 0)) bad(`${unit.name}: required test command \`${String(step.command).slice(0, 160)}\` executed no test`);
      }
      if (row.status !== "allowed-exclusion" && row.consumer?.verified !== true) bad(`${unit.name}: the installed or extracted shipped artifact was not consumer-verified on this host`);
      statusByUnit.get(unit.name)?.push(row.status);
    }
  }
  for (const [name, statuses] of statusByUnit) {
    if (statuses.length && !statuses.some((s) => s === "pass" || s === "allowed-skips") && !statuses.every((s) => s === "allowed-no-test")) problems.push(`${name}: no host passed it (${statuses.join(", ")})`);
  }
  return problems;
}

// ---- reviewer-approved artifacts --------------------------------------------------------------

/**
 * The committed list of artifact hashes a reviewer signed. The `release` environment has no required
 * reviewer, so this file is the human gate: an upload (and its dry run) proceeds only when EVERY sealed
 * unit has an entry with the exact registry, name, version and sha256. The path is fixed relative to this
 * script, which the workflow checks out at the dispatched commit; no input, argument or environment
 * variable can point at another file.
 */
export const APPROVED_ARTIFACTS_FILE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "ci", "approved-artifacts.json");
export const APPROVED_ENTRY_KEYS = ["name", "registry", "reviewRef", "reviewer", "sha256", "version"];
const SHA256_HEX = /^[0-9a-f]{64}$/;
const SEMVER_PLAIN = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

/** Read and validate the approved-artifacts file. Returns { entries, problems }; any defect is a problem (fail closed). */
export function loadApprovedArtifacts(file) {
  if (typeof file !== "string" || !file) return { entries: [], problems: ["no approved-artifacts file was provided; an upload needs ci/approved-artifacts.json from the dispatched commit"] };
  if (!fs.existsSync(file)) return { entries: [], problems: [`approved-artifacts file ${path.basename(file)} is missing; no artifact is reviewer-approved`] };
  let doc;
  try { doc = JSON.parse(fs.readFileSync(file, "utf8")); } catch { return { entries: [], problems: ["approved-artifacts file is not JSON"] }; }
  const problems = [];
  if (!doc || typeof doc !== "object" || Array.isArray(doc)) return { entries: [], problems: ["approved-artifacts file is not an object"] };
  const expectedKeys = doc.schema === 2 ? ["artifacts", "candidates", "schema"] : ["artifacts", "schema"];
  if (JSON.stringify(Object.keys(doc).sort()) !== JSON.stringify(expectedKeys)) problems.push(`approved-artifacts file must hold exactly the keys ${expectedKeys.join(", ")}`);
  if (doc.schema !== 1 && doc.schema !== 2) problems.push(`approved-artifacts schema ${JSON.stringify(doc.schema)} is not 1 or 2`);
  if (doc.schema === 2 && !Array.isArray(doc.candidates)) problems.push("approved-artifacts.candidates is not a list");
  if (!Array.isArray(doc.artifacts)) return { entries: [], problems: [...problems, "approved-artifacts.artifacts is not a list"] };
  const entries = []; const seen = new Set();
  doc.artifacts.forEach((entry, i) => {
    const at = `approved-artifacts entry ${i}`;
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) { problems.push(`${at} is not an object`); return; }
    const keys = Object.keys(entry).sort();
    if (JSON.stringify(keys) !== JSON.stringify(APPROVED_ENTRY_KEYS)) { problems.push(`${at} must hold exactly the keys ${APPROVED_ENTRY_KEYS.join(", ")}`); return; }
    const text = (v) => typeof v === "string" && v.trim() !== "" && v === v.trim();
    if (entry.registry !== "npm" && entry.registry !== "crates") problems.push(`${at}: registry must be npm or crates`);
    if (!text(entry.name)) problems.push(`${at}: name must be a non-empty string`);
    if (!SEMVER_PLAIN.test(entry.version ?? "")) problems.push(`${at}: version must be a plain MAJOR.MINOR.PATCH`);
    if (!SHA256_HEX.test(entry.sha256 ?? "")) problems.push(`${at}: sha256 must be 64 lowercase hex characters`);
    if (!text(entry.reviewer)) problems.push(`${at}: reviewer must be a non-empty string`);
    if (!text(entry.reviewRef)) problems.push(`${at}: reviewRef must be a non-empty string`);
    const id = `${entry.registry}\u0000${entry.name}\u0000${entry.version}`;
    if (seen.has(id)) problems.push(`${at}: ${entry.registry} ${entry.name}@${entry.version} is approved twice`);
    seen.add(id);
    entries.push(entry);
  });
  return { entries, problems };
}

/**
 * Every unit in the sealed manifest must have exactly one approved entry with the same registry, name,
 * version and sha256. A missing entry, a different hash, a malformed file or a duplicate unit refuses;
 * approvals for units that are not in this run are allowed (an approval is permission, not a request).
 * Returns problems.
 */
export function checkApprovedArtifacts({ manifest, registry, approvedFile }) {
  const { entries, problems } = loadApprovedArtifacts(approvedFile);
  if (!manifest || !Array.isArray(manifest.units) || manifest.units.length === 0) { problems.push("the sealed manifest lists no units to approve"); return problems; }
  if (manifest.registry !== registry) problems.push(`approval: manifest registry ${manifest.registry} is not ${registry}`);
  const names = new Set();
  for (const unit of manifest.units) {
    if (names.has(unit.name)) problems.push(`approval: ${unit.name} is sealed twice`);
    names.add(unit.name);
    const matches = entries.filter((e) => e.registry === registry && e.name === unit.name && e.version === unit.version);
    if (matches.length === 0) problems.push(`${unit.name}@${unit.version}: not reviewer-approved; ci/approved-artifacts.json has no ${registry} entry for it`);
    else if (!SHA256_HEX.test(unit.sha256 ?? "") || matches.some((e) => e.sha256 !== unit.sha256)) problems.push(`${unit.name}@${unit.version}: sealed sha256 ${String(unit.sha256).slice(0, 12)} is not the reviewer-approved ${matches[0].sha256.slice(0, 12)} (${matches[0].reviewRef})`);
  }
  return problems;
}

// ---- registry reads ---------------------------------------------------------------------------

async function getJson(url, fetchImpl, headers = {}) {
  const response = await fetchImpl(url, { headers: { "user-agent": USER_AGENT, ...headers }, redirect: "error", signal: AbortSignal.timeout(30_000) });
  if (response.status === 404) return null;
  if (!response.ok) throw new Error(`HTTP ${response.status} from ${url}`);
  return JSON.parse(await response.text());
}
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export async function waitForNpmBytes({ name, version, sha1, integrity: sri, fetchImpl, attempts = 40, pause = 10_000 }) {
  for (let i = 0; i < attempts; i += 1) {
    const doc = await getJson(`${NPM_REGISTRY_URL}${name.replace("/", "%2F")}/${version}`, fetchImpl, { accept: "application/json" }).catch(() => null);
    if (doc?.dist) {
      if (doc.dist.shasum !== sha1 || (doc.dist.integrity && doc.dist.integrity !== sri)) throw new Error(`npm serves different bytes for ${name}@${version} (shasum ${doc.dist.shasum}); inspect the registry`);
      return;
    }
    await sleep(pause);
  }
  throw new Error(`${name}@${version} is not visible on npm after publishing`);
}

/**
 * The crates.io publish answer (Cargo registry web API): `{"warnings":{"invalid_categories":[],"invalid_badges":[],"other":[]}}`
 * on success, `{"errors":[{"detail":"..."}]}` on failure (which crates.io can send with HTTP 200). Returns
 * { errors: [text], warnings: [text], unreadable }. Anything that is not a JSON object is "unreadable".
 */
export function parseCratesResponse(text) {
  let doc;
  try { doc = JSON.parse(String(text ?? "")); } catch { return { errors: [], warnings: [], unreadable: true }; }
  if (!doc || typeof doc !== "object" || Array.isArray(doc)) return { errors: [], warnings: [], unreadable: true };
  const errors = [];
  if (doc.errors !== undefined) {
    const list = Array.isArray(doc.errors) ? doc.errors : [doc.errors];
    for (const e of list) errors.push(typeof e === "string" ? e : String(e?.detail ?? JSON.stringify(e)));
  }
  const warnings = [];
  const w = doc.warnings;
  if (w && typeof w === "object") for (const [kind, items] of Object.entries(w)) for (const item of Array.isArray(items) ? items : [items]) if (item !== undefined && item !== null && item !== "") warnings.push(`${kind}: ${typeof item === "string" ? item : JSON.stringify(item)}`);
  return { errors, warnings, unreadable: false };
}

export function crateIndexPath(name) {
  const n = name.toLowerCase();
  if (n.length === 1) return `1/${n}`;
  if (n.length === 2) return `2/${n}`;
  if (n.length === 3) return `3/${n[0]}/${n}`;
  return `${n.slice(0, 2)}/${n.slice(2, 4)}/${n}`;
}

export async function waitForCrateIndex({ name, version, cksum, fetchImpl, attempts = 40, pause = 15_000 }) {
  for (let i = 0; i < attempts; i += 1) {
    const response = await fetchImpl(`${CRATES_INDEX_URL}${crateIndexPath(name)}`, { headers: { "user-agent": USER_AGENT }, redirect: "error", signal: AbortSignal.timeout(30_000) }).catch(() => null);
    if (response?.ok) {
      for (const line of (await response.text()).split("\n")) {
        if (!line.trim()) continue;
        const row = JSON.parse(line);
        if (row.vers === version) {
          if (row.cksum !== cksum) throw new Error(`crates.io checksum ${row.cksum} != sealed .crate ${cksum} for ${name}@${version}; inspect the registry`);
          return;
        }
      }
    }
    await sleep(pause);
  }
  throw new Error(`${name}@${version} is not in the crates.io index after publishing`);
}

// ---- upload -----------------------------------------------------------------------------------

/** The previous attempt's receipt for this same run, or null on a first attempt. Refuses a receipt that is not this run's. */
export function loadPreviousReceipt({ previousDir, registry, commit, run }) {
  const files = listFiles(previousDir).filter((f) => path.basename(f) === "publication-receipt.json");
  if (files.length === 0) return null;
  if (files.length > 1) throw new Error(`previous attempt receipt is ambiguous (${files.length} files)`);
  let doc;
  try { doc = JSON.parse(fs.readFileSync(files[0], "utf8")); } catch { throw new Error("previous attempt receipt is unreadable; refusing to continue without its evidence"); }
  const bad = [];
  if (doc?.schema !== 1) bad.push("unknown schema");
  if (doc?.dryRun !== false) bad.push("it is not a real publication receipt");
  if (doc?.registry !== registry) bad.push(`registry ${doc?.registry} is not ${registry}`);
  if (doc?.commit !== commit) bad.push(`commit ${doc?.commit} is not ${commit}`);
  if (String(doc?.runId) !== String(run.runId)) bad.push(`run ${doc?.runId} is not this run ${run.runId}`);
  if (!(Number(doc?.runAttempt) >= 1 && Number(doc.runAttempt) < Number(run.runAttempt))) bad.push(`attempt ${doc?.runAttempt} is not earlier than this attempt ${run.runAttempt}`);
  if (!Array.isArray(doc?.receipts)) bad.push("it has no receipts list");
  if (bad.length) throw new Error(`previous attempt receipt refused: ${bad.join("; ")}`);
  return doc;
}

class Receipt {
  constructor(dir, doc, previous = null) {
    this.file = path.join(dir, "publication-receipt.json"); fs.mkdirSync(dir, { recursive: true });
    this.attempt = String(doc.runAttempt);
    this.previous = previous;
    const carried = (previous?.receipts ?? []).map((r) => ({ ...r, attempt: r.attempt ?? String(previous.runAttempt) }));
    this.doc = { ...doc, priorAttempts: [...(previous?.priorAttempts ?? []), ...(previous ? [String(previous.runAttempt)] : [])], receipts: carried, history: [...(previous?.history ?? [])] };
    this.flush();
  }
  /** What the earlier attempt recorded for this unit, if anything. */
  earlier(unit) { return (this.previous?.receipts ?? []).find((r) => r.unit === unit) ?? null; }
  record(entry) {
    const full = { ...entry, attempt: this.attempt, registry: this.doc.registry, commit: this.doc.commit, dryRun: this.doc.dryRun, timestamp: new Date().toISOString() };
    const at = this.doc.receipts.findIndex((r) => r.unit === entry.unit);
    if (at >= 0) {
      // A superseded row from an earlier attempt is evidence: keep it, never overwrite it.
      if (this.doc.receipts[at].attempt !== this.attempt) this.doc.history.push(this.doc.receipts[at]);
      this.doc.receipts[at] = full;
    } else this.doc.receipts.push(full);
    this.flush();
  }
  flush() { fs.writeFileSync(this.file, `${JSON.stringify(this.doc, null, 2)}\n`); }
}

/**
 * What the registry holds for one planned unit: "absent", "same" (this exact version with the sealed hash),
 * or throws when it holds the version with DIFFERENT bytes or cannot be read (an ambiguous answer never
 * becomes permission to publish).
 */
export async function reconcileUnit({ registry, unit, fetchImpl }) {
  if (registry === "npm") {
    const doc = await getJson(`${NPM_REGISTRY_URL}${unit.name.replace("/", "%2F")}/${unit.version}`, fetchImpl, { accept: "application/json" });
    if (!doc?.dist) return "absent";
    if (doc.dist.shasum !== unit.sha1 || (doc.dist.integrity && doc.dist.integrity !== unit.integrity)) throw new Error(`npm already serves different bytes for ${unit.name}@${unit.version} (shasum ${doc.dist.shasum}, sealed ${unit.sha1}); refusing to publish`);
    return "same";
  }
  const response = await fetchImpl(`${CRATES_INDEX_URL}${crateIndexPath(unit.name)}`, { headers: { "user-agent": USER_AGENT }, redirect: "error", signal: AbortSignal.timeout(30_000) });
  if (response.status === 404) return "absent";
  if (!response.ok) throw new Error(`HTTP ${response.status} from the crates.io index for ${unit.name}`);
  for (const line of (await response.text()).split("\n")) {
    if (!line.trim()) continue;
    const row = JSON.parse(line);
    if (row.vers !== unit.version) continue;
    if (row.cksum !== unit.sha256) throw new Error(`crates.io already serves different bytes for ${unit.name}@${unit.version} (checksum ${row.cksum}, sealed ${unit.sha256}); refusing to publish`);
    return "same";
  }
  return "absent";
}

/**
 * Check everything, then upload (or, with dryRun, describe) each sealed unit in order. Injectable `spawn`
 * and `fetchImpl` let a test intercept every child process and HTTP call. Returns the receipt document.
 */
export async function executeUpload({ env, packDir, receiptsDir, approvedFile, previousDir = null, outDir, dryRun, candidateContext = null, spawn = spawnSync, fetchImpl = fetch, log = (line) => process.stdout.write(`${line}\n`), pause = 10_000 }) {
  if (candidateContext) {
    const { assertAuthenticatedContext } = await import("./reviewed-candidate.mjs");
    assertAuthenticatedContext(candidateContext, { env, packDir, receiptsDir, approvedFile });
  }
  const registry = env.PUBLISH_REGISTRY;
  if (registry !== "npm" && registry !== "crates") throw new Error(`registry must be npm or crates, got ${JSON.stringify(registry)}`);
  if (env.GITHUB_ACTIONS !== "true") throw new Error("upload runs in GitHub Actions only");
  if (env.GITHUB_REF !== "refs/heads/main") throw new Error(`upload runs from refs/heads/main only, not ${env.GITHUB_REF}`);
  if (env.GITHUB_REPOSITORY !== REPOSITORY) throw new Error(`upload runs in ${REPOSITORY} only, not ${env.GITHUB_REPOSITORY}`);
  const tokenVar = registry === "npm" ? "NODE_AUTH_TOKEN" : "CARGO_REGISTRY_TOKEN";
  const token = env[tokenVar] ?? "";
  if (dryRun) {
    for (const variable of TOKEN_VARS) if (env[variable]) throw new Error(`${variable} is present in a dry run; the dry-run job must not receive secrets`);
    if (env.ACTIONS_ID_TOKEN_REQUEST_URL || env.ACTIONS_ID_TOKEN_REQUEST_TOKEN) throw new Error("an OIDC token is present in a dry run; the dry-run job must not request id-token");
  } else {
    if (!token) throw new Error(`${tokenVar} is not set for a real upload`);
    for (const variable of TOKEN_VARS) if (variable !== tokenVar && env[variable]) throw new Error(`${variable} must not be present while uploading to ${registry}`);
    if (registry === "npm" && !(env.ACTIONS_ID_TOKEN_REQUEST_URL && env.ACTIONS_ID_TOKEN_REQUEST_TOKEN)) throw new Error("npm provenance needs id-token: write; no OIDC request token is present");
  }
  const allow = { hostExclusion: splitNames(env.PUBLISH_ALLOW_HOST_EXCLUSION), noTest: splitNames(env.PUBLISH_ALLOW_NO_TEST), ignored: splitNames(env.PUBLISH_ALLOW_IGNORED), missingAssets: splitNames(env.PUBLISH_ALLOW_MISSING_ASSETS) };
  if (!/^\d+$/.test(env.GITHUB_RUN_ID ?? "") || !/^\d+$/.test(env.GITHUB_RUN_ATTEMPT ?? "")) throw new Error("GITHUB_RUN_ID and GITHUB_RUN_ATTEMPT must be set; the sealed bytes and receipts are bound to this run");
  const promotionRun = { runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT };
  const run = candidateContext ? { runId: candidateContext.candidate.runId, runAttempt: candidateContext.candidate.runAttempt } : promotionRun;
  const { manifest, manifestSha256, problems } = loadSealed({ packDir, registry, unitsText: env.PUBLISH_UNITS, run });
  if (manifest) {
    const sourceSha = candidateContext?.candidate.sourceSha ?? env.GITHUB_SHA;
    if (manifest.commit !== sourceSha) problems.push(`sealed commit ${manifest.commit} is not the dispatched commit ${sourceSha}`);
    problems.push(...checkApprovedArtifacts({ manifest, registry, approvedFile }));
    problems.push(...checkReceipts({ manifest, manifestSha256, receiptsDir, allow, run }));
    if (registry === "npm") {
      for (const unit of manifest.units) {
        try { problems.push(...checkNpmTarball({ file: path.join(packDir, unit.file), name: unit.name, version: unit.version, dir: unit.dir, spawn }).problems); } catch (error) { problems.push(`${unit.name}: ${error.message}`); }
      }
    } else {
      for (const unit of manifest.units) {
        try {
          const meta = JSON.parse(fs.readFileSync(path.join(packDir, unit.metaFile), "utf8"));
          if (meta.name !== unit.name || meta.vers !== unit.version || !Array.isArray(meta.deps)) problems.push(`${unit.name}: upload metadata does not describe ${unit.name}@${unit.version}`);
        } catch { problems.push(`${unit.name}: upload metadata is unreadable`); }
      }
    }
  }
  if (problems.length) throw new Error(`sealed upload refused:\n  - ${problems.join("\n  - ")}`);

  const previous = !dryRun && previousDir ? loadPreviousReceipt({ previousDir, registry, commit: manifest.commit, run: promotionRun }) : null;
  const receipt = new Receipt(outDir, { schema: 1, registry, dryRun, commit: manifest.commit, repository: env.GITHUB_REPOSITORY, runId: env.GITHUB_RUN_ID ?? "", runAttempt: env.GITHUB_RUN_ATTEMPT ?? "", ...(candidateContext ? { promotionCommit: env.GITHUB_SHA, candidate: candidateContext.candidate } : {}) }, previous);
  const redact = (text) => (token ? String(text ?? "").split(token).join("<redacted>") : String(text ?? ""));
  log(`sealed ${manifest.units.length} ${registry} unit(s) at ${manifest.commit}; both hosts verified the same bytes`);
  if (previous) log(`continuing run ${run.runId} from attempt ${previous.runAttempt}'s receipt (${previous.receipts.length} unit row(s))`);

  // Reconcile EVERY planned unit with the registry before sending anything: a unit already there with the sealed
  // hash was published by an earlier attempt; the same version with other bytes, or an unreadable answer, stops the run.
  const done = new Set();
  if (!dryRun) {
    for (const unit of manifest.units) {
      const base = { unit: unit.name, version: unit.version, artifact: unit.file, sha256: unit.sha256, ...(registry === "npm" ? { sha1: unit.sha1 } : {}) };
      let state;
      try { state = await reconcileUnit({ registry, unit, fetchImpl }); } catch (error) {
        receipt.record({ ...base, published: false, registryVerified: false, outcome: "refused", error: redact(error.message).slice(0, 300) });
        throw new Error(`cannot reconcile ${unit.name}@${unit.version} with the ${registry} registry: ${redact(error.message)}`);
      }
      const earlier = receipt.earlier(unit.name);
      if (state === "same") {
        receipt.record({ ...base, published: true, registryVerified: true, outcome: "published-by-earlier-attempt", publishedBy: "earlier-attempt", earlierAttempt: earlier?.attempt ?? null, earlierOutcome: earlier?.outcome ?? null });
        log(`${unit.name}@${unit.version} is already on ${registry} with the sealed hash: published by an earlier attempt`);
        done.add(unit.name);
      } else if (earlier?.registryVerified === true) {
        const why = `${unit.name}@${unit.version} was recorded as published and verified by attempt ${earlier.attempt}, but the ${registry} registry no longer serves it`;
        receipt.record({ ...base, published: false, registryVerified: false, outcome: "refused", error: why });
        throw new Error(why);
      }
    }
  }

  if (registry === "npm") {
    const scratch = fs.mkdtempSync(path.join(env.RUNNER_TEMP || os.tmpdir(), "rk-npmrc-"));
    const userconfig = path.join(scratch, "npmrc");
    // npm substitutes ${NODE_AUTH_TOKEN} itself; the file holds no secret.
    fs.writeFileSync(userconfig, "//registry.npmjs.org/:_authToken=${NODE_AUTH_TOKEN}\n", { mode: 0o600 });
    try {
      for (const unit of manifest.units) {
        const file = path.join(packDir, unit.file);
        const args = ["publish", file, "--ignore-scripts", "--access", "public", "--provenance", "--registry", NPM_REGISTRY_URL, "--userconfig", userconfig];
        const base = { unit: unit.name, version: unit.version, artifact: unit.file, sha256: unit.sha256, sha1: unit.sha1 };
        if (dryRun) { log(`dry run: would run npm ${args.map((a) => (a === file ? unit.file : a === userconfig ? "<userconfig>" : a)).join(" ")}`); receipt.record({ ...base, published: false, registryVerified: false }); continue; }
        if (done.has(unit.name)) continue;
        let published = false;
        try {
          receipt.record({ ...base, published: false, registryVerified: false, outcome: "pending" });
          const result = spawn("npm", args, { encoding: "utf8", env: childEnv(env, { NODE_AUTH_TOKEN: token }), cwd: packDir });
          const out = redact(`${result.stdout ?? ""}${result.stderr ?? ""}`).trim();
          if (out) log(out);
          if (result.error || result.status !== 0) throw new Error(`npm publish failed for ${unit.name}@${unit.version}`);
          published = true;
          receipt.record({ ...base, published: true, registryVerified: false, outcome: "published-unverified" });
          await waitForNpmBytes({ name: unit.name, version: unit.version, sha1: unit.sha1, integrity: unit.integrity, fetchImpl, pause });
          receipt.record({ ...base, published: true, registryVerified: true, outcome: "published" });
        } catch (error) {
          receipt.record({ ...base, published, registryVerified: false, outcome: published ? "published-unverified" : "failed", error: redact(error.message).slice(0, 300) });
          throw error;
        }
      }
    } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
  } else {
    for (const unit of manifest.units) {
      const crateBytes = fs.readFileSync(path.join(packDir, unit.file));
      const body = buildCratePublishBody(fs.readFileSync(path.join(packDir, unit.metaFile), "utf8"), crateBytes);
      const base = { unit: unit.name, version: unit.version, artifact: unit.file, sha256: unit.sha256 };
      if (dryRun) { log(`dry run: would PUT ${CRATES_PUBLISH_URL} with ${body.length} bytes for ${unit.name}@${unit.version}`); receipt.record({ ...base, published: false, registryVerified: false }); continue; }
      if (done.has(unit.name)) continue;
      let published = false;
      try {
        receipt.record({ ...base, published: false, registryVerified: false, outcome: "pending" });
        // The token is passed to this one HTTP call. redirect: "error" keeps it from following a redirect elsewhere.
        const response = await fetchImpl(CRATES_PUBLISH_URL, { method: "PUT", headers: { authorization: token, accept: "application/json", "content-type": "application/octet-stream", "user-agent": USER_AGENT }, body, redirect: "error", signal: AbortSignal.timeout(300_000) });
        const text = redact(await response.text());
        if (!response.ok) throw new Error(`crates.io refused ${unit.name}@${unit.version}: HTTP ${response.status} ${text.slice(0, 600)}`);
        const answer = parseCratesResponse(text);
        if (answer.errors.length) throw new Error(`crates.io answered HTTP ${response.status} with errors for ${unit.name}@${unit.version}; not recorded as published: ${answer.errors.join("; ").slice(0, 600)}`);
        if (answer.unreadable) throw new Error(`crates.io answered HTTP ${response.status} with a body that is not the documented JSON for ${unit.name}@${unit.version}; not recorded as published (inspect the registry): ${text.slice(0, 200)}`);
        if (answer.warnings.length) log(`crates.io warnings for ${unit.name}@${unit.version}: ${answer.warnings.join("; ").slice(0, 600)}`);
        published = true;
        receipt.record({ ...base, published: true, registryVerified: false, outcome: "published-unverified", warnings: answer.warnings });
        await waitForCrateIndex({ name: unit.name, version: unit.version, cksum: unit.sha256, fetchImpl, pause: pause * 1.5 });
        receipt.record({ ...base, published: true, registryVerified: true, outcome: "published", warnings: answer.warnings });
      } catch (error) {
        // A network failure leaves the outcome ambiguous: the row says so, and the next attempt reconciles with the registry.
        receipt.record({ ...base, published, registryVerified: false, outcome: published ? "published-unverified" : "failed", error: redact(error.message).slice(0, 300) });
        throw error;
      }
    }
  }
  return receipt.doc;
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const [command, ...flags] = process.argv.slice(2);
  if (command !== "run" || flags.some((f) => f !== "--dry-run")) { process.stderr.write("usage: upload.mjs run [--dry-run]\n"); process.exit(2); }
  const temp = process.env.RUNNER_TEMP || os.tmpdir();
  const sealed = process.env.SEALED_DIR || path.join(temp, "sealed");
  try {
    await executeUpload({ env: process.env, packDir: path.join(sealed, "packed"), receiptsDir: path.join(sealed, "receipts"), approvedFile: APPROVED_ARTIFACTS_FILE, previousDir: path.join(sealed, "previous"), outDir: path.join(temp, "publication-receipt"), dryRun: flags.includes("--dry-run") });
  } catch (error) { process.stderr.write(`upload: ${error.message}\n`); process.exit(1); }
}
