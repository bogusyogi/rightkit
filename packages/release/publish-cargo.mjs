#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
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

// A committed env template (`.env.example`, `.env.test.example`, `.env.sample`, `.env.template`) holds key names for
// people to copy. The repository gate (`envTemplates: true`) reads it like any other file, for credentials, instead of
// refusing it by name; a crate package and its archive still refuse it by name, since nothing published needs it.
const ENV_TEMPLATE = /(^|\/)\.env(?:\.[^/]*)?\.(?:example|sample|template)$/i;

export function scanCrateForSecrets(crateDir, { envTemplates = false } = {}) {
  const violations = [];
  for (const file of walkFiles(crateDir)) {
    const relative = path.relative(crateDir, file).replaceAll("\\", "/");
    if (FORBIDDEN_PACKAGE_PATH.test(relative) && !(envTemplates && ENV_TEMPLATE.test(relative))) {
      violations.push(relative);
      continue;
    }
    const bytes = fs.readFileSync(file);
    const text = bytes.toString("utf8");
    if (SECRET_PATTERNS.some((pattern) => pattern.test(text))) violations.push(relative);
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
