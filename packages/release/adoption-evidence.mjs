#!/usr/bin/env node
// right-release adoption-evidence: PK17-6 adoption-evidence validation (RV11, Q8).
//
// Two checks, both fail-closed, neither ever satisfied by prose (CHANGELOG, deletion lists, summaries):
//
//   1. The delivery manifest (`published-release-manifest.json`, packet 01 section 6): schema, derived fields recomputed from
//      their sources (capability status from packages and qualification rows, contract rollups, task `ready`, `readyForAdoption`),
//      and the AP id set equal to the ids extracted from the frozen app packets (an omission, an extra id or a duplicate fails).
//   2. An app's adoption record (`<app>/rightkit-adoption.json`): for every adopted capability, the manifest package must be
//      pinned by the app's lockfile at the exact version and integrity, the active target and feature closure (a saved
//      `cargo tree -e features` text, hashed) must be one the manifest qualified, and a machine-readable exercised-entrypoint
//      receipt (kind `test` or `run`, exit code 0, bound to the pinned versions, target, entrypoint and lockfile hashes) must exist.
//      A task that is not `ready` in the manifest, or is N/A, is never adopted.
//
//   right-release adoption-evidence --manifest <published-release-manifest.json> [--packets <dir>] [--json]
//                                   [--app <dir> [--adoption <file>] [--target <win32-x64|darwin-arm64|...>]]
//
//   Without --app only the manifest is validated. --packets defaults to the directory above the manifest's directory (the layout
//   docs/plans/<set>/handoff/published-release-manifest.json beside 02-*.md .. 07-*.md); no packets found is a finding, never a skip.
//   Exit codes: 0 no findings, 1 findings, 2 usage error.
//
// Adoption record (schema 1):
//   { "schema": 1, "app": "heardright", "manifestSha256": "<sha256 of the manifest file>", "target": "win32-x64",
//     "locks": { "cargo": "src-tauri/Cargo.lock", "pnpm": "pnpm-lock.yaml", "swift": "Package.resolved" },   // optional; defaults below
//     "cargoTree": { "file": "evidence/cargo-tree-win32-x64.txt", "sha256": "<hex>" },                        // required with any cargo package
//     "tasks": { "AP-HR-03": { "adopted": [ {
//        "capability": "PK03.helper-trust",
//        "packages": [ { "id": "cargo:rightkit-credentials", "features": ["helper-trust"] } ],   // or a bare id; features are the ones the app requests
//        "entrypoints": ["src-tauri/src/vault.rs#open_vault"],
//        "receipt": { "file": "evidence/AP-HR-03-helper-trust.json", "sha256": "<hex>" } } ] } } }
// Receipt (schema 1, JSON only): { "schema": 1, "kind": "test|run", "app", "capability", "entrypoint", "packages": ["<id>@<version>"],
//   "target", "command": ["..."], "exitCode": 0, "finishedAt": "<iso>", "lockfileSha256": { "cargo": "<hex>" } }
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, realpathSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parse as parseToml } from "smol-toml";

export const APP_PREFIX = Object.freeze({ pulse: "PU", heardright: "HR", viewright: "VR", mailright: "MR", scraperight: "SR", coderight: "CR" });
export const PK_IDS = Object.freeze(Array.from({ length: 17 }, (_, i) => `PK${String(i + 1).padStart(2, "0")}`));
export const DEFERRED_CAPABILITY = "PK14.fixture-capture";
const ECOSYSTEMS = new Set(["cargo", "npm", "swift"]);
const HOST_FOR_TARGET = [[/^win32-/, "win32"], [/^darwin-/, "darwin"], [/^linux-/, "linux-pr"], [/^android-/, "android-compile"]];
const sha256Of = (bytes) => createHash("sha256").update(bytes).digest("hex");
const isHex = (value, length) => typeof value === "string" && new RegExp(`^[0-9a-f]{${length}}$`).test(value);
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const finding = (rule, reason, extra = {}) => ({ rule, reason, ...extra });

