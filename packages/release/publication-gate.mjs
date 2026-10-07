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
import { fileURLToPath } from "node:url";

export const GATE_SCHEMA = 1;
export const REQUIRED_HOSTS = Object.freeze(["darwin", "win32"]);
// Swift packages (RightKitSwift) are Apple-only; Windows cannot run them.
export const requiredHosts = (kind) => (kind === "swift" ? Object.freeze(["darwin"]) : REQUIRED_HOSTS);
const SHA = /^[0-9a-f]{40}$/;

export function defaultEvidencePath(workspaceRoot, kind, name) {
  return path.join(workspaceRoot, "release-evidence", "gate", `${kind}-${name.replace(/^@/, "").replace("/", "-")}.json`);
}

function defaultGit(args, cwd) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (result.error || result.status !== 0) return null;
  return (result.stdout ?? "").trim();
}

// Returns { ok, missing[] }. Pure apart from the injected git/readFile.
export function checkPublicationGate({ kind, name, version, packagePath, evidencePath, git = defaultGit, readFile = (p) => fs.readFileSync(p, "utf8") }) {
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
    if (version && evidence.version !== version) missing.push(`evidence version ${evidence.version} != ${version}`);
    if (!SHA.test(String(evidence.commit ?? ""))) missing.push("evidence commit is not a full 40-character SHA");
    else if (head && evidence.commit !== head) missing.push(`evidence commit ${evidence.commit} != current HEAD ${head}`);
    const runs = Array.isArray(evidence.runs) ? evidence.runs : [];
    // npm UI packages may ship on macOS evidence first when Adrian decides so; the decision must be
    // recorded in the evidence (hostPolicy "darwin-only" + a non-empty hostPolicyReason).
    const darwinOnly = kind === "npm" && evidence.hostPolicy === "darwin-only"
      && typeof evidence.hostPolicyReason === "string" && evidence.hostPolicyReason.trim() !== "";
    for (const host of darwinOnly ? ["darwin"] : requiredHosts(kind)) {
      const label = host === "darwin" ? "macOS" : "Windows";
      const matching = runs.filter((run) => run?.host?.os === host);
      if (!matching.length) { missing.push(`no ${label} host run receipt`); continue; }
      const good = matching.some((run) => run.commit === evidence.commit
        && Number.isInteger(run.passed) && run.passed > 0 && run.failed === 0
        && typeof run.command === "string" && run.command.trim()
        && typeof run.operator === "string" && run.operator.trim()
        && typeof run.timestamp === "string" && run.timestamp.trim());
      if (!good) missing.push(`${label} run receipt must name commit ${evidence.commit}, command, operator, timestamp, passed>0 and failed=0`);
    }
    if (!/^[0-9a-f]{64}$/.test(String(evidence.artifactSha256 ?? ""))) missing.push("evidence artifactSha256 must be a 64-hex hash");
  }
  return Object.freeze({ ok: missing.length === 0, missing });
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
// evidence artifactSha256, the token is read from the macOS Keychain inside this process (never
// printed or passed on a command line), and the registry shasum is verified after publication.
export function runNpmPreflight(argv, workspaceRoot = process.cwd(), deps = {}) {
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
    process.stdout.write(`[right-release] published ${name}@${version}\n`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
  const sha1 = crypto.createHash("sha1").update(bytes).digest("hex");
  const deadline = Date.now() + (deps.verifyTimeoutMs ?? 10 * 60 * 1000);
  for (;;) {
    const view = run("npm", ["view", `${name}@${version}`, "dist.shasum", "--prefer-online", "--registry", "https://registry.npmjs.org/"], { encoding: "utf8", windowsHide: true });
    const registry = (view.stdout ?? "").trim();
    if (registry === sha1) { process.stdout.write(`[right-release] registry bytes verified for ${name}@${version} (shasum ${sha1})\n`); return; }
    if (registry && registry !== sha1) throw new Error(`registry shasum ${registry} != gated tarball ${sha1} for ${name}@${version}`);
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

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url))) {
  try { runNpmPreflight(process.argv.slice(2)); } catch (error) {
    process.stderr.write(`right-release publish npm: ${error.message}\n`); process.exitCode = 1;
  }
}
