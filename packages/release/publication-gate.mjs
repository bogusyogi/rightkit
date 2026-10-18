#!/usr/bin/env node
// Publication gate: refuse to publish a shared npm package, crate, or Swift tag unless a gate
// evidence file names the exact current commit SHA (clean tree for the package path) and carries
// passing host run receipts from that same SHA: macOS and Windows, or macOS only for Swift.
// Format: docs/RIGHTKIT-SHARED-PACKAGES.md, "Publication gate" -> "Evidence file format".
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { recordPublished } from "./rightkit-sync.mjs";
import { fileURLToPath } from "node:url";
import { digestBytes, readVerifiedArtifact } from "./publication-artifacts.mjs";

export const GATE_SCHEMA = 1;
export const MAX_CARGO_JOBS = 64;
export const REQUIRED_HOSTS = Object.freeze(["darwin", "win32"]);
// Swift packages (RightKitSwift) are Apple-only; Windows cannot run them.
export const requiredHosts = (kind) => (kind === "swift" ? Object.freeze(["darwin"]) : REQUIRED_HOSTS);
const SHA = /^[0-9a-f]{40}$/;
// The one definition of a recorded darwin-only host policy (release evidence and the runtime budget gate share it).
export const isDarwinOnlyPolicy = (source) => source?.hostPolicy === "darwin-only"
  && typeof source.hostPolicyReason === "string" && source.hostPolicyReason.trim() !== "";
// The symmetric recorded win32-only host policy: a crate published from the Windows host under the local-build grant.
export const isWin32OnlyPolicy = (source) => source?.hostPolicy === "win32-only"
  && typeof source.hostPolicyReason === "string" && source.hostPolicyReason.trim() !== "";
/** The one host a recorded single-host policy names ("darwin" or "win32"), or null when none is recorded. */
export const singleHostOfPolicy = (source) => (isDarwinOnlyPolicy(source) ? "darwin" : isWin32OnlyPolicy(source) ? "win32" : null);

export function defaultEvidencePath(workspaceRoot, kind, name) {
  return path.join(workspaceRoot, "release-evidence", "gate", `${kind}-${name.replace(/^@/, "").replace("/", "-")}.json`);
}

function defaultGit(args, cwd) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (result.error || result.status !== 0) return null;
  return (result.stdout ?? "").trim();
}

// Explains a receipt commit that is not the publishing HEAD. The gate still refuses it (exact-commit rule,
// docs/RIGHTKIT-SHARED-PACKAGES.md "Publication gate" rule 1); this only says whether the receipt is an
// ancestor with an unchanged unit path (re-qualify by re-running on HEAD) or unrelated/changed.
function describeCommitMismatch(evidenceCommit, head, packagePath, git) {
  const known = git(["cat-file", "-t", evidenceCommit], packagePath);
  if (known !== "commit") return "receipt commit is not a commit object in this repository";
  // `merge-base --is-ancestor` prints nothing: "" means ancestor, null (non-zero exit) means not.
  const ancestor = git(["merge-base", "--is-ancestor", evidenceCommit, head], packagePath) !== null;
  const relation = ancestor ? git(["rev-list", "--count", `${evidenceCommit}..${head}`], packagePath) : null;
  const changed = git(["diff", "--name-only", evidenceCommit, head, "--", "."], packagePath);
  const parts = [];
  parts.push(ancestor ? `receipt commit is an ancestor, ${relation ?? "?"} commit(s) behind HEAD` : "receipt commit is not an ancestor of HEAD");
  if (changed === null) parts.push("cannot diff the unit path");
  else parts.push(changed === "" ? "unit path unchanged since the receipt (re-run the host runs on HEAD to re-qualify)" : `unit path changed since the receipt: ${changed.split("\n").slice(0, 5).join(", ")}`);
  return parts.join("; ");
}

const text = (value) => typeof value === "string" && value.trim() !== "";
const escapeRegExp = (value) => String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

// A batch publication (local-publish.mjs --crates) runs fmt, the secret scan and clippy ONCE for the whole
// batch at one commit and tree, then copies that receipt into every crate's evidence run as `run.batch`.
// The receipt proves, for THIS crate, the same three checks a per-crate run proves; it is bound to the evidence
// commit and to the HEAD tree (the git tree of that commit), so it cannot be carried to another commit.
export const BATCH_CHECKS = Object.freeze(["fmt", "secret-scan", "clippy"]);
const BATCH_COMMAND = Object.freeze({
  fmt: /\bfmt\b.*--check\b/,
  "secret-scan": /\S/,
  clippy: /\bclippy\b.*--all-targets\b.*-D warnings\s*$/,
});

