#!/usr/bin/env node
// Publish RightKitSwift to a Swift package registry, gated exactly like `publish cargo`:
// secret scan -> swift test -> release build -> deterministic source archive + hash ->
// registry dry-run -> publish. Canonical source stays under tools/rightkit/swift; the
// registry package is an output only. No Git distribution repo, no path/Git dependency.
//
// swift-tools is macOS/Linux only, so the plan builder, arg parser, and version check are
// pure and unit-tested on any OS (publish-swift.test.mjs); the swift steps run where swift exists.
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { scanCrateForSecrets } from "./publish-cargo.mjs";

const IDENTIFIER = /^[a-z0-9]+(?:[-_][a-z0-9]+)*$/i;

export function parseSwiftPublishArgs(argv, env = process.env) {
  const allowed = new Set(["--scope", "--url", "--dry-run", "--allow-dirty"]);
  let scope = env.RIGHTKIT_SWIFT_REGISTRY_SCOPE ?? "rightsuite";
  let url = env.RIGHTKIT_SWIFT_REGISTRY_URL ?? "";
  let dryRun = false;
  let allowDirty = false;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!allowed.has(arg)) throw new Error(`unexpected argument: ${arg}`);
    if (arg === "--dry-run") dryRun = true;
    else if (arg === "--allow-dirty") allowDirty = true;
    else {
      const value = argv[(index += 1)];
      if (value === undefined) throw new Error(`${arg} needs a value`);
      if (arg === "--scope") scope = value;
      if (arg === "--url") url = value;
    }
  }
  if (!IDENTIFIER.test(scope)) throw new Error(`invalid registry scope: ${scope}`);
  if (allowDirty && !dryRun) throw new Error("--allow-dirty is only valid with --dry-run");
  if (!dryRun && !url) throw new Error("registry publish needs --url or RIGHTKIT_SWIFT_REGISTRY_URL");
  return { scope, url, dryRun, allowDirty };
}

// The VERSION file is the single source of truth; Package.swift must name the package and the
// version must be a clean semver so the registry archive is reproducible.
export function readPackageIdentity(packageDir) {
  const manifest = path.join(packageDir, "Package.swift");
  if (!fs.existsSync(manifest)) throw new Error(`Package.swift not found: ${manifest}`);
  const versionFile = path.join(packageDir, "VERSION");
  if (!fs.existsSync(versionFile)) throw new Error(`VERSION not found: ${versionFile}`);
  const version = fs.readFileSync(versionFile, "utf8").trim();
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error(`VERSION is not clean semver: "${version}"`);
  }
  const nameMatch = fs.readFileSync(manifest, "utf8").match(/name:\s*"([^"]+)"/);
  if (!nameMatch) throw new Error("could not read package name from Package.swift");
  return { name: nameMatch[1], version };
}

export function buildSwiftPublishPlan({ packageDir, scope, name, version, url, dryRun, allowDirty = false }) {
  const packageId = `${scope}.${name}`;
  const archive = path.join(packageDir, ".build", `${name}-${version}.zip`);
  const plan = [
    { label: "secret scan", internal: "secret-scan" },
    { label: "tests", command: "swift", args: ["test", "--package-path", packageDir] },
    { label: "release build", command: "swift", args: ["build", "-c", "release", "--package-path", packageDir] },
    // Deterministic source archive; its bytes are what the registry stores, so hash it for the log.
    { label: "source archive", command: "swift", args: ["package", "--package-path", packageDir, "archive-source", "-o", archive] },
    { label: "archive hash", internal: "archive-hash", archive },
  ];
  if (dryRun) {
    plan.push({ label: "registry dry-run (validate only)", internal: "dry-run-note", packageId, version });
    return plan;
  }
  plan.push({
    label: "registry publish",
    command: "swift",
    args: ["package-registry", "publish", packageId, version, "--url", url, "--package-path", packageDir],
  });
  return plan;
}

export function runSwiftPublish(options, execute = spawnSync) {
  const packageDir = options.packageDir;
  const { name, version } = readPackageIdentity(packageDir);
  const plan = buildSwiftPublishPlan({ ...options, packageDir, name, version });

  for (const step of plan) {
    process.stdout.write(`[right-release] swift ${step.label}\n`);
    if (step.internal === "secret-scan") {
      const violations = scanCrateForSecrets(packageDir);
      if (violations.length) throw new Error(`secret scan rejected: ${violations.join(", ")}`);
      continue;
    }
    if (step.internal === "dry-run-note") {
      process.stdout.write(`[right-release] dry-run OK: ${step.packageId} ${step.version} (not published)\n`);
      continue;
    }
    if (step.internal === "archive-hash") {
      const bytes = fs.readFileSync(step.archive);
      process.stdout.write(`sha256 ${createHash("sha256").update(bytes).digest("hex")}  ${path.basename(step.archive)}\n`);
      continue;
    }
    const result = execute(step.command, step.args, { cwd: packageDir, encoding: "utf8", windowsHide: true, stdio: "inherit" });
    if (result.error) {
      if (result.error.code === "ENOENT") {
        throw new Error(`${step.command} not found — run publish swift on macOS/Linux with the Swift toolchain installed`);
      }
      throw result.error;
    }
    if (result.status !== 0) throw new Error(`${step.label} failed with exit code ${result.status}`);
  }
  return { name, version, published: !options.dryRun };
}

if (process.argv[1]?.endsWith("publish-swift.mjs")) {
  try {
    const options = parseSwiftPublishArgs(process.argv.slice(2));
    const packageDir = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/(\w:)/, "$1")), "../../swift/RightKitSwift");
    runSwiftPublish({ ...options, packageDir });
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}