// ---- strict JSON (duplicate keys are an error, not last-wins) ---------------------------------------------------------------
export function parseJsonStrict(text, label = "JSON") {
  const source = String(text).replace(/^﻿/, "");
  let i = 0;
  const fail = (message) => { throw new Error(`${label}: ${message} at offset ${i}`); };
  const skip = () => { while (i < source.length && (source[i] === " " || source[i] === "\t" || source[i] === "\r" || source[i] === "\n")) i += 1; };
  const STRING = /"(?:[^"\\\u0000-\u001f]|\\(?:["\\/bfnrt]|u[0-9a-fA-F]{4}))*"/y;
  const LITERAL = /-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?|true|false|null/y;
  const readString = () => {
    STRING.lastIndex = i;
    const m = STRING.exec(source);
    if (!m) fail("malformed string");
    i = STRING.lastIndex;
    return JSON.parse(m[0]);
  };
  const readValue = () => {
    skip();
    const c = source[i];
    if (c === "{") {
      i += 1;
      const object = {}; const keys = new Set();
      skip();
      if (source[i] === "}") { i += 1; return object; }
      for (;;) {
        skip();
        if (source[i] !== '"') fail("expected a string key");
        const key = readString();
        if (keys.has(key)) fail(`duplicate key ${JSON.stringify(key)}`);
        keys.add(key);
        skip();
        if (source[i] !== ":") fail("expected ':'");
        i += 1;
        Object.defineProperty(object, key, { value: readValue(), enumerable: true, writable: true, configurable: true });
        skip();
        if (source[i] === ",") { i += 1; continue; }
        if (source[i] === "}") { i += 1; return object; }
        fail("expected ',' or '}'");
      }
    }
    if (c === "[") {
      i += 1;
      const list = [];
      skip();
      if (source[i] === "]") { i += 1; return list; }
      for (;;) {
        list.push(readValue());
        skip();
        if (source[i] === ",") { i += 1; continue; }
        if (source[i] === "]") { i += 1; return list; }
        fail("expected ',' or ']'");
      }
    }
    if (c === '"') return readString();
    LITERAL.lastIndex = i;
    const m = LITERAL.exec(source);
    if (!m) fail("unexpected token");
    i = LITERAL.lastIndex;
    return JSON.parse(m[0]);
  };
  const value = readValue();
  skip();
  if (i !== source.length) fail("trailing characters");
  return value;
}

export function loadManifest(file) {
  const bytes = readFileSync(file);
  return { manifest: parseJsonStrict(bytes.toString("utf8"), path.basename(file)), sha256: sha256Of(bytes), bytes };
}

// ---- frozen packet task ids ---------------------------------------------------------------------------------------------
const TASK_DEFINITION = /^- \[[ xX]\] \*{0,2}(AP-[A-Z]{2}-\d{2})\b/;

/** Task ids defined by the frozen app packets 02..07 (`- [ ] AP-XX-nn` lines): { ids, duplicates, files }. */
export function extractPacketTaskIds(packetDir) {
  const files = readdirSync(packetDir).filter((name) => /^0[2-7]-.+\.md$/.test(name)).sort();
  const counts = new Map();
  for (const name of files) {
    for (const line of readFileSync(path.join(packetDir, name), "utf8").split(/\r?\n/)) {
      const m = TASK_DEFINITION.exec(line);
      if (m) counts.set(m[1], (counts.get(m[1]) ?? 0) + 1);
    }
  }
  return { ids: [...counts.keys()].sort(), duplicates: [...counts].filter(([, n]) => n > 1).map(([id]) => id).sort(), files };
}

// ---- manifest validation --------------------------------------------------------------------------------------------------
const INTEGRITY = { cargo: (v) => isHex(v, 64), npm: (v) => typeof v === "string" && /^sha512-[A-Za-z0-9+/]+={0,2}$/.test(v), swift: (v) => isHex(v, 40) };
const nonEmptyString = (v) => typeof v === "string" && v.trim().length > 0;
const packageKey = (p) => `${p.id}@${p.version}`;

function validatePackage(p, at, out) {
  const bad = (rule, reason) => out.push(finding(rule, `${at}: ${reason}`));
  if (!isObject(p)) { bad("package-invalid", "must be an object"); return; }
  const m = /^(cargo|npm|swift):(.+)$/.exec(String(p.id));
  if (!m) bad("package-invalid", `id must be <cargo|npm|swift>:<name>, got ${JSON.stringify(p.id)}`);
  else {
    if (p.ecosystem !== m[1]) bad("package-invalid", `ecosystem ${JSON.stringify(p.ecosystem)} does not match id ${p.id}`);
    if (p.name !== m[2]) bad("package-invalid", `name ${JSON.stringify(p.name)} does not match id ${p.id}`);
  }
  if (!ECOSYSTEMS.has(p.ecosystem)) bad("package-invalid", "ecosystem must be cargo, npm or swift");
  if (!nonEmptyString(p.version) || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(p.version)) bad("package-invalid", `version must be exact semver, got ${JSON.stringify(p.version)}`);
  if (ECOSYSTEMS.has(p.ecosystem) && !INTEGRITY[p.ecosystem](p.integrity)) bad("package-invalid", `integrity is not a valid ${p.ecosystem} integrity value`);
  if (!isHex(p.archiveSha256, 64)) bad("package-invalid", "archiveSha256 must be 64 hex characters");
  if (!nonEmptyString(p.archiveUrl)) bad("package-invalid", "archiveUrl is required");
  if (p.sourceCommit === null) { if (!nonEmptyString(p.sourceCommitNullReason)) bad("package-invalid", "sourceCommitNullReason is required exactly when sourceCommit is null"); }
  else {
    if (!isHex(p.sourceCommit, 40)) bad("package-invalid", "sourceCommit must be a 40-hex commit or null");
    if (p.sourceCommitNullReason !== undefined && p.sourceCommitNullReason !== null) bad("package-invalid", "sourceCommitNullReason must be null unless sourceCommit is null");
  }
  if (!Array.isArray(p.features) || p.features.some((f) => typeof f !== "string")) bad("package-invalid", "features must be an array of strings");
  if (!Array.isArray(p.targets) || !p.targets.length || p.targets.some((t) => typeof t !== "string")) bad("package-invalid", "targets must be a non-empty array of strings");
  if (!Array.isArray(p.qualification)) bad("package-invalid", "qualification must be an array");
  else {
    for (const row of p.qualification) {
      if (!isObject(row) || !nonEmptyString(row.host) || typeof row.features !== "string" || !["pass", "fail", "not-applicable"].includes(row.result)) bad("package-invalid", `qualification row is malformed: ${JSON.stringify(row)}`);
      else if (!nonEmptyString(row.gateEvidence)) bad("package-invalid", `qualification row for ${row.host} has no gateEvidence`);
    }
  }
  if (!isObject(p.fetchedConsumer) || typeof p.fetchedConsumer.ok !== "boolean") bad("package-invalid", "fetchedConsumer.ok (boolean) is required");
  if (p.event === "publish") {
    if (!isObject(p.publisherTool)) bad("package-invalid", "a publish event needs publisherTool");
    if (p.originalPublication != null || p.qualifier != null) bad("package-invalid", "a publish event has null originalPublication and qualifier");
    if (!isObject(p.packedConsumer) || typeof p.packedConsumer.ok !== "boolean") bad("package-invalid", "a publish event needs packedConsumer.ok");
  } else if (p.event === "qualify-existing") {
    if (p.publisherTool !== null) bad("package-invalid", "a qualify-existing event has publisherTool null");
    if (!isObject(p.originalPublication) || !isObject(p.qualifier)) bad("package-invalid", "a qualify-existing event needs originalPublication and qualifier");
    if (!isObject(p.packedConsumer) || p.packedConsumer.ok !== null || p.packedConsumer.notApplicable !== "unchanged published bytes") bad("package-invalid", "a qualify-existing event has packedConsumer { ok: null, notApplicable: \"unchanged published bytes\" }");
  } else bad("package-invalid", `event must be publish or qualify-existing, got ${JSON.stringify(p.event)}`);
}

/** Capability status that follows from its packages: [] when satisfiable, else the reasons it is not. */
function satisfactionProblems(capability, packagesByKey, packagesById) {
  const problems = [];
  if (!Array.isArray(capability.evidence) || !capability.evidence.length || capability.evidence.some((e) => !nonEmptyString(e))) problems.push("no evidence paths");
  for (const ref of Array.isArray(capability.packageIds) ? capability.packageIds : []) {
    const p = packagesByKey.get(ref) ?? (packagesById.get(ref)?.length === 1 ? packagesById.get(ref)[0] : null);
    if (!p) { problems.push(`package ${ref} is not in the manifest (or is ambiguous without a version)`); continue; }
    if (p.fetchedConsumer?.ok !== true) problems.push(`package ${ref} has no passing fetchedConsumer`);
    const rows = Array.isArray(p.qualification) ? p.qualification : [];
    if (rows.some((r) => r.result === "fail")) problems.push(`package ${ref} has a failing qualification row`);
    if (!rows.some((r) => r.result === "pass")) problems.push(`package ${ref} has no passing qualification row`);
  }
  return problems;
}

function contractStatus(slices, caps) {
  const live = slices.filter((id) => caps[id]?.status !== "deferred-separate-dispatch");
  const satisfied = live.filter((id) => caps[id]?.status === "satisfied");
  if (satisfied.length === live.length) return "satisfied";
  return satisfied.length ? "partial" : "pending";
}

/** Readiness derived from capability statuses alone: { <AP-id>: { app, ready, reasons } }. Never reads the stored `ready`. */
export function deriveReadiness(manifest) {
  const caps = isObject(manifest?.capabilities) ? manifest.capabilities : {};
  const result = {};
  for (const [app, entry] of Object.entries(isObject(manifest?.apps) ? manifest.apps : {})) {
    for (const [id, task] of Object.entries(isObject(entry?.tasks) ? entry.tasks : {})) {
      const reasons = [];
      if (task?.applicable !== true) reasons.push("not applicable");
      if (task?.applicabilityResolved !== true) reasons.push("applicability unresolved");
      if (!Array.isArray(task?.requiresCapabilities)) reasons.push("requiresCapabilities missing");
      else for (const cap of task.requiresCapabilities) if (caps[cap]?.status !== "satisfied") reasons.push(`capability ${cap} is ${caps[cap]?.status ?? "unknown"}`);
      result[id] = { app, ready: reasons.length === 0, reasons };
    }
  }
  return result;
}

/**
 * Validate a parsed manifest. `packetIds` ({ ids, duplicates }) is the frozen packet task set; without it the AP-set check is a
 * finding. `baseDir` (the manifest's directory) enables the selected-snapshot hash and the evidence file checks.
 */
export function validateManifest(manifest, { packetIds, baseDir } = {}) {
  const out = [];
  const add = (rule, reason) => out.push(finding(rule, reason));
  if (!isObject(manifest)) return [finding("manifest-invalid", "manifest must be a JSON object")];
  if (manifest.schemaVersion !== 1) add("manifest-invalid", `schemaVersion must be 1, got ${JSON.stringify(manifest.schemaVersion)}`);
  if (!isObject(manifest.spec) || !isHex(manifest.spec.commit, 40) || !isHex(manifest.spec.sha256, 64)) add("manifest-invalid", "spec { commit (40 hex), sha256 (64 hex) } is required");
  const selected = manifest.selectedSnapshot;
  if (!isObject(selected) || !nonEmptyString(selected.path) || !isHex(selected.sha256, 64)) add("manifest-invalid", "selectedSnapshot { path, sha256 } is required");
  else if (baseDir) {
    const read = readInside(baseDir, selected.path);
    if (read.error) add("snapshot-unreadable", `selectedSnapshot ${selected.path}: ${read.error}`);
    else if (sha256Of(read.bytes) !== selected.sha256) add("snapshot-hash-mismatch", `selectedSnapshot ${selected.path} has sha256 ${sha256Of(read.bytes)}, the manifest says ${selected.sha256}`);
  }

  // packages
  const packagesByKey = new Map(); const packagesById = new Map();
  if (!Array.isArray(manifest.packages)) add("manifest-invalid", "packages must be an array");
  else {
    manifest.packages.forEach((p, index) => {
      validatePackage(p, `packages[${index}]${isObject(p) && p.id ? ` ${p.id}` : ""}`, out);
      if (!isObject(p) || !p.id) return;
      const key = packageKey(p);
      if (packagesByKey.has(key)) add("package-duplicate", `package ${key} is listed twice`);
      packagesByKey.set(key, p);
      packagesById.set(p.id, [...(packagesById.get(p.id) ?? []), p]);
    });
  }

  // contracts and capabilities
  const caps = isObject(manifest.capabilities) ? manifest.capabilities : null;
  const contracts = isObject(manifest.contracts) ? manifest.contracts : null;
  if (!caps) add("manifest-invalid", "capabilities must be an object");
  if (!contracts) add("manifest-invalid", "contracts must be an object");
  if (caps && contracts) {
    const contractIds = Object.keys(contracts);
    for (const id of PK_IDS) if (!contractIds.includes(id)) add("contract-missing", `contracts has no ${id}`);
    for (const id of contractIds) if (!PK_IDS.includes(id)) add("contract-unknown", `contracts lists ${id}, which is not PK01..PK17`);
    const listed = new Map();
    for (const [id, contract] of Object.entries(contracts)) {
      if (!isObject(contract) || !Array.isArray(contract.capabilities)) { add("contract-invalid", `${id}: capabilities must be an array`); continue; }
      for (const cap of contract.capabilities) {
        if (listed.has(cap)) add("capability-duplicate", `capability ${cap} is listed by ${listed.get(cap)} and ${id}`);
        listed.set(cap, id);
        if (!cap.startsWith(`${id}.`)) add("capability-misplaced", `capability ${cap} is listed under ${id}`);
        if (!Object.hasOwn(caps, cap)) add("capability-missing", `contracts.${id} lists ${cap}, which capabilities does not define`);
      }
      const expected = contractStatus(contract.capabilities, caps);
      if (contract.status !== expected) add("contract-not-derived", `contracts.${id}.status is ${JSON.stringify(contract.status)} but its capabilities derive ${expected}`);
    }
    for (const [id, capability] of Object.entries(caps)) {
      if (!listed.has(id)) add("capability-orphan", `capability ${id} is not listed by any contract`);
      if (!isObject(capability)) { add("capability-invalid", `${id}: must be an object`); continue; }
      if (!["satisfied", "pending", "deferred-separate-dispatch"].includes(capability.status)) add("capability-invalid", `${id}: status ${JSON.stringify(capability.status)} is not satisfied, pending or deferred-separate-dispatch`);
      if (capability.status === "deferred-separate-dispatch" && id !== DEFERRED_CAPABILITY) add("capability-invalid", `${id}: deferred-separate-dispatch is allowed only for ${DEFERRED_CAPABILITY}`);
      if (!Array.isArray(capability.packageIds)) add("capability-invalid", `${id}: packageIds must be an array`);
      if (capability.status === "satisfied") {
        for (const problem of satisfactionProblems(capability, packagesByKey, packagesById)) add("capability-not-satisfied", `${id} is marked satisfied but: ${problem}`);
        if (baseDir) for (const evidence of Array.isArray(capability.evidence) ? capability.evidence : []) {
          if (typeof evidence !== "string") continue;
          const read = readInside(baseDir, evidence);
          if (read.error) { add("evidence-unreadable", `${id}: evidence ${evidence}: ${read.error}`); continue; }
          let snapshot = null;
          try { snapshot = parseJsonStrict(read.bytes.toString("utf8"), evidence); } catch { snapshot = null; }
          const claimed = snapshot?.package;
          if (isObject(claimed)) {
            const match = packagesByKey.get(packageKey(claimed));
            if (!match || match.integrity !== claimed.integrity || match.archiveSha256 !== claimed.archiveSha256) add("snapshot-mismatch", `${id}: evidence ${evidence} records ${packageKey(claimed)} with an integrity the manifest does not carry`);
          }
        }
      }
    }
  }

  // apps
  const derived = deriveReadiness(manifest);
  const apps = isObject(manifest.apps) ? manifest.apps : null;
  if (!apps) add("manifest-invalid", "apps must be an object");
  else {
    for (const app of Object.keys(APP_PREFIX)) if (!Object.hasOwn(apps, app)) add("app-missing", `apps has no ${app}`);
    const manifestIds = [];
    for (const [app, entry] of Object.entries(apps)) {
      if (!Object.hasOwn(APP_PREFIX, app)) { add("app-unknown", `apps lists ${app}, which is not one of ${Object.keys(APP_PREFIX).join(", ")}`); continue; }
      if (!isObject(entry) || !isObject(entry.tasks) || !Array.isArray(entry.readyForAdoption)) { add("app-invalid", `${app}: tasks (object) and readyForAdoption (array) are required`); continue; }
      for (const [id, task] of Object.entries(entry.tasks)) {
        manifestIds.push(id);
        if (!new RegExp(`^AP-${APP_PREFIX[app]}-\\d{2}$`).test(id)) { add("task-misplaced", `${id} is listed under ${app}`); continue; }
        if (!isObject(task)) { add("task-invalid", `${id}: must be an object`); continue; }
        if (typeof task.applicable !== "boolean" || typeof task.applicabilityResolved !== "boolean" || typeof task.ready !== "boolean") add("task-invalid", `${id}: applicable, applicabilityResolved and ready must be booleans`);
        if (!Array.isArray(task.requiresCapabilities)) add("task-invalid", `${id}: requiresCapabilities must be an array`);
        else {
          if (new Set(task.requiresCapabilities).size !== task.requiresCapabilities.length) add("task-invalid", `${id}: requiresCapabilities lists a capability twice`);
          for (const cap of task.requiresCapabilities) if (caps && !Object.hasOwn(caps, cap)) add("requires-unknown-capability", `${id} requires ${cap}, which capabilities does not define`);
        }
        if (task.applicable === false && !nonEmptyString(task.naReason)) add("task-invalid", `${id}: an N/A task needs a naReason`);
        if (task.applicable === true && task.naReason != null) add("task-invalid", `${id}: naReason must be null for an applicable task`);
        if (!Array.isArray(task.evidence)) add("task-invalid", `${id}: evidence must be an array`);
        if (typeof task.ready === "boolean" && task.ready !== derived[id]?.ready) add("ready-not-derived", `${id}: ready is ${task.ready} but derives ${derived[id]?.ready} (${derived[id]?.reasons.join("; ") || "all requirements satisfied"})`);
      }
      const expectedReady = Object.keys(entry.tasks).filter((id) => derived[id]?.ready).sort();
      const stored = [...entry.readyForAdoption].sort();
      if (new Set(entry.readyForAdoption).size !== entry.readyForAdoption.length) add("ready-list-duplicate", `${app}: readyForAdoption lists an id twice`);
      if (JSON.stringify(stored) !== JSON.stringify(expectedReady)) add("ready-list-not-derived", `${app}: readyForAdoption is [${stored.join(", ")}] but the derived ready list is [${expectedReady.join(", ")}]`);
    }
    // The AP id set must equal the frozen packet task set exactly.
    if (!packetIds) add("packets-missing", "no frozen packet task ids were supplied: the AP id set cannot be verified");
    else {
      for (const id of packetIds.duplicates ?? []) add("packet-duplicate-task", `the frozen packets define ${id} more than once`);
      const frozen = new Set(packetIds.ids);
      if (!frozen.size) add("packets-missing", "the frozen packets define no AP task ids");
      const present = new Set(manifestIds);
      for (const id of frozen) if (!present.has(id)) add("ap-missing", `${id} is in the frozen packets but not in the manifest`);
      for (const id of present) if (!frozen.has(id)) add("ap-extra", `${id} is in the manifest but not in the frozen packets`);
      for (const [app, prefix] of Object.entries(APP_PREFIX)) if (!packetIds.ids.some((id) => id.startsWith(`AP-${prefix}-`))) add("packets-missing", `the frozen packets define no ${app} task`);
    }
  }
  return out;
}

// ---- files inside an app / manifest directory ---------------------------------------------------------------------------
/** Read a relative path that must stay inside `root` (no absolute, drive, `..`, or symlink escape): { bytes } or { error }. */
export function readInside(root, relative) {
  if (typeof relative !== "string" || !relative.trim()) return { error: "path is empty" };
  const normalized = relative.replace(/\\/g, "/");
  if (normalized.startsWith("/") || /^[A-Za-z]:/.test(normalized) || normalized.split("/").includes("..")) return { error: "path must be relative and stay inside the directory" };
  const file = path.join(root, normalized);
  let real; let base;
  try { base = realpathSync(root); } catch { return { error: "directory not found" }; }
  try { real = realpathSync(file); } catch { return { error: "file not found" }; }
  if (real !== base && !real.startsWith(base + path.sep)) return { error: "path resolves outside the directory" };
  try { if (!statSync(real).isFile()) return { error: "not a file" }; } catch { return { error: "file not found" }; }
  return { bytes: readFileSync(real), file };
}

// ---- lockfiles -------------------------------------------------------------------------------------------------------------
const CARGO_REGISTRY_SOURCES = ["registry+https://github.com/rust-lang/crates.io-index", "sparse+https://index.crates.io/"];

/** Cargo.lock packages: [{ name, version, source, checksum }]. */
export function parseCargoLock(text) {
  const doc = parseToml(text);
  return (Array.isArray(doc.package) ? doc.package : []).map((p) => ({ name: p.name, version: p.version, source: p.source ?? null, checksum: p.checksum ?? null }));
}

/** pnpm-lock.yaml (lockfileVersion 9) `packages:` entries: [{ name, version, integrity, registry }]. Throws on any other lockfile version. */
export function parsePnpmLock(text) {
  const lines = String(text).split(/\r?\n/);
  const version = /^lockfileVersion:\s*['"]?([^'"\s]+)['"]?\s*$/m.exec(text)?.[1];
  if (!version || !/^9(?:\.|$)/.test(version)) throw new Error(`unsupported pnpm lockfileVersion ${JSON.stringify(version)} (only 9.x is read)`);
  const entries = [];
  let inPackages = false; let current = null;
  const flush = () => {
    if (!current) return;
    const body = current.lines.join("\n");
    const integrity = /\bintegrity:\s*(sha\d+-[A-Za-z0-9+/=]+)/.exec(body)?.[1] ?? null;
    const registry = !/\b(?:directory|repo|commit)\s*:/.test(body) && !/\btype:\s*git\b/.test(body) && !/\btarball:\s*['"]?(?:file|link):/.test(body);
    entries.push({ name: current.name, version: current.version, integrity, registry });
    current = null;
  };
  for (const line of lines) {
    if (/^\S/.test(line)) { flush(); inPackages = /^packages:\s*$/.test(line); continue; }
    if (!inPackages) continue;
    const key = /^ {2}'?((?:@[^/@'\s]+\/)?[^@'\s]+)@([^'\s:(]+)'?:\s*$/.exec(line);
    if (key) { flush(); current = { name: key[1], version: key[2], lines: [] }; continue; }
    if (current && /^ {3,}\S/.test(line)) current.lines.push(line);
    else if (current && /^ {0,2}\S/.test(line)) flush();
  }
  flush();
  return entries;
}