/** Problems with a batch receipt for the crate `evidence.name` (empty when it proves fmt, secret scan and clippy).
 * `tree` is the current HEAD tree when known; `undefined` skips only that comparison. */
export function batchReceiptProblems(batch, evidence, tree) {
  if (!batch || typeof batch !== "object" || Array.isArray(batch)) return ["batch receipt must be an object"];
  const problems = [];
  if (!text(batch.id)) problems.push("batch.id");
  if (batch.commit !== evidence.commit) problems.push(`batch.commit (must equal ${evidence.commit})`);
  if (!SHA.test(String(batch.tree ?? ""))) problems.push("batch.tree (must be a 40-hex git tree)");
  else if (tree !== undefined && batch.tree !== tree) problems.push(`batch.tree (must equal the HEAD tree ${tree})`);
  if (!(Array.isArray(batch.crates) && batch.crates.includes(evidence.name))) problems.push("batch.crates (must name this crate)");
  const checks = Array.isArray(batch.checks) ? batch.checks : [];
  for (const name of BATCH_CHECKS) {
    const rows = checks.filter((row) => row?.name === name);
    if (!rows.length) { problems.push(`batch check ${name} missing`); continue; }
    for (const row of rows) {
      if (row.ok !== true) problems.push(`batch check ${name} not ok`);
      if (row.commit !== evidence.commit || row.tree !== batch.tree) problems.push(`batch check ${name} not bound to the batch commit and tree`);
      if (!(text(row.command) && BATCH_COMMAND[name].test(row.command))) problems.push(`batch check ${name} command`);
    }
    // fmt is workspace-wide; clippy and the secret scan name the crates they covered.
    if (name !== "fmt" && !rows.some((row) => Array.isArray(row.crates) && row.crates.includes(evidence.name))) problems.push(`batch check ${name} does not cover this crate`);
  }
  return problems;
}

/**
 * Which per-crate publication steps the evidence ALREADY proves for this exact commit, so the publisher may
 * skip repeating them: `tests` (a passing `cargo test -p <name>` step that executed tests), `clippy` (a passing
 * `cargo clippy -p <name> ... -D warnings` step, or a valid batch receipt covering the crate), and `fmt` and
 * `secretScan` (a valid batch receipt covering the crate). Every run on the proving host (darwin, or win32 under a win32-only policy) at the evidence commit must prove a
 * step for it to count. Call after the gate accepted the evidence (the gate binds commit, tree and artifact).
 */
export function provenGateSteps(evidence) {
  const none = { tests: false, clippy: false, fmt: false, secretScan: false };
  if (!evidence || !SHA.test(String(evidence.commit ?? "")) || !text(evidence.name)) return none;
  // The proving host is the one a recorded win32-only policy names; every other evidence is proved on darwin.
  const proofHost = isWin32OnlyPolicy(evidence) ? "win32" : "darwin";
  const runs = (Array.isArray(evidence.runs) ? evidence.runs : []).filter((run) => run?.host?.os === proofHost);
  if (!runs.length || runs.some((run) => run.commit !== evidence.commit)) return none;
  const name = escapeRegExp(evidence.name);
  const stepProves = (run, pattern, needTests) => (Array.isArray(run.steps) ? run.steps : []).some((step) => step?.ok === true
    && pattern.test(String(step.command ?? "")) && (!needTests || (step.executed > 0 && step.failed === 0)));
  const batchOk = (run) => run.batch !== undefined && batchReceiptProblems(run.batch, evidence, undefined).length === 0;
  return {
    tests: runs.every((run) => stepProves(run, new RegExp(`^cargo test -p ${name}( |$)`), true)),
    clippy: runs.every((run) => batchOk(run) || stepProves(run, new RegExp(`^cargo clippy -p ${name} .*-- -D warnings$`), false)),
    fmt: runs.every(batchOk),
    secretScan: runs.every(batchOk),
  };
}

// Names of the fields of one host run that do not qualify (empty when the run qualifies). Every run binds to
// the same unit, commit and artifact: commit, host arch, feature set and artifactSha256 equal to the evidence.
function invalidRunFields(run, evidence, tree) {
  const bad = [];
  if (run.commit !== evidence.commit) bad.push(`commit (must equal ${evidence.commit})`);
  if (!text(run.host?.arch)) bad.push("host.arch");
  if (!(Array.isArray(run.features) && run.features.length > 0 && run.features.every(text))) bad.push("features");
  if (run.artifactSha256 !== evidence.artifactSha256) bad.push("artifactSha256 (must equal the evidence hash)");
  if (!text(run.command)) bad.push("command");
  if (!text(run.operator)) bad.push("operator");
  if (!text(run.timestamp)) bad.push("timestamp");
  if (!(Number.isInteger(run.passed) && run.passed > 0)) bad.push("passed (must be > 0)");
  if (run.failed !== 0) bad.push("failed (must be 0)");
  if (run.jobs !== undefined && !(Number.isInteger(run.jobs) && run.jobs >= 1 && run.jobs <= MAX_CARGO_JOBS)) bad.push(`jobs (must be an integer from 1 to ${MAX_CARGO_JOBS})`);
  if (run.batch !== undefined) bad.push(...batchReceiptProblems(run.batch, evidence, tree ?? null).map((problem) => `batch: ${problem}`));
  return bad;
}

