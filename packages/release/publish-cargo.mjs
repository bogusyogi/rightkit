#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { enforcePublicationGate, defaultEvidencePath } from "./publication-gate.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { readVerifiedArtifact, assertArtifactUnchanged } from "./publication-artifacts.mjs";
import { verifyCargoRegistry } from "./cargo-registry-readback.mjs";
import { recordPublished } from "./rightkit-sync.mjs";

const FORBIDDEN_PACKAGE_PATH = /(^|\/)(?:\.env(?:\.[^/]*)?|target(?:\/.*)?|node_modules(?:\/.*)?|[^/]+\.(?:p8|p12|pem|key))$/i;
const SECRET_PATTERNS = [
  /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/,
  /\bAKIA[0-9A-Z]{16}\b/,
  /\bgh[pousr]_[A-Za-z0-9_]{20,}\b/,
  /\bnpm_[A-Za-z0-9]{30,}\b/,
];

export function parseCargoPublishArgs(argv, cwd = process.cwd()) {
  const crate = valueAfter(argv, "--crate");
  if (!crate) throw new Error("publish cargo requires --crate <name>");
  if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(crate)) throw new Error(`invalid crate name: ${crate}`);
  const allowed = new Set(["--crate", "--workspace", "--dry-run", "--allow-dirty", "--evidence"]);
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) continue;
    if (!allowed.has(arg)) throw new Error(`unknown publish cargo option: ${arg}`);
    if (arg !== "--dry-run" && arg !== "--allow-dirty") index += 1;
  }
  const dryRun = argv.includes("--dry-run");
  const allowDirty = argv.includes("--allow-dirty");
  if (allowDirty && !dryRun) throw new Error("--allow-dirty is only valid with --dry-run");
  return {
    crate,
    dryRun,
    allowDirty,
    workspaceRoot: path.resolve(valueAfter(argv, "--workspace") ?? cwd),
    evidence: valueAfter(argv, "--evidence"),
  };
}

export function buildCargoPublishPlan({ crate, crateDir, dryRun, allowDirty = false }) {
  const dryRunArgs = ["publish", "--dry-run", "-p", crate];
  const packageArgs = ["package", "--list", "-p", crate];
  if (allowDirty) dryRunArgs.push("--allow-dirty");
  if (allowDirty) packageArgs.push("--allow-dirty");
  const plan = [
    { label: "secret scan", internal: "secret-scan", crateDir },
    { label: "format", command: "cargo", args: ["fmt", "-p", crate, "--check"] },
    { label: "tests", command: "cargo", args: ["test", "-p", crate, "--", "--test-threads=1"] },
    { label: "clippy", command: "cargo", args: ["clippy", "-p", crate, "--all-targets", "--", "-D", "warnings"] },
    { label: "package contents", command: "cargo", args: packageArgs, capture: true },
    { label: "registry dry-run", command: "cargo", args: dryRunArgs },
  ];
  if (!dryRun) plan.push(
    // `publish --dry-run` leaves its archive under target/package/tmp-crate on current Cargo; package
    // explicitly to bind target/package/<name>-<version>.crate to evidence before Cargo uploads.
    { label: "package archive", command: "cargo", args: ["package", "--no-verify", "-p", crate, ...(allowDirty ? ["--allow-dirty"] : [])] },
    { label: "package location", command: "cargo", args: ["metadata", "--format-version", "1", "--no-deps"], capture: true },
    { label: "archive hash check", internal: "verify-archive" },
    { label: "registry publish", command: "cargo", args: ["publish", "--no-verify", "-p", crate] },
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

export function scanCrateForSecrets(crateDir) {
  const violations = [];
  for (const file of walkFiles(crateDir)) {
    const relative = path.relative(crateDir, file).replaceAll("\\", "/");
    if (FORBIDDEN_PACKAGE_PATH.test(relative)) {
      violations.push(relative);
      continue;
    }
    const bytes = fs.readFileSync(file);
    if (bytes.includes(0)) continue;
    const text = bytes.toString("utf8");
    if (SECRET_PATTERNS.some((pattern) => pattern.test(text))) violations.push(relative);
  }
  return [...new Set(violations)].sort();
}

export async function runCargoPublish(options, execute = (_command, args, settings) => runCargoSync(args, settings), deps = {}) {
  const crateDir = path.join(options.workspaceRoot, "crates", options.crate);
  const manifest = path.join(crateDir, "Cargo.toml");
  if (!fs.existsSync(manifest)) throw new Error(`crate manifest not found: ${manifest}`);
  const cargoVersion = fs.readFileSync(manifest, "utf8").match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const gateOptions = {
    kind: "cargo", name: options.crate, version: cargoVersion, packagePath: crateDir,
    evidencePath: path.resolve(options.evidence ?? defaultEvidencePath(options.workspaceRoot, "cargo", options.crate)),
  };
  const gate = (deps.enforceGate ?? enforcePublicationGate)(gateOptions, { dryRun: options.dryRun });
  const expectedSha256 = gate.evidence?.artifactSha256;
  const plan = buildCargoPublishPlan({ ...options, crateDir });

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
    if (step.label === "registry publish") assertArtifactUnchanged(artifact);
    const result = execute(step.command, step.args, {
      cwd: options.workspaceRoot,
      encoding: "utf8",
      windowsHide: true,
      stdio: step.capture ? "pipe" : "inherit",
    });
    if (result.error) throw result.error;
    if (result.status !== 0) {
      if (step.capture && result.stderr) process.stderr.write(result.stderr);
      throw new Error(`${step.label} failed with exit code ${result.status}`);
    }
    if (step.label === "package location") {
      const metadata = JSON.parse(result.stdout);
      if (typeof metadata.target_directory !== "string" || !path.isAbsolute(metadata.target_directory)) throw new Error("invalid Cargo package target directory");
      targetDir = metadata.target_directory;
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

function walkFiles(root) {
  const files = [];
  const pending = [root];
  while (pending.length) {
    const current = pending.pop();
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      if (entry.isDirectory() && [".git", "target", "node_modules"].includes(entry.name)) continue;
      const absolute = path.join(current, entry.name);
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
  const result = await runCargoPublish(options);
  process.stdout.write(`[right-release] ${options.dryRun ? "validated" : "published"} ${options.crate}\n`);
  if (!options.dryRun) {
    recordPublished({ registry: "cargo", name: result.name, version: result.version });
  }
}

const realOrSelf = (file) => { try { return fs.realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  main().catch((error) => {
    process.stderr.write(`right-release publish cargo: ${error.code ? `[${error.code}] ` : ""}${error.message}\n`);
    process.exitCode = 1;
  });
}