/** Package.resolved pins (v1 `object.pins`, v2/v3 `pins`): [{ identity, version, revision }]. */
export function parsePackageResolved(text) {
  const doc = parseJsonStrict(text, "Package.resolved");
  const pins = Array.isArray(doc?.pins) ? doc.pins : Array.isArray(doc?.object?.pins) ? doc.object.pins : [];
  return pins.map((pin) => ({ identity: String(pin.identity ?? pin.package ?? "").toLowerCase(), version: pin.state?.version ?? null, revision: pin.state?.revision ?? null }));
}

// ---- cargo tree closure ----------------------------------------------------------------------------------------------------
const TREE_FEATURE = /^[\s│├└─|`+\\-]*([A-Za-z0-9_][A-Za-z0-9_-]*)(?: v(\S+))? feature "([^"]+)"/;
const TREE_NODE = /^[\s│├└─|`+\\-]*([A-Za-z0-9_][A-Za-z0-9_-]*) v(\d+\.\d+\.\d+\S*)/;

/** Crate versions and enabled features in a saved `cargo tree -e features` text. */
export function cargoTreeFacts(text) {
  const versions = new Map(); const features = new Map();
  const note = (map, name, value) => map.set(name, new Set([...(map.get(name) ?? []), value]));
  for (const line of String(text).split(/\r?\n/)) {
    const feature = TREE_FEATURE.exec(line);
    if (feature) { note(features, feature[1], feature[3]); if (feature[2]) note(versions, feature[1], feature[2]); continue; }
    const node = TREE_NODE.exec(line);
    if (node) note(versions, node[1], node[2]);
  }
  return { versions, features };
}