// Returns { ok, missing[] }. Pure apart from the injected git/readFile.
export function checkPublicationGate({ kind, name, version, packagePath, evidencePath, artifactBytes, git = defaultGit, readFile = (p) => fs.readFileSync(p, "utf8") }) {
  const missing = [];
  const head = git(["rev-parse", "HEAD"], packagePath);
  if (!head || !SHA.test(head)) missing.push(`cannot read current commit SHA for ${packagePath}`);
  const dirty = git(["status", "--porcelain", "--", "."], packagePath);
  if (dirty === null) missing.push(`cannot read git status for ${packagePath}`);
  else if (dirty !== "") missing.push(`tree is not clean for ${packagePath}`);
  let evidence = null;
  try { evidence = JSON.parse(readFile(evidencePath)); } catch {
    missing.push(`gate evidence file missing or unreadable: ${evidencePath}`);
  }
  if (evidence) {
    if (evidence.schema !== GATE_SCHEMA) missing.push(`evidence schema must be ${GATE_SCHEMA}`);
    if (evidence.kind !== kind || evidence.name !== name) missing.push(`evidence is for ${evidence.kind}:${evidence.name}, expected ${kind}:${name}`);
    // A missing publishing version must refuse: a skipped comparison would qualify any evidence version.
    if (typeof version !== "string" || version.trim() === "") missing.push("publishing version is unknown (manifest has no literal version), cannot bind the receipt to it");
    else if (evidence.version !== version) missing.push(`evidence version ${evidence.version} != ${version}`);
    if (!SHA.test(String(evidence.commit ?? ""))) missing.push("evidence commit is not a full 40-character SHA");
    else if (head && evidence.commit !== head) {
      missing.push(`evidence commit ${evidence.commit} != current HEAD ${head} (${describeCommitMismatch(evidence.commit, head, packagePath, git)})`);
    }
    const runs = Array.isArray(evidence.runs) ? evidence.runs : [];
    // A batch receipt is bound to the HEAD tree as well as the commit; read it only when a run carries one.
    // `tree ?? null` below makes an unreadable tree fail the comparison (null never equals a 40-hex tree).
    const tree = runs.some((run) => run?.batch !== undefined) ? git(["rev-parse", "HEAD^{tree}"], packagePath) : undefined;
    if (tree === null) missing.push(`cannot read the HEAD tree for ${packagePath}`);
    // npm packages and crates may ship on macOS evidence first when Adrian decides so; the decision must be
    // recorded in the evidence (hostPolicy "darwin-only" + a non-empty hostPolicyReason).
    // Crates may ship Mac-first too (Adrian 2026-10-07): the Windows run follows; the reason is recorded.
    // A crate published from the Windows host under the local-build grant records the symmetric
    // hostPolicy "win32-only" + reason, and then the one required receipt is the Windows run.
    const singleHost = (kind === "npm" || kind === "cargo") ? singleHostOfPolicy(evidence) : null;
    for (const host of singleHost ? [singleHost] : requiredHosts(kind)) {
      const label = host === "darwin" ? "macOS" : "Windows";
      const matching = runs.filter((run) => run?.host?.os === host);
      if (!matching.length) { missing.push(`no ${label} host run receipt`); continue; }
      // Every run on the host must qualify: a failing run is not masked by a passing run beside it.
      matching.forEach((run, index) => {
        const fields = invalidRunFields(run, evidence, tree);
        if (fields.length) missing.push(`${label} run receipt ${index} invalid: ${fields.join(", ")}`);
      });
    }
    if (!/^[0-9a-f]{64}$/.test(String(evidence.artifactSha256 ?? ""))) missing.push("evidence artifactSha256 must be a 64-hex hash");
  }
  if (artifactBytes !== undefined && evidence && digestBytes(artifactBytes) !== evidence.artifactSha256) missing.push("artifact sha256 does not match evidence artifactSha256");
  const result = { ok: missing.length === 0, missing };
  Object.defineProperty(result, "evidence", { value: evidence });
  return Object.freeze(result);
}

