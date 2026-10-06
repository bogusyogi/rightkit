#!/usr/bin/env node
// Publication gate: refuse to publish a shared npm package, crate, or Swift tag unless a gate
// evidence file names the exact current commit SHA (clean tree for the package path) and carries
// passing macOS and Windows host run receipts from that same SHA.
// Format: docs/RIGHTKIT-SHARED-PACKAGES.md, "Publication gate" -> "Evidence file format".
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";

export const GATE_SCHEMA = 1;
export const REQUIRED_HOSTS = Object.freeze(["darwin", "win32"]);
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
    for (const host of REQUIRED_HOSTS) {
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

// `right-release publish npm --package <name> [--evidence <file>] [--dry-run]`: preflight only.
export function runNpmPreflight(argv, workspaceRoot = process.cwd()) {
  let pkg; let evidence; const dryRun = argv.includes("--dry-run");
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--package") pkg = argv[++i];
    else if (argv[i] === "--evidence") evidence = argv[++i];
    else if (argv[i] === "--workspace") workspaceRoot = path.resolve(argv[++i]);
    else if (argv[i] !== "--dry-run") throw new Error(`unknown publish npm option: ${argv[i]}`);
  }
  if (!pkg || !/^@rightkit\/[a-z0-9-]+$/.test(pkg)) throw new Error("publish npm requires --package @rightkit/<name>");
  const packagePath = path.join(workspaceRoot, "packages", pkg.slice("@rightkit/".length));
  const manifest = JSON.parse(fs.readFileSync(path.join(packagePath, "package.json"), "utf8"));
  const options = { kind: "npm", name: pkg, version: manifest.version, packagePath, evidencePath: path.resolve(evidence ?? defaultEvidencePath(workspaceRoot, "npm", pkg)) };
  enforcePublicationGate(options, { dryRun });
  process.stdout.write("[right-release] npm preflight only: publish stays on the authenticated Windows primary checkout (pnpm publish)\n");
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url))) {
  try { runNpmPreflight(process.argv.slice(2)); } catch (error) {
    process.stderr.write(`right-release publish npm: ${error.message}\n`); process.exitCode = 1;
  }
}