const normalizeFeatures = (list) => [...new Set(list.map((f) => String(f).trim()).filter((f) => f && f !== "default" && f !== "[]"))].sort().join(",");
const featureRowSet = (text) => normalizeFeatures(String(text).split(/[,\s]+/));
const hostForTarget = (target) => HOST_FOR_TARGET.find(([pattern]) => pattern.test(target))?.[1] ?? null;

// ---- adoption --------------------------------------------------------------------------------------------------------------
const entrypointParts = (entry) => { const [file, ...symbol] = entry.split(/#|::/); return { file, symbol: symbol.join("::") }; };
const escapeRegExp = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

function lockFor(root, ecosystem, adoption, cache, out, at) {
  if (cache.has(ecosystem)) return cache.get(ecosystem);
  const kind = ecosystem === "cargo" ? "cargo" : ecosystem === "npm" ? "pnpm" : "swift";
  const candidates = adoption.locks?.[kind] ? [adoption.locks[kind]] : kind === "cargo" ? ["Cargo.lock", "src-tauri/Cargo.lock"] : kind === "pnpm" ? ["pnpm-lock.yaml"] : ["Package.resolved"];
  let result = null;
  for (const candidate of candidates) {
    const read = readInside(root, candidate);
    if (read.error) continue;
    try {
      const text = read.bytes.toString("utf8");
      result = { kind, file: candidate, sha256: sha256Of(read.bytes), entries: kind === "cargo" ? parseCargoLock(text) : kind === "pnpm" ? parsePnpmLock(text) : parsePackageResolved(text) };
    } catch (error) { out.push(finding("lockfile-unsupported", `${at}: ${candidate}: ${error.message}`)); result = false; }
    break;
  }
  if (result === null) out.push(finding("lockfile-missing", `${at}: no ${kind} lockfile found (tried ${candidates.join(", ")})`));
  cache.set(ecosystem, result);
  return result;
}

function checkLockPin(lock, p, at, out) {
  const rows = lock.kind === "cargo" ? lock.entries.filter((e) => e.name === p.name)
    : lock.kind === "pnpm" ? lock.entries.filter((e) => e.name === p.name)
      : lock.entries.filter((e) => e.identity === p.name.toLowerCase());
  if (!rows.length) { out.push(finding("lock-missing", `${at}: ${p.id} is not in ${lock.file}`)); return; }
  const versions = [...new Set(rows.map((r) => r.version))];
  if (versions.length !== 1 || versions[0] !== p.version) out.push(finding("lock-version-mismatch", `${at}: ${lock.file} resolves ${p.id} to [${versions.join(", ")}], the manifest pins exactly ${p.version}`));
  for (const row of rows) {
    if (row.version !== p.version) continue;
    if (lock.kind === "cargo") {
      if (!CARGO_REGISTRY_SOURCES.includes(row.source)) out.push(finding("lock-not-registry", `${at}: ${p.id} in ${lock.file} has source ${JSON.stringify(row.source)}, not the crates.io registry`));
      else if (row.checksum !== p.integrity) out.push(finding("lock-integrity-mismatch", `${at}: ${p.id} checksum in ${lock.file} is ${row.checksum}, the manifest integrity is ${p.integrity}`));
    } else if (lock.kind === "pnpm") {
      if (!row.registry) out.push(finding("lock-not-registry", `${at}: ${p.id} in ${lock.file} resolves from a directory, git or file source`));
      else if (row.integrity !== p.integrity) out.push(finding("lock-integrity-mismatch", `${at}: ${p.id} integrity in ${lock.file} is ${row.integrity}, the manifest integrity is ${p.integrity}`));
    } else if (row.revision !== p.integrity) out.push(finding("lock-integrity-mismatch", `${at}: ${p.id} revision in ${lock.file} is ${row.revision}, the manifest tag commit is ${p.integrity}`));
  }
}

// `declared` is the feature list the adoption record says the app requests for this crate. The cargo tree evidence must show every
// declared feature active, the declared set must equal a passing qualification row for the host (a tree lists implied features
// too, so it cannot be compared to a row directly), and no active feature may be outside the published feature list.
function checkClosure(p, capability, declared, adoption, tree, at, out) {
  const host = hostForTarget(adoption.target);
  if (!p.targets.includes(adoption.target)) out.push(finding("target-not-published", `${at}: ${p.id}@${p.version} does not list the active target ${adoption.target} (published: ${p.targets.join(", ")})`));
  const rows = (p.qualification ?? []).filter((r) => r.result === "pass" && r.host === host);
  if (!rows.length) out.push(finding("target-not-qualified", `${at}: ${p.id}@${p.version} has no passing qualification row for host ${host ?? adoption.target}`));
  if (p.ecosystem !== "cargo") return;
  const versions = tree.versions.get(p.name);
  if (!versions?.size) { out.push(finding("closure-missing-crate", `${at}: ${p.name} is not in the cargo tree evidence, so its active features cannot be read`)); return; }
  if (versions.size !== 1 || !versions.has(p.version)) out.push(finding("closure-version-mismatch", `${at}: the cargo tree evidence has ${p.name} at [${[...versions].join(", ")}], the manifest pins ${p.version}`));
  const active = new Set(tree.features.get(p.name) ?? []);
  const published = new Set([...p.features, "default"]);
  for (const feature of active) if (!published.has(feature)) out.push(finding("feature-unpublished", `${at}: active feature ${feature} of ${p.name} is not in the published feature list [${p.features.join(", ")}]`));
  for (const feature of declared) {
    if (!published.has(feature)) out.push(finding("feature-unpublished", `${at}: declared feature ${feature} of ${p.name} is not in the published feature list [${p.features.join(", ")}]`));
    if (!active.has(feature)) out.push(finding("feature-not-active", `${at}: declared feature ${feature} of ${p.name} is not active in the cargo tree evidence`));
  }
  for (const feature of Array.isArray(capability.features) ? capability.features : []) if (!declared.includes(feature) || !active.has(feature)) out.push(finding("feature-required-missing", `${at}: ${capability.id ?? "capability"} requires feature ${feature} of ${p.name}, which is not both declared and active`));
  const declaredSet = normalizeFeatures(declared);
  if (rows.length && !rows.some((r) => featureRowSet(r.features) === declaredSet)) out.push(finding("feature-set-unqualified", `${at}: declared features of ${p.name} are [${declaredSet || "default only"}], which matches no passing qualification row (${rows.map((r) => `"${r.features}"`).join(", ")})`));
}

function checkReceipt({ root, adoption, entry, packages, locks, at, out }) {
  const ref = entry.receipt;
  if (!isObject(ref) || !nonEmptyString(ref.file) || !isHex(ref.sha256, 64)) { out.push(finding("receipt-missing", `${at}: an exercised-entrypoint receipt { file, sha256 } is required for ${entry.capability}`)); return; }
  const read = readInside(root, ref.file);
  if (read.error) { out.push(finding("receipt-unreadable", `${at}: receipt ${ref.file}: ${read.error}`)); return; }
  if (sha256Of(read.bytes) !== ref.sha256) { out.push(finding("receipt-hash-mismatch", `${at}: receipt ${ref.file} has sha256 ${sha256Of(read.bytes)}, the adoption record says ${ref.sha256}`)); return; }
  let receipt;
  try { receipt = parseJsonStrict(read.bytes.toString("utf8"), ref.file); } catch (error) { out.push(finding("receipt-invalid", `${at}: receipt ${ref.file} is not machine-readable JSON (${error.message}); prose never satisfies a receipt`)); return; }
  const bad = (reason) => out.push(finding("receipt-invalid", `${at}: receipt ${ref.file}: ${reason}`));
  if (!isObject(receipt) || receipt.schema !== 1) { bad("schema must be 1"); return; }
  if (!["test", "run"].includes(receipt.kind)) bad("kind must be test or run");
  if (receipt.app !== adoption.app) bad(`app is ${JSON.stringify(receipt.app)}, expected ${JSON.stringify(adoption.app)}`);
  if (receipt.capability !== entry.capability) bad(`capability is ${JSON.stringify(receipt.capability)}, expected ${entry.capability}`);
  if (receipt.target !== adoption.target) bad(`target is ${JSON.stringify(receipt.target)}, expected ${adoption.target}`);
  if (!Array.isArray(entry.entrypoints) || !entry.entrypoints.includes(receipt.entrypoint)) bad(`entrypoint ${JSON.stringify(receipt.entrypoint)} is not one of the adopted entrypoints`);
  if (!Array.isArray(receipt.command) || !receipt.command.length || receipt.command.some((c) => !nonEmptyString(c))) bad("command must be a non-empty array of strings");
  if (receipt.exitCode !== 0) bad(`exitCode must be 0, got ${JSON.stringify(receipt.exitCode)}`);
  if (!nonEmptyString(receipt.finishedAt) || Number.isNaN(Date.parse(receipt.finishedAt))) bad("finishedAt must be an ISO date");
  const listed = Array.isArray(receipt.packages) ? receipt.packages : [];
  for (const p of packages) if (!listed.includes(`${p.id}@${p.version}`)) bad(`it does not name ${p.id}@${p.version}, so it does not prove the pinned version was exercised`);
  const hashes = isObject(receipt.lockfileSha256) ? receipt.lockfileSha256 : {};
  for (const [kind, lock] of locks) if (hashes[kind] !== lock.sha256) bad(`lockfileSha256.${kind} is ${hashes[kind] ?? "absent"}, the lockfile ${lock.file} is ${lock.sha256}: the receipt predates the current lock`);
}

function checkEntrypoints(root, entrypoints, at, out) {
  if (!Array.isArray(entrypoints) || !entrypoints.length) { out.push(finding("entrypoint-missing", `${at}: at least one real entrypoint is required`)); return; }
  for (const entry of entrypoints) {
    if (!nonEmptyString(entry)) { out.push(finding("entrypoint-invalid", `${at}: an entrypoint must be a non-empty string`)); continue; }
    const { file, symbol } = entrypointParts(entry);
    const read = readInside(root, file);
    if (read.error) { out.push(finding("entrypoint-not-a-file", `${at}: entrypoint ${entry}: ${read.error} (an entrypoint is <relative file>[#symbol])`)); continue; }
    if (symbol && !new RegExp(`(?<![A-Za-z0-9_])${escapeRegExp(symbol)}(?![A-Za-z0-9_])`).test(read.bytes.toString("utf8"))) out.push(finding("entrypoint-symbol-missing", `${at}: entrypoint ${entry}: ${file} does not contain ${symbol}`));
  }
}

/**
 * Check one app's adoption record against a manifest that already validated. Returns { findings, tasks } where each task lists
 * its adopted capabilities and findings. Never mutates anything; reads only files inside `appRoot`.
 */
export function checkAdoption({ manifest, manifestSha256, appRoot, adoption, target }) {
  const findings = [];
  const tasks = {};
  const add = (rule, reason) => findings.push(finding(rule, reason));
  if (!isObject(adoption) || adoption.schema !== 1) { add("adoption-invalid", "adoption record must be an object with schema 1"); return { findings, tasks }; }
  if (!Object.hasOwn(APP_PREFIX, adoption.app)) add("adoption-invalid", `app must be one of ${Object.keys(APP_PREFIX).join(", ")}, got ${JSON.stringify(adoption.app)}`);
  if (adoption.manifestSha256 !== manifestSha256) add("adoption-stale", `the adoption record was made against manifest ${adoption.manifestSha256 ?? "(none)"}, the manifest is ${manifestSha256}`);
  if (target) adoption = { ...adoption, target };
  if (!nonEmptyString(adoption.target)) { add("adoption-invalid", "target is required (adoption record or --target)"); return { findings, tasks }; }
  if (!isObject(adoption.tasks) || !Object.keys(adoption.tasks).length) { add("adoption-invalid", "tasks must list at least one adopted task"); return { findings, tasks }; }

  const readiness = deriveReadiness(manifest);
  const caps = manifest.capabilities ?? {};
  const packagesByKey = new Map((manifest.packages ?? []).map((p) => [packageKey(p), p]));
  const packagesById = new Map();
  for (const p of manifest.packages ?? []) packagesById.set(p.id, [...(packagesById.get(p.id) ?? []), p]);
  const locks = new Map();
  let tree = null; let treeChecked = false;
  const loadTree = (out, at) => {
    if (treeChecked) return tree;
    treeChecked = true;
    const ref = adoption.cargoTree;
    if (!isObject(ref) || !nonEmptyString(ref.file) || !isHex(ref.sha256, 64)) { out.push(finding("closure-evidence-missing", `${at}: a cargoTree { file, sha256 } (a saved \`cargo tree -e features\` for ${adoption.target}) is required with any cargo package`)); return null; }
    const read = readInside(appRoot, ref.file);
    if (read.error) { out.push(finding("closure-evidence-unreadable", `${at}: cargoTree ${ref.file}: ${read.error}`)); return null; }
    if (sha256Of(read.bytes) !== ref.sha256) { out.push(finding("closure-evidence-hash-mismatch", `${at}: cargoTree ${ref.file} has sha256 ${sha256Of(read.bytes)}, the adoption record says ${ref.sha256}`)); return null; }
    tree = cargoTreeFacts(read.bytes.toString("utf8"));
    return tree;
  };

  for (const [taskId, claim] of Object.entries(adoption.tasks)) {
    const out = [];
    const at = taskId;
    const report = { ready: false, adopted: [], findings: out };
    tasks[taskId] = report;
    const state = readiness[taskId];
    const task = Object.values(manifest.apps ?? {}).map((a) => a?.tasks?.[taskId]).find(Boolean);
    if (!state || !task) { out.push(finding("task-unknown", `${at}: not a task in the manifest`)); continue; }
    if (state.app !== adoption.app) out.push(finding("task-wrong-app", `${at}: belongs to ${state.app}, the adoption record is for ${adoption.app}`));
    report.ready = state.ready;
    if (task.applicable !== true) out.push(finding("task-not-applicable", `${at}: is N/A in the manifest (${task.naReason ?? "no reason"}); an N/A task is never ready and is never adopted`));
    else if (!state.ready) out.push(finding("task-not-ready", `${at}: is not ready in the manifest (${state.reasons.join("; ")})`));
    const adopted = Array.isArray(claim?.adopted) ? claim.adopted : null;
    if (!adopted) { out.push(finding("adoption-invalid", `${at}: adopted must be an array`)); continue; }
    const adoptedIds = adopted.map((a) => a?.capability);
    if (task.applicable === true) for (const required of Array.isArray(task.requiresCapabilities) ? task.requiresCapabilities : []) if (!adoptedIds.includes(required)) out.push(finding("required-capability-not-adopted", `${at}: requires ${required}, which the adoption record does not adopt`));
    for (const entry of adopted) {
      const where = `${at} ${entry?.capability}`;
      const capability = isObject(entry) ? caps[entry.capability] : null;
      if (!capability) { out.push(finding("capability-unknown", `${where}: not a capability in the manifest`)); continue; }
      if (capability.status !== "satisfied") { out.push(finding("capability-not-satisfied", `${where}: is ${capability.status} in the manifest, so it cannot be adopted`)); continue; }
      const refs = Array.isArray(entry.packages) ? entry.packages : [];
      if (!refs.length) out.push(finding("adoption-invalid", `${where}: packages must list at least one package id from the capability`));
      const resolved = [];
      const lockUse = new Map();
      for (const raw of refs) {
        // A package is named by id (`cargo:rightkit-credentials`) or by { id, features: [...] } for the features the app requests.
        const ref = isObject(raw) ? raw.id : raw;
        const declared = isObject(raw) && Array.isArray(raw.features) ? raw.features.filter((f) => typeof f === "string") : [];
        const candidates = (capability.packageIds ?? []).map((id) => packagesByKey.get(id) ?? (packagesById.get(id)?.length === 1 ? packagesById.get(id)[0] : null)).filter(Boolean);
        const p = candidates.find((c) => c.id === ref || packageKey(c) === ref);
        if (!p) { out.push(finding("package-not-in-capability", `${where}: ${JSON.stringify(ref)} is not a package of this capability (${(capability.packageIds ?? []).join(", ")})`)); continue; }
        resolved.push(p);
        const lock = lockFor(appRoot, p.ecosystem, adoption, locks, out, where);
        if (lock) { checkLockPin(lock, p, where, out); lockUse.set(lock.kind, lock); }
        const closure = p.ecosystem === "cargo" ? loadTree(out, where) : { versions: new Map(), features: new Map() };
        if (closure) checkClosure(p, { ...capability, id: entry.capability }, p.ecosystem === "cargo" ? declared : [], adoption, closure, where, out);
      }
      checkEntrypoints(appRoot, entry.entrypoints, where, out);
      checkReceipt({ root: appRoot, adoption, entry, packages: resolved, locks: lockUse, at: where, out });
      report.adopted.push({ capability: entry.capability, packages: resolved.map((p) => `${p.id}@${p.version}`), receipt: entry.receipt?.file ?? null });
    }
  }
  for (const report of Object.values(tasks)) findings.push(...report.findings);
  return { findings, tasks };
}

// ---- command ---------------------------------------------------------------------------------------------------------------
const flag = (args, name) => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; };

export function renderText(report) {
  const out = [`adoption-evidence — manifest ${report.manifest.sha256}${report.app ? `, app ${report.app}` : ", manifest only"}`];
  for (const f of report.findings) out.push(`  ${f.rule.padEnd(30)} ${f.reason}`);
  for (const [id, task] of Object.entries(report.tasks ?? {})) out.push(`  ${id}: ${task.findings.length ? `${task.findings.length} finding(s)` : "ok"}${task.adopted.length ? ` (${task.adopted.map((a) => a.capability).join(", ")})` : ""}`);
  out.push(report.ok ? "ok: no findings" : `FAILED: ${report.findings.length} finding(s)`);
  return `${out.join("\n")}\n`;
}

export async function main(args, io = {}) {
  const stdout = io.stdout ?? process.stdout; const stderr = io.stderr ?? process.stderr;
  const usage = (message) => { stderr.write(`[right-release] adoption-evidence: ${message}\n`); return 2; };
  const manifestPath = flag(args, "--manifest");
  if (!manifestPath) return usage("--manifest <published-release-manifest.json> is required");
  let loaded;
  try { loaded = loadManifest(path.resolve(manifestPath)); } catch (error) { return usage(`cannot read the manifest: ${error.message}`); }
  const baseDir = path.dirname(path.resolve(manifestPath));
  const packetsDir = path.resolve(flag(args, "--packets") ?? path.join(baseDir, ".."));
  let packetIds = null;
  try { const extracted = extractPacketTaskIds(packetsDir); if (extracted.files.length) packetIds = extracted; } catch { packetIds = null; }
  const findings = validateManifest(loaded.manifest, { packetIds, baseDir });
  const report = { schema: 1, ok: false, manifest: { path: path.resolve(manifestPath), sha256: loaded.sha256 }, app: null, tasks: {}, findings };
  const appArg = flag(args, "--app");
  if (appArg) {
    const appRoot = path.resolve(appArg);
    if (!existsSync(appRoot) || !statSync(appRoot).isDirectory()) return usage(`--app ${appArg} is not a directory`);
    const adoptionPath = path.resolve(flag(args, "--adoption") ?? path.join(appRoot, "rightkit-adoption.json"));
    let adoption;
    try { adoption = parseJsonStrict(readFileSync(adoptionPath, "utf8"), path.basename(adoptionPath)); } catch (error) { return usage(`cannot read the adoption record: ${error.message}`); }
    report.app = adoption?.app ?? null;
    // A manifest that fails its own checks is not a basis for adoption: report its findings and stop.
    if (!findings.length) {
      const checked = checkAdoption({ manifest: loaded.manifest, manifestSha256: loaded.sha256, appRoot, adoption, target: flag(args, "--target") });
      report.tasks = checked.tasks;
      findings.push(...checked.findings);
    }
  }
  report.ok = findings.length === 0;
  stdout.write(args.includes("--json") ? `${JSON.stringify(report, null, 2)}\n` : renderText(report));
  return report.ok ? 0 : 1;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try { process.exitCode = await main(process.argv.slice(2)); }
  catch (error) { console.error(`[right-release] adoption-evidence: ${error.message}`); process.exitCode = 1; }
}