// Enforce: throws on missing evidence unless dryRun (then reports and continues).
export function enforcePublicationGate(options, { dryRun = false, write = (line) => process.stdout.write(line) } = {}) {
  const result = checkPublicationGate(options);
  if (result.ok) { write(`[right-release] publication gate OK for ${options.kind}:${options.name}\n`); return result; }
  const report = `publication gate not satisfied for ${options.kind}:${options.name}:\n  - ${result.missing.join("\n  - ")}`;
  if (dryRun) { write(`[right-release] ${report}\n[right-release] (dry-run: a real publish would be refused)\n`); return result; }
  throw new Error(report);
}

// Keychain item holding a granular npm token scoped to @rightkit with 2FA bypass (created by Adrian).
export const NPM_KEYCHAIN_SERVICE = "npm-rightkit-publish";

// `right-release publish npm --package <name> [--evidence <file>] [--dry-run]` runs the gate only.
// Add `--publish --tarball <file>` to publish that exact gated tarball: its sha256 must equal the
// evidence artifactSha256. Mandatory registry readback compares against captured bytes,
// so a source-path swap cannot return success or record publication. The token is read from the macOS Keychain inside this process (never
// printed or passed on a command line), and the registry shasum is verified after publication.
/** `right-release publish npm --check-auth`: prove the Keychain token authenticates (whoami +
 * @rightkit package access) without publishing. Prints only the username and counts. */
