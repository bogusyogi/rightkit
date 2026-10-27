#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { enforcePublicationGate, defaultEvidencePath, provenGateSteps, singleHostOfPolicy, batchClippyMissingFeatureSets } from "./publication-gate.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { assertPublishCapability, refuseHostUpload } from "./host-upload-refusal.mjs";
import { spawnSync } from "./background-process.mjs";
import { readVerifiedArtifact, assertArtifactUnchanged } from "./publication-artifacts.mjs";
import { verifyCargoRegistry } from "./cargo-registry-readback.mjs";
import { recordPublished, workspaceFeedOptions } from "./rightkit-sync.mjs";
import { readCargoPackage } from "./crate-graph.mjs";
import { runtimeBudgetCheck, RUNTIME_BUDGET_CODE } from "./runtime-budgets.mjs";
import { formatPreflight, PREFLIGHT_FAIL } from "./preflight.mjs";
import { cargoManifestState } from "./cargo-manifest-text.mjs";

const FORBIDDEN_PACKAGE_PATH = /(^|\/)(?:\.env(?:\.[^/]*)?|target(?:\/.*)?|node_modules(?:\/.*)?|[^/]+\.(?:p8|p12|pem|key))$/i;
export const SECRET_PATTERNS = [
  /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/,
  /\bAKIA[0-9A-Z]{16}\b/,
  /\bgh[pousr]_[A-Za-z0-9_]{20,}\b/,
  /\bnpm_[A-Za-z0-9]{30,}\b/,
];

export function parseCargoPublishArgs(argv, cwd = process.cwd()) {
  const crate = valueAfter(argv, "--crate");
  if (!crate) throw new Error("publish cargo requires --crate <name>");
  if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(crate)) throw new Error(`invalid crate name: ${crate}`);
  const allowed = new Set(["--crate", "--workspace", "--dry-run", "--allow-dirty", "--evidence", "--from-evidence"]);
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) continue;
    if (!allowed.has(arg)) throw new Error(`unknown publish cargo option: ${arg}`);
    if (arg !== "--dry-run" && arg !== "--allow-dirty") index += 1;
  }
  const dryRun = argv.includes("--dry-run");
  const allowDirty = argv.includes("--allow-dirty");
  if (allowDirty && !dryRun) throw new Error("--allow-dirty is only valid with --dry-run");
  const fromEvidence = valueAfter(argv, "--from-evidence");
  if (argv.includes("--from-evidence") && (!fromEvidence || fromEvidence.startsWith("--"))) throw new Error("--from-evidence requires a gate evidence file");
  if (fromEvidence && (dryRun || allowDirty || argv.includes("--evidence"))) throw new Error("--from-evidence cannot be combined with --dry-run, --allow-dirty or --evidence");
  return {
    crate,
    dryRun,
    allowDirty,
    workspaceRoot: path.resolve(valueAfter(argv, "--workspace") ?? cwd),
    evidence: fromEvidence ?? valueAfter(argv, "--evidence"),
    ...(fromEvidence ? { fromEvidence: true } : {}),
  };
}