export function checkNpmAuth(deps = {}) {
  const run = deps.spawnSync ?? spawnSync;
  const token = deps.readToken ? deps.readToken() : readKeychainToken(run);
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-npm-auth-"));
  const userconfig = path.join(dir, "npmrc");
  const redact = (text) => String(text ?? "").split(token).join("<redacted>");
  try {
    fs.writeFileSync(userconfig, "//registry.npmjs.org/:_authToken=${RIGHTKIT_NPM_TOKEN}\n", { mode: 0o600 });
    const env = { ...process.env, RIGHTKIT_NPM_TOKEN: token };
    const base = ["--registry", "https://registry.npmjs.org/", "--userconfig", userconfig];
    const who = run("npm", ["whoami", ...base], { encoding: "utf8", env, windowsHide: true });
    if (who.status !== 0) throw new Error(`npm whoami failed with the Keychain token:\n${redact(`${who.stdout ?? ""}${who.stderr ?? ""}`).trim()}`);
    const access = run("npm", ["access", "list", "packages", "@rightkit", "--json", ...base], { encoding: "utf8", env, windowsHide: true });
    let packages = {};
    try { packages = JSON.parse(access.stdout || "{}"); } catch { packages = {}; }
    const writable = Object.values(packages).filter((level) => level === "read-write").length;
    process.stdout.write(`[right-release] npm token OK: ${String(who.stdout).trim()}, read-write on ${writable} @rightkit packages\n`);
    return { user: String(who.stdout).trim(), writable };
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

export function runNpmPreflight(argv, workspaceRoot = process.cwd(), deps = {}) {
  if (argv.includes("--check-auth")) { checkNpmAuth(deps); return; }
  let pkg; let evidence; let tarball; const dryRun = argv.includes("--dry-run"); const publish = argv.includes("--publish");
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--package") pkg = argv[++i];
    else if (argv[i] === "--evidence") evidence = argv[++i];
    else if (argv[i] === "--tarball") tarball = path.resolve(argv[++i]);
    else if (argv[i] === "--workspace") workspaceRoot = path.resolve(argv[++i]);
    else if (argv[i] !== "--dry-run" && argv[i] !== "--publish") throw new Error(`unknown publish npm option: ${argv[i]}`);
  }
  if (!pkg || !/^@rightkit\/[a-z0-9-]+$/.test(pkg)) throw new Error("publish npm requires --package @rightkit/<name>");
  const packagePath = path.join(workspaceRoot, "packages", pkg.slice("@rightkit/".length));
  const manifest = JSON.parse(fs.readFileSync(path.join(packagePath, "package.json"), "utf8"));
  const evidencePath = path.resolve(evidence ?? defaultEvidencePath(workspaceRoot, "npm", pkg));
  const options = { kind: "npm", name: pkg, version: manifest.version, packagePath, evidencePath };
  if (!publish) {
    enforcePublicationGate(options, { dryRun });
    process.stdout.write("[right-release] npm preflight only; add --publish --tarball <file> to publish\n");
    return;
  }
  if (dryRun) throw new Error("--publish and --dry-run are mutually exclusive");
  if (!tarball) throw new Error("--publish requires --tarball <gated .tgz>");
  enforcePublicationGate(options);
  publishGatedTarball({ name: pkg, version: manifest.version, tarball, evidencePath }, deps);
}

export function publishGatedTarball({ name, version, tarball, evidencePath }, deps = {}) {
  const run = deps.spawnSync ?? spawnSync;
  const bytes = fs.readFileSync(tarball);
  const evidence = JSON.parse(fs.readFileSync(evidencePath, "utf8"));
  const sha256 = crypto.createHash("sha256").update(bytes).digest("hex");
  if (sha256 !== evidence.artifactSha256) throw new Error(`tarball sha256 ${sha256} != evidence artifactSha256 ${evidence.artifactSha256}`);
  const token = deps.readToken ? deps.readToken() : readKeychainToken(run);
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-npm-publish-"));
  const userconfig = path.join(dir, "npmrc");
  const redact = (text) => String(text ?? "").split(token).join("<redacted>");
  try {
    fs.writeFileSync(userconfig, "//registry.npmjs.org/:_authToken=${RIGHTKIT_NPM_TOKEN}\n", { mode: 0o600 });
    const result = run("npm", ["publish", tarball, "--access", "public", "--registry", "https://registry.npmjs.org/", "--userconfig", userconfig],
      { encoding: "utf8", env: { ...process.env, RIGHTKIT_NPM_TOKEN: token }, windowsHide: true });
    const out = redact(`${result.stdout ?? ""}${result.stderr ?? ""}`);
    if (result.error || result.status !== 0) throw new Error(`npm publish failed for ${name}@${version}:\n${out.trim()}`);
    process.stdout.write(`[right-release] submitted ${name}@${version}\n`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
  const sha1 = crypto.createHash("sha1").update(bytes).digest("hex");
  verifyNpmRegistryBytes({ name, version, sha1 }, deps);
  (deps.recordPublished ?? recordPublished)({ registry: "npm", name, version });
}

// CI and the authenticated helper share mandatory, fail-closed registry equality.
export function prepareNpmPublication({ name, version, tarball, evidencePath }) {
  if (!name || !version) throw new Error("npm publication identity is required");
  const evidence = JSON.parse(fs.readFileSync(evidencePath, "utf8"));
  const artifact = readVerifiedArtifact(tarball, { sha256: evidence.artifactSha256 });
  return { name, version, sha1: crypto.createHash("sha1").update(artifact.bytes).digest("hex"), sha256: artifact.sha256 };
}

export function verifyNpmRegistryBytes({ name, version, sha1 }, deps = {}) {
  if (!name || !version || !/^[a-f0-9]{40}$/.test(sha1 ?? "")) throw new Error("captured npm publication hash evidence is required");
  const run = deps.spawnSync ?? spawnSync;
  const deadline = Date.now() + (deps.verifyTimeoutMs ?? 10 * 60 * 1000);
  for (;;) {
    const view = run("npm", ["view", `${name}@${version}`, "dist.shasum", "--prefer-online", "--registry", "https://registry.npmjs.org/"], { encoding: "utf8", windowsHide: true });
    const registry = (view.stdout ?? "").trim();
    if (!view.error && view.status === 0 && registry === sha1) { process.stdout.write(`[right-release] registry bytes verified for ${name}@${version} (shasum ${sha1})\n`); return; }
    if (registry && registry !== sha1) throw new Error(`registry shasum ${registry} != gated tarball ${sha1} for ${name}@${version}; inspect remote package ${name}@${version}`);
    if (Date.now() > deadline) throw new Error(`${name}@${version} not visible on the registry after publish; verify later with npm view`);
    (deps.sleep ?? ((ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms)))(15000);
  }
}

function readKeychainToken(run) {
  if (process.platform !== "darwin") throw new Error(`npm publish token lookup is implemented for the macOS Keychain (${NPM_KEYCHAIN_SERVICE}) only`);
  const result = run("security", ["find-generic-password", "-s", NPM_KEYCHAIN_SERVICE, "-w"], { encoding: "utf8", windowsHide: true });
  const token = (result.stdout ?? "").trim();
  if (result.status !== 0 || !token) throw new Error(`Keychain item ${NPM_KEYCHAIN_SERVICE} not found; create it with: security add-generic-password -a "$USER" -s ${NPM_KEYCHAIN_SERVICE} -w`);
  return token;
}

const realOrSelf = (file) => { try { return fs.realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try { runNpmPreflight(process.argv.slice(2)); } catch (error) {
    process.stderr.write(`right-release publish npm: ${error.message}\n`); process.exitCode = 1;
  }
}