const OFFLINE_CACHE_MISS = /(?:attempting to make an HTTP request, but --offline was specified|as a reminder, you're using offline mode(?: \(--offline\))?)/i;
const OFFLINE_READ_ONLY_COMMANDS = new Set(["metadata", "package"]);

/** Retry only a Cargo offline cache miss for metadata/package inspection, once with normal network access. */
export async function runCargoReadOnly(args, settings, execute, onOfflineFailure = () => {}) {
  const subcommand = args[0];
  if (!OFFLINE_READ_ONLY_COMMANDS.has(subcommand) || !args.includes("--offline")) {
    return execute("cargo", args, settings);
  }
  const first = await execute("cargo", args, settings);
  if (!first.error && first.status === 0) return first;
  const diagnostic = `${first.stdout ?? ""}\n${first.stderr ?? ""}`;
  if (!OFFLINE_CACHE_MISS.test(diagnostic)) return first;
  onOfflineFailure(first);
  const retryArgs = args.filter((arg) => arg !== "--offline");
  const retry = await execute("cargo", retryArgs, settings);
  if (!retry.error && retry.status === 0) return { ...retry, usedOfflineFallback: true };
  const labeled = (result) => String(result ?? "");
  const stdout = `[offline attempt]\n${labeled(first.stdout)}\n[online retry]\n${labeled(retry.stdout)}`;
  const stderr = `[offline attempt]\n${labeled(first.stderr)}\n[online retry]\n${labeled(retry.stderr)}`;
  return {
    ...retry,
    stdout,
    stderr,
    ...(retry.error ? { error: new Error(`${retry.error.message}\n${stdout}\n${stderr}`) } : {}),
    offlineFallbackFailed: true,
  };
}

// A committed ci/runtime-budget-deferred.json names crates whose runtime-budget refusal is deferred by an
// explicit decision (who, when, why). The refusal is still printed; a listed crate publishes with the
// deferral logged. Returns the logged reason, or null when the crate is not listed.
export function runtimeBudgetDeferral(crateDir, crate) {
  const file = path.join(crateDir, "..", "..", "ci", "runtime-budget-deferred.json");
  let data;
  try { data = JSON.parse(fs.readFileSync(file, "utf8")); } catch { return null; }
  if (!data?.crates?.includes?.(crate) || !data.approvedBy || !data.approved || !data.reason) return null;
  return `${data.reason} (${data.approvedBy}, ${data.approved}; ci/runtime-budget-deferred.json)`;
}

// Plan step label -> the `provenGateSteps` key whose proof lets that step be skipped.
const PROVEN_STEP = Object.freeze({ "secret scan": "secretScan", format: "fmt", tests: "tests", clippy: "clippy" });

/**
 * `proven` (from `provenGateSteps(evidence)`) removes a secret scan, format, tests or clippy step the evidence
 * already proves for this exact commit; the runtime budget, package contents, archive hash and registry steps
 * are never skippable. `verifyOnPublish` (real publish only) drops the separate registry dry-run and lets the
 * real `cargo publish` run Cargo's own verify build (no `--no-verify`), so the package compiles once, before
 * the upload, instead of in a dry-run and not at all in the publish. `packageVerified` is set only after
 * explicit evidence reuse proves a successful standalone package verification; hash checking stays mandatory.
 */
export function buildCargoPublishPlan({ crate, crateDir, dryRun, allowDirty = false, proven = {}, verifyOnPublish = false, packageVerified = false, offlineReadOnly = false }) {
  const offline = offlineReadOnly ? ["--offline"] : [];
  const dryRunArgs = ["publish", "--dry-run", "-p", crate];
  const packageArgs = ["package", "--list", ...offline, "-p", crate];
  if (allowDirty) dryRunArgs.push("--allow-dirty");
  if (allowDirty) packageArgs.push("--allow-dirty");
  const verifyHere = verifyOnPublish && !dryRun;
  const plan = [
    { label: "secret scan", internal: "secret-scan", crateDir },
    // Runtime-budget crates (runtime-budgets.mjs) must carry a measured runtime-budget.json for this
    // version on every required host. Dry-run reports the check; a real publish refuses it.
    { label: "runtime budget", internal: "runtime-budget", crate, crateDir },
    { label: "format", command: "cargo", args: ["fmt", "-p", crate, "--check"] },
    { label: "tests", command: "cargo", args: ["test", "-p", crate, "--", "--test-threads=1"] },
    { label: "clippy", command: "cargo", args: ["clippy", "-p", crate, "--all-targets", "--", "-D", "warnings"] },
    { label: "package contents", command: "cargo", args: packageArgs, capture: true },
    ...((verifyHere || packageVerified) ? [] : [{ label: "registry dry-run", command: "cargo", args: dryRunArgs }]),
  ].filter((step) => !(PROVEN_STEP[step.label] && proven[PROVEN_STEP[step.label]] === true));
  if (!dryRun) plan.push(
    // `publish --dry-run` leaves its archive under target/package/tmp-crate on current Cargo; package
    // explicitly to bind target/package/<name>-<version>.crate to evidence before Cargo uploads. Always
    // online and --locked, like qualification: offline packaging resolves the archive's Cargo.lock from
    // only the .crate files already downloaded, so a sibling published since then silently resolves to an
    // older version and the archive no longer matches its evidence hash.
    { label: "package archive", command: "cargo", args: ["package", "--locked", "--no-verify", "-p", crate, ...(allowDirty ? ["--allow-dirty"] : [])] },
    { label: "package location", command: "cargo", args: ["metadata", ...offline, "--format-version", "1", "--no-deps"], capture: true },
    { label: "archive hash check", internal: "verify-archive" },
    { label: "registry publish", command: "cargo", args: verifyHere ? ["publish", "--locked", "-p", crate] : ["publish", ...(packageVerified ? ["--locked"] : []), "--no-verify", "-p", crate] },
    { label: "registry readback", internal: "registry-readback" },
  );
  return plan;
}

export function packageFileViolations(files) {
  return files
    .map((file) => file.trim().replaceAll("\\", "/"))
    .filter(Boolean)
    .filter((file) => FORBIDDEN_PACKAGE_PATH.test(file));
}

// ---- the secret scan ---------------------------------------------------------------------------------------------------
//
// Strict (the default): a crate package, its archive and the local-publish batch scan. `scanCrateForSecrets(dir)` returns the
// relative paths that are refused by name (FORBIDDEN_PACKAGE_PATH, which includes every .env* file, so env templates too) or whose
// content matches SECRET_PATTERNS. Nothing published needs a template or a test vector, and the records file below is never read.
//
// Repository gate (`{ envTemplates: true }`, deterministic-scans.mjs "secret-scan"): the units of a commit that is not being
// published. An app legitimately commits env templates (`.env.example`) and test vectors for its own redaction and screening code,
// and no content heuristic tells a placeholder from a pasted credential, so a file is admitted by a reviewed record, not by a guess.
// `.rightkit/secret-scan-records.json` in the repository:
//   { "schema": 1, "records": [ { "path": "<repo-relative>", "sha256": "<hex of the bytes, CRLF read as LF>", "kind": "env-template" | "fixture", "note": "<what it holds and why it is safe>" } ] }
//  - an env template (.env.example, .env.<name>.example, .env.sample, .env.template) is admitted only by a record of kind
//    "env-template" whose sha256 is the file's: any edit needs a new record, which is the review, in the pull request diff;
//  - any other file whose content matches SECRET_PATTERNS is admitted only by a record of kind "fixture" with the file's sha256;
//  - never recordable: .env and every .env.* that is not a template, *.pem, *.p8, *.p12, *.key (refused by name, and a record naming
//    one is itself a violation); a template that is not UTF-8 (NUL bytes, a UTF-16 mark), that holds a PEM private key header or one
//    of the other high-precision credential shapes below, or that sits inside a publishable crate or package is refused even when recorded;
//  - a malformed records file is a violation and admits nothing.
// Each violation is `<path>[:<line>]: <reason>`; a hit names the line and the shape, never the matched text.
export const SECRET_SCAN_RECORDS_FILE = ".rightkit/secret-scan-records.json";
const RECORD_KINDS = Object.freeze(["env-template", "fixture"]);
const RECORD_KEYS = Object.freeze(["path", "sha256", "kind", "note"]);
const RECORD_NOTE_PLACEHOLDER = "<what it holds and why it is safe>";
const ENV_TEMPLATE = /(^|\/)\.env(?:\.[^/]*)?\.(?:example|sample|template)$/i;
// Labels of SECRET_PATTERNS, index for index.
const SECRET_PATTERN_LABELS = Object.freeze(["a PEM private key header", "an AWS access key id (AKIA)", "a GitHub token (gh*_)", "an npm token (npm_)"]);
// Credential shapes that are never a placeholder, matched in env templates besides SECRET_PATTERNS. Every expression is linear:
// each quantified class is followed by a delimiter it cannot match or by the end of the match.
const TEMPLATE_SHAPES = Object.freeze([
  [/-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----/, "a PEM private key header"],
  [/github_pat_[A-Za-z0-9_]{22,}/, "a GitHub fine-grained token (github_pat_)"],
  [/sk_live_[A-Za-z0-9]{16,}/, "a Stripe live key (sk_live_)"],
  [/xox[abprs]-[A-Za-z0-9-]{10,}/, "a Slack token (xox*-)"],
  [/AIza[0-9A-Za-z_-]{35}/, "a Google API key (AIza)"],
  [/hooks\.slack\.com\/services\/T[A-Z0-9]+\/B[A-Z0-9]+\/[A-Za-z0-9]+/, "a Slack webhook URL"],
]);
const FILE_SHAPES = Object.freeze(SECRET_PATTERNS.map((pattern, index) => [pattern, SECRET_PATTERN_LABELS[index]]));
const TEMPLATE_ALL_SHAPES = Object.freeze([...FILE_SHAPES, ...TEMPLATE_SHAPES]);
const UTF8 = new TextDecoder("utf-8", { fatal: true });

const neverRecordable = (relative) => FORBIDDEN_PACKAGE_PATH.test(relative) && !ENV_TEMPLATE.test(relative);
const sha256Hex = (bytes) => createHash("sha256").update(bytes).digest("hex");
// A record's digest reads CRLF as LF, so one record holds on every host: `git archive` (the gate's export) writes CRLF on a
// Windows host with `core.autocrlf=true` or `core.eol=crlf` for `text=auto` files and LF elsewhere. The content checks still read
// the bytes as exported; a lone CR is kept, so no other byte is ever ignored.
const recordDigest = (bytes) => sha256Hex(Buffer.from(Buffer.from(bytes).toString("latin1").replace(/\r\n/g, "\n"), "latin1"));
const recordLine = (relative, sha256, kind) => JSON.stringify({ path: relative, sha256, kind, note: RECORD_NOTE_PLACEHOLDER });
const repoRelativePath = (p) => p !== "" && p !== "." && !p.includes("\\") && !p.includes("\0") && !p.startsWith("/") && !/^[A-Za-z]:/.test(p) && !p.endsWith("/")
  && path.posix.normalize(p) === p && !p.split("/").includes("..");

/** `{ records: Map<path, { sha256, kind, note }>, problems: string[] }` from the text of the records file (undefined: no file). A record with a problem admits nothing. */
export function parseSecretScanRecords(text) {
  const records = new Map();
  const problems = [];
  const bad = (message) => problems.push(`${SECRET_SCAN_RECORDS_FILE}: ${message}`);
  if (text === undefined) return { records, problems };
  let doc;
  try { doc = JSON.parse(String(text).replace(/^﻿/, "")); } catch (error) { bad(`is not valid JSON (${error.message})`); return { records, problems }; }
  if (doc === null || typeof doc !== "object" || Array.isArray(doc)) { bad('must be an object { "schema": 1, "records": [...] }'); return { records, problems }; }
  for (const key of Object.keys(doc)) if (key !== "schema" && key !== "records") bad(`has an unknown key ${JSON.stringify(key)}`);
  if (doc.schema !== 1) { bad(`schema must be 1 (found ${JSON.stringify(doc.schema)}); no record is read`); return { records, problems }; }
  if (!Array.isArray(doc.records)) { bad('"records" must be an array'); return { records, problems }; }
  const counts = new Map();
  const valid = [];
  doc.records.forEach((record, index) => {
    if (record === null || typeof record !== "object" || Array.isArray(record)) { bad(`record ${index} must be an object`); return; }
    const label = typeof record.path === "string" ? `record ${index} (${JSON.stringify(record.path)})` : `record ${index}`;
    let ok = true;
    const fail = (message) => { bad(`${label}: ${message}`); ok = false; };
    for (const key of Object.keys(record)) if (!RECORD_KEYS.includes(key)) fail(`has an unknown key ${JSON.stringify(key)}`);
    if (typeof record.path !== "string" || !repoRelativePath(record.path)) fail('"path" must be a normalised repository-relative path with forward slashes');
    if (typeof record.sha256 !== "string" || !/^[0-9a-f]{64}$/i.test(record.sha256)) fail('"sha256" must be 64 hex characters, the sha256 of the file bytes with CRLF read as LF');
    if (!RECORD_KINDS.includes(record.kind)) fail(`"kind" must be one of ${RECORD_KINDS.join(", ")}`);
    if (typeof record.note !== "string" || record.note.trim() === "" || record.note.trim() === RECORD_NOTE_PLACEHOLDER) fail('"note" must say what the file holds and why it is safe');
    if (typeof record.path === "string") counts.set(record.path, (counts.get(record.path) ?? 0) + 1);
    if (!ok) return;
    if (neverRecordable(record.path)) fail("names a path that can never be recorded (.env and every .env.* that is not a template, *.pem, *.p8, *.p12, *.key)");
    else if (record.kind === "env-template" && !ENV_TEMPLATE.test(record.path)) fail('kind "env-template" names a path that is not an env template (.env.example, .env.<name>.example, .env.sample, .env.template)');
    else if (record.kind === "fixture" && ENV_TEMPLATE.test(record.path)) fail('kind "fixture" cannot admit an env template; record it as "env-template"');
    else valid.push(record);
  });
  for (const [recorded, count] of counts) if (count > 1) bad(`${JSON.stringify(recorded)} is recorded ${count} times; keep one record per path`);
  for (const record of valid) if (counts.get(record.path) === 1) records.set(record.path, { sha256: record.sha256.toLowerCase(), kind: record.kind, note: record.note });
  return { records, problems };
}

// The records file of the repository being scanned, read as data: a regular file whose real path stays inside the repository.
function readSecretScanRecords(repoRoot) {
  const file = path.join(repoRoot, SECRET_SCAN_RECORDS_FILE);
  try {
    if (!fs.lstatSync(file).isFile()) return { problems: [`${SECRET_SCAN_RECORDS_FILE}: is not a regular file`] };
    const relative = path.relative(fs.realpathSync(repoRoot), fs.realpathSync(file));
    if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) return { problems: [`${SECRET_SCAN_RECORDS_FILE}: resolves outside the repository`] };
    return parseSecretScanRecords(fs.readFileSync(file, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT" || error?.code === "ENOTDIR") return parseSecretScanRecords(undefined);
    return { problems: [`${SECRET_SCAN_RECORDS_FILE}: cannot be read (${error?.code ?? error?.message})`] };
  }
}

/** Why a file is not UTF-8 text (NUL bytes, a UTF-16 byte-order mark, an invalid sequence), or null. */
function notUtf8(bytes) {
  if (bytes.length >= 2 && ((bytes[0] === 0xff && bytes[1] === 0xfe) || (bytes[0] === 0xfe && bytes[1] === 0xff))) return "starts with a UTF-16 byte-order mark";
  if (bytes.includes(0)) return "contains NUL bytes";
  try { UTF8.decode(bytes); } catch { return "is not valid UTF-8"; }
  return null;
}

/**
 * `[{ label, lines }]` for the shapes that match `text`: at most the first five matching lines of each, found in one pass.
 * After a hit the search resumes on the next line, so the work is linear in the text however many matches a line holds.
 */
function contentHits(text, shapes) {
  const hits = new Map();
  for (const [shape, label] of shapes) {
    const pattern = new RegExp(shape.source, `${shape.flags.replace("g", "")}g`);
    const lines = hits.get(label) ?? [];
    let line = 1;
    let newline = text.indexOf("\n");
    for (let match = pattern.exec(text); match !== null && lines.length < 5; match = pattern.exec(text)) {
      while (newline !== -1 && newline < match.index) { line += 1; newline = text.indexOf("\n", newline + 1); }
      if (!lines.includes(line)) lines.push(line);
      pattern.lastIndex = newline === -1 ? text.length : newline + 1;
    }
    if (lines.length) hits.set(label, lines);
  }
  const found = [...hits].map(([label, lines]) => ({ label, lines: [...lines].sort((a, b) => a - b).slice(0, 5) }));
  return found.sort((a, b) => a.lines[0] - b.lines[0] || (a.label < b.label ? -1 : 1));
}

const hitsText = (hits) => hits.map((hit) => `${hit.label} (line${hit.lines.length > 1 ? "s" : ""} ${hit.lines.join(", ")})`).join("; ");
const firstLine = (hits) => Math.min(...hits.flatMap((hit) => hit.lines));

// The publishable crate or package that holds `file`, or null: the nearest Cargo.toml and the nearest package.json at or above
// it, up to the repository root. A crate is publishable when its [package] does not say publish = false / []; a package when its
// package.json is not "private": true. A manifest that cannot be read counts as publishable.
function publishableOwner(file, repoRoot) {
  const root = path.resolve(repoRoot);
  const shown = (dir) => path.relative(root, dir).replaceAll("\\", "/") || ".";
  let cargoDecided = false;
  let npmDecided = false;
  for (let dir = path.dirname(path.resolve(file)); ; dir = path.dirname(dir)) {
    const outside = path.relative(root, dir);
    if (outside === ".." || outside.startsWith(`..${path.sep}`) || path.isAbsolute(outside)) return null;
    if (!cargoDecided && fs.existsSync(path.join(dir, "Cargo.toml"))) {
      cargoDecided = true;
      let state = null;
      try { state = cargoManifestState(fs.readFileSync(path.join(dir, "Cargo.toml"), "utf8")); } catch { /* unreadable: publishable */ }
      if (state === null || (state.hasPackage && !state.unpublished)) return `the publishable crate at ${shown(dir)} (its Cargo.toml [package] does not say publish = false)`;
    }
    if (!npmDecided && fs.existsSync(path.join(dir, "package.json"))) {
      npmDecided = true;
      let manifest = null;
      try { manifest = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8").replace(/^﻿/, "")); } catch { /* unreadable: publishable */ }
      if (manifest?.private !== true) return `the publishable package at ${shown(dir)} (its package.json is not "private": true)`;
    }
    if ((cargoDecided && npmDecided) || dir === root || path.dirname(dir) === dir) return null;
  }
}

function envTemplateViolations(relative, file, bytes, records, repoRoot) {
  const owner = publishableOwner(file, repoRoot);
  const encoding = notUtf8(bytes);
  const hits = encoding === null ? contentHits(bytes.toString("utf8"), TEMPLATE_ALL_SHAPES) : [];
  const refused = [];
  if (owner) refused.push(`${relative}: env template inside ${owner}; a publishable unit never carries one (refused by name, as at publish time and in the archive scan). Move it out of the unit, or mark the unit unpublished`);
  if (encoding) refused.push(`${relative}: env template ${encoding}, so it cannot be read as UTF-8 text; refused even when recorded. Save it as UTF-8`);
  if (hits.length) refused.push(`${relative}:${firstLine(hits)}: env template matches ${hitsText(hits)}; refused even when recorded. Replace the value with a placeholder`);
  if (refused.length) return refused;
  const sha256 = recordDigest(bytes);
  const record = records.get(relative);
  if (record?.kind === "env-template" && record.sha256 === sha256) return [];
  const line = recordLine(relative, sha256, "env-template");
  if (record?.kind === "env-template") return [`${relative}: env template changed since it was recorded (recorded sha256 ${record.sha256}, now ${sha256}); after reviewing the change, replace its record in ${SECRET_SCAN_RECORDS_FILE} with: ${line}`];
  return [`${relative}: env template (sha256 ${sha256}) is not recorded in ${SECRET_SCAN_RECORDS_FILE}; after reviewing that it holds only names and placeholders, add this record: ${line}`];
}

function fileViolations(relative, bytes, records) {
  const hits = contentHits(bytes.toString("utf8"), FILE_SHAPES);
  if (!hits.length) return [];
  const sha256 = recordDigest(bytes);
  const record = records.get(relative);
  if (record?.kind === "fixture" && record.sha256 === sha256) return [];
  const line = recordLine(relative, sha256, "fixture");
  const where = `${relative}:${firstLine(hits)}: content matches ${hitsText(hits)}`;
  if (record?.kind === "fixture") return [`${where}; its fixture record is out of date (recorded sha256 ${record.sha256}, now ${sha256}). After reviewing the change, replace its record in ${SECRET_SCAN_RECORDS_FILE} with: ${line}`];
  return [`${where}; sha256 ${sha256}. If this is a redaction or screening test vector and not a credential, review it and add this record to ${SECRET_SCAN_RECORDS_FILE}: ${line}`];
}

/**
 * Files under `crateDir` that must not be committed or packaged. Strict by default: bare relative paths. With `envTemplates: true`
 * (the repository gate) the reviewed records of `<repoRoot>/.rightkit/secret-scan-records.json` apply and each violation is a
 * `<repo-relative path>[:<line>]: <reason>` line; `repoRoot` (default `crateDir`) is the root those paths and the records are relative to.
 */
export function scanCrateForSecrets(crateDir, { envTemplates = false, repoRoot = crateDir, skip = null } = {}) {
  if (envTemplates) return scanRepositoryForSecrets(crateDir, repoRoot, skip);
  const violations = [];
  for (const file of walkFiles(crateDir, skip)) {
    const relative = path.relative(crateDir, file).replaceAll("\\", "/");
    if (FORBIDDEN_PACKAGE_PATH.test(relative)) {
      violations.push(relative);
      continue;
    }
    const text = fs.readFileSync(file).toString("utf8");
    if (SECRET_PATTERNS.some((pattern) => pattern.test(text))) violations.push(relative);
  }
  return [...new Set(violations)].sort();
}

function scanRepositoryForSecrets(dir, repoRoot, skip = null) {
  const { records = new Map(), problems = [] } = readSecretScanRecords(repoRoot);
  const violations = [...problems];
  for (const file of walkFiles(dir, skip)) {
    const inDir = path.relative(dir, file).replaceAll("\\", "/");
    const relative = path.relative(repoRoot, file).replaceAll("\\", "/");
    if (neverRecordable(inDir)) {
      violations.push(`${relative}: refused by name (an .env file, a key or certificate file, or build output); it is never committed and cannot be recorded`);
      continue;
    }
    // A file that vanished between the walk and the read was a tool's temporary file (pnpm writes `_tmp_<pid>_*` next to a
    // lockfile it reads), never committed content.
    let bytes;
    try { bytes = fs.readFileSync(file); } catch (e) { if (e.code === "ENOENT") continue; throw e; }
    violations.push(...(ENV_TEMPLATE.test(inDir) ? envTemplateViolations(relative, file, bytes, records, repoRoot) : fileViolations(relative, bytes, records)));
  }
  return [...new Set(violations)].sort();
}

/**
 * Package, qualify and (unless `options.dryRun`) publish one crate. A real publish is refused (HOST_UPLOAD_REFUSED) before anything
 * runs unless `options.capability` is a publish capability (host-upload-refusal.mjs createPublishCapability) created from a
 * validated admission that names this crate, and again before the registry upload unless the verified archive is the admitted one.
 */
export async function runCargoPublish(options, execute = (_command, args, settings) => runCargoSync(args, settings), deps = {}) {
  if (!options.dryRun) assertPublishCapability(options.capability, { what: "runCargoPublish", registry: "crates", name: options.crate });
  const crateDir = path.join(options.workspaceRoot, "crates", options.crate);
  const manifest = path.join(crateDir, "Cargo.toml");
  if (!fs.existsSync(manifest)) throw new Error(`crate manifest not found: ${manifest}`);
  // Parsed, not pattern-matched: `version.workspace = true` resolves through the workspace root, and a
  // manifest whose [package].name is not the requested crate is refused before anything else runs.
  const pkg = readCargoPackage(crateDir);
  if (pkg.name !== options.crate) throw new Error(`Cargo.toml [package].name ${JSON.stringify(pkg.name)} differs from --crate ${options.crate}`);
  const cargoVersion = pkg.version ?? undefined;
  const gateOptions = {
    kind: "cargo", name: options.crate, version: cargoVersion, packagePath: crateDir,
    evidencePath: path.resolve(options.evidence ?? defaultEvidencePath(options.workspaceRoot, "cargo", options.crate)),
  };
  const gate = (deps.enforceGate ?? enforcePublicationGate)(gateOptions, { dryRun: options.dryRun });
  const expectedSha256 = gate.evidence?.artifactSha256;
  // Explicit evidence reuse enters through the same gate: current commit, version, clean source, host runs
  // and artifact digest must match. Windows evidence keeps its actual win32 host policy on the Mac.
  const reuseEvidence = options.fromEvidence === true;
  if (reuseEvidence && (options.dryRun || options.allowDirty || !options.evidence)) throw new Error("--from-evidence requires a real publish, clean source and explicit evidence");
  const proven = (reuseEvidence || options.trustEvidenceSteps === true) && !options.dryRun ? provenGateSteps(gate.evidence) : {};
  if (reuseEvidence && (proven.tests !== true || proven.clippy !== true)) throw new Error("--from-evidence requires gate-proven tests and clippy for this crate and commit");
  if (reuseEvidence) {
    const status = (deps.workspaceStatus ?? ((workspaceRoot) => spawnSync("git", ["status", "--porcelain", "--", "Cargo.toml", "Cargo.lock", "ci/gate.config.json", "ci/asset-gated.json"], { cwd: workspaceRoot, encoding: "utf8", windowsHide: true })))(options.workspaceRoot);
    // The gate config decides the steps replayed below, so an uncommitted edit would change what is demanded (0.4.1 review A3).
    if (status.error || status.status !== 0 || String(status.stdout ?? "").trim()) throw new Error("--from-evidence requires clean workspace Cargo.toml, Cargo.lock and ci gate config (ci/gate.config.json, ci/asset-gated.json)");
    const proofHost = singleHostOfPolicy(gate.evidence) ?? "darwin";
    const runs = gate.evidence.runs.filter((run) => run?.host?.os === proofHost);
    const command = `cargo package --locked -p ${options.crate}`;
    if (!runs.length || !runs.every((run) => run.steps?.some((step) => step.ok === true && step.command === command))) {
      throw new Error("--from-evidence requires a verified cargo package --locked receipt for this artifact");
    }
    // Load the shared planner after module initialization: local-publish itself imports this publisher.
    // Validate the complete current qualification matrix, not merely one passing test and lint command.
    const { localTestPlan } = await import("./local-publish.mjs");
    let config;
    try { config = JSON.parse(fs.readFileSync(path.join(options.workspaceRoot, "ci", "gate.config.json"), "utf8")); }
    catch { throw new Error("--from-evidence cannot read current ci/gate.config.json"); }
    for (const run of runs) {
      if (!Array.isArray(run.steps) || run.steps.some((step) => step?.ok !== true || (step.failed ?? 0) !== 0)) {
        throw new Error("--from-evidence refuses an unsuccessful qualification step");
      }
      const expected = localTestPlan({ kind: "cargo", unit: options.crate, dir: `crates/${options.crate}`, config, batch: Boolean(run.batch), platform: proofHost });
      for (const step of expected.steps) {
        const text = `${step.command} ${step.args.join(" ")}`;
        if (!run.steps.some((row) => row.command === text && row.ok === true && (!step.test || (row.executed > 0 && row.failed === 0)))) {
          throw new Error(`--from-evidence missing configured qualification step: ${text}`);
        }
      }
      // A batch run's clippy lives in its batch receipt, which `expected` leaves out: demand one row per feature set (0.4.1 review A2).
      if (run.batch) {
        const missing = batchClippyMissingFeatureSets(run.batch, gate.evidence, config?.rust?.[options.crate]?.featureSets ?? [[]]);
        if (missing.length) throw new Error(`--from-evidence missing batch clippy for feature set${missing.length === 1 ? "" : "s"}: ${missing.map((features) => features.join(" ") || "default").join("; ")}`);
      }
    }
  }
  const plan = buildCargoPublishPlan({ ...options, crateDir, proven,
    verifyOnPublish: !reuseEvidence && options.verifyOnPublish === true, packageVerified: reuseEvidence, offlineReadOnly: options.offlineReadOnly === true });
  for (const [label, key] of Object.entries(PROVEN_STEP)) {
    if (proven[key] === true) process.stdout.write(`[right-release] cargo ${label}: proven by the gate evidence for this commit, not repeated\n`);
  }

  let targetDir;
  let artifact;
  let publication;
  for (const step of plan) {
    process.stdout.write(`[right-release] cargo ${step.label}\n`);
    if (step.internal === "secret-scan") {
      const violations = scanCrateForSecrets(crateDir);
      if (violations.length) throw new Error(`secret scan rejected: ${violations.join(", ")}`);
      continue;
    }
    if (step.internal === "runtime-budget") {
      const check = runtimeBudgetCheck({ crate: options.crate, crateDir, version: cargoVersion, evidence: gate.evidence ?? undefined });
      if (check) process.stdout.write(`${formatPreflight([check])}\n`);
      const deferral = check?.status === PREFLIGHT_FAIL ? runtimeBudgetDeferral(crateDir, options.crate) : null;
      if (deferral) {
        process.stdout.write(`[right-release] runtime budget DEFERRED for ${options.crate}: ${deferral}\n`);
        continue;
      }
      if (check?.status === PREFLIGHT_FAIL && !options.dryRun) {
        const error = new Error(`runtime budget refused: ${check.detail}. Fix: ${check.fix}`);
        error.code = RUNTIME_BUDGET_CODE;
        throw error;
      }
      continue;
    }
    if (step.internal === "verify-archive") {
      // Revalidate source/evidence after packaging; preserve that exact evidence object.
      const current = (deps.enforceGate ?? enforcePublicationGate)(gateOptions);
      if (current.evidence?.artifactSha256 !== expectedSha256) throw new Error("publication evidence changed during packaging");
      artifact = readVerifiedArtifact(path.join(targetDir, "package", `${options.crate}-${cargoVersion}.crate`),
        { sha256: current.evidence?.artifactSha256 });
      continue;
    }
    if (step.internal === "registry-readback") {
      publication = await verifyCargoRegistry({ name: options.crate, version: cargoVersion, sha256: artifact.sha256 }, deps);
      continue;
    }
    if (step.label === "registry publish") {
      assertArtifactUnchanged(artifact);
      assertPublishCapability(options.capability, { what: "runCargoPublish", registry: "crates", name: options.crate, version: cargoVersion, sha256: artifact.sha256 });
    }
    // `execute` may be synchronous (the default) or async (the batch publisher overlaps uploads).
    const isReadOnlyInspection = ["package contents", "package location"].includes(step.label);
    const settings = {
      cwd: options.workspaceRoot,
      encoding: "utf8",
      windowsHide: true,
      stdio: step.capture || (options.offlineReadOnly === true && isReadOnlyInspection) ? "pipe" : "inherit",
    };
    const result = options.offlineReadOnly === true && isReadOnlyInspection
      ? await runCargoReadOnly(step.args, settings, execute, (failed) => {
        process.stderr.write(`[right-release] offline Cargo inspection missed cache; retrying online once\n${failed.stdout ?? ""}${failed.stderr ? `\n${failed.stderr}` : ""}`);
      })
      : await execute(step.command, step.args, settings);
    if (result.error) {
      if (result.offlineFallbackFailed) throw new Error(`${step.label} failed: ${result.error.message}`);
      throw result.error;
    }
    if (result.status !== 0) {
      if (result.offlineFallbackFailed || step.label === "package archive") {
        if (result.stdout) process.stdout.write(result.stdout);
        if (result.stderr) process.stderr.write(result.stderr);
      } else if (step.capture && result.stderr) process.stderr.write(result.stderr);
      const fallback = result.offlineFallbackFailed ? `\n${result.stdout ?? ""}\n${result.stderr ?? ""}` : "";
      throw new Error(`${step.label} failed with exit code ${result.status}${fallback}`);
    }
    if (step.label === "package location") {
      if (result.stderr) process.stderr.write(result.stderr);
      const metadata = JSON.parse(result.stdout);
      if (typeof metadata.target_directory !== "string" || !path.isAbsolute(metadata.target_directory)) throw new Error("invalid Cargo package target directory");
      targetDir = metadata.target_directory;
    } else if (step.label === "package archive") {
      if (result.stdout) process.stdout.write(result.stdout);
      if (result.stderr) process.stderr.write(result.stderr);
    } else if (step.capture) {
      if (result.stderr) process.stderr.write(result.stderr);
      const files = String(result.stdout ?? "").split(/\r?\n/).filter(Boolean);
      const violations = packageFileViolations(files);
      if (violations.length) throw new Error(`package contents rejected: ${violations.join(", ")}`);
      process.stdout.write(`${files.join("\n")}\n`);
    }
  }
  return publication ?? { name: options.crate, version: cargoVersion, dryRun: true };
}

// `skip(absolutePath)`: a caller's exclusion (the gate's git-ignored paths of a live tree); a skipped directory is not descended.
function walkFiles(root, skip = null) {
  const files = [];
  const pending = [root];
  while (pending.length) {
    const current = pending.pop();
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      if (entry.isDirectory() && [".git", "target", "node_modules"].includes(entry.name)) continue;
      const absolute = path.join(current, entry.name);
      if (skip && skip(absolute)) continue;
      if (entry.isDirectory()) pending.push(absolute);
      else if (entry.isFile()) files.push(absolute);
    }
  }
  return files;
}

function valueAfter(argv, flag) {
  const index = argv.indexOf(flag);
  return index >= 0 ? argv[index + 1] : undefined;
}

async function main() {
  const options = parseCargoPublishArgs(process.argv.slice(2));
  // Every non---dry-run call (including --from-evidence) is an upload: refused on every host, with no environment override.
  if (!options.dryRun) refuseHostUpload("right-release publish cargo");
  const result = await runCargoPublish(options);
  process.stdout.write(`[right-release] ${options.dryRun ? "validated" : "published"} ${options.crate}\n`);
  if (!options.dryRun) {
    recordPublished({ registry: "cargo", name: result.name, version: result.version }, workspaceFeedOptions(options.workspaceRoot));
  }
}

const realOrSelf = (file) => { try { return fs.realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  main().catch((error) => {
    process.stderr.write(`right-release publish cargo: ${error.code ? `[${error.code}] ` : ""}${error.message}\n`);
    process.exitCode = 1;
  });
}
